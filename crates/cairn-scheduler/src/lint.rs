use crate::task::{TaskDef, Trigger};
use std::collections::{BTreeMap, BTreeSet};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Error)]
pub enum LintError {
    #[error("task `{0}`: `after` dependency cycle: {1}")]
    Cycle(String, String),
    #[error("task `{0}`: depends on unknown task `{1}`")]
    UnknownDependency(String, String),
    #[error("task `{0}`: `uses: {1}` does not resolve to a loaded plugin or builtin")]
    UnknownPlugin(String, String),
    #[error("task `{0}`: model `{1}` is not on the gateway allowlist")]
    ModelNotAllowed(String, String),
    #[error("task `{0}`: AI tasks need `models` and `budget`")]
    MissingAiConfig(String),
    #[error("task `{0}`: invalid cron `{1}`: {2}")]
    BadCron(String, String, String),
    #[error("worst-case daily spend ${0:.2} across enabled AI tasks exceeds gateway cap ${1:.2}")]
    OverDailyCap(f64, f64),
}

/// Facts the linter needs from the rest of the system.
pub struct LintContext<'a> {
    pub ai_capabilities: &'a BTreeSet<String>,
    pub builtins: &'a BTreeSet<String>,
    pub allowed_models: &'a BTreeSet<String>,
    pub gateway_daily_cap_usd: f64,
}

/// Lint the full set of task definitions. Called whenever any task item is created or patched;
/// tasks that fail are saved with `enabled: false` and `lint_errors` populated.
pub fn lint(tasks: &[TaskDef], ctx: &LintContext<'_>) -> Vec<LintError> {
    let mut errs = Vec::new();
    let names: BTreeSet<&str> = tasks.iter().map(|t| t.name.as_str()).collect();

    for t in tasks {
        let (kind, cap) = t.uses.split_once('/').unwrap_or(("", ""));
        let known = match kind {
            "ai" => ctx.ai_capabilities.contains(cap),
            "builtin" => ctx.builtins.contains(cap),
            _ => false,
        };
        if !known {
            errs.push(LintError::UnknownPlugin(t.name.clone(), t.uses.clone()));
        }
        if t.is_ai() {
            if t.models.is_empty() || t.budget.is_none() {
                errs.push(LintError::MissingAiConfig(t.name.clone()));
            }
            for m in &t.models {
                if !ctx.allowed_models.contains(m) {
                    errs.push(LintError::ModelNotAllowed(t.name.clone(), m.clone()));
                }
            }
        }
        match &t.trigger {
            Trigger::Cron(expr) => {
                if let Err(e) = crate::parse_cron(expr) {
                    errs.push(LintError::BadCron(t.name.clone(), expr.clone(), e.to_string()));
                }
            }
            Trigger::After(deps) => {
                for d in deps {
                    if !names.contains(d.as_str()) {
                        errs.push(LintError::UnknownDependency(t.name.clone(), d.clone()));
                    }
                }
            }
            Trigger::On { .. } => {}
        }
    }

    errs.extend(find_cycles(tasks));

    // Worst-case daily spend: sum of per-day budgets of enabled AI tasks.
    let worst: f64 =
        tasks.iter().filter(|t| t.enabled && t.is_ai()).filter_map(|t| t.budget.map(|b| b.per_day_usd)).sum();
    if worst > ctx.gateway_daily_cap_usd {
        errs.push(LintError::OverDailyCap(worst, ctx.gateway_daily_cap_usd));
    }
    errs
}

fn find_cycles(tasks: &[TaskDef]) -> Vec<LintError> {
    let deps: BTreeMap<&str, Vec<&str>> = tasks
        .iter()
        .map(|t| {
            let d = match &t.trigger {
                Trigger::After(v) => v.iter().map(String::as_str).collect(),
                _ => vec![],
            };
            (t.name.as_str(), d)
        })
        .collect();

    #[derive(Clone, Copy, PartialEq)]
    enum Mark {
        Temp,
        Done,
    }
    fn visit<'a>(
        n: &'a str,
        deps: &BTreeMap<&'a str, Vec<&'a str>>,
        marks: &mut BTreeMap<&'a str, Mark>,
        stack: &mut Vec<&'a str>,
        out: &mut Vec<LintError>,
    ) {
        match marks.get(n) {
            Some(Mark::Done) => return,
            Some(Mark::Temp) => {
                let start = stack.iter().position(|s| *s == n).unwrap_or(0);
                let mut cycle: Vec<&str> = stack[start..].to_vec();
                cycle.push(n);
                out.push(LintError::Cycle(n.to_string(), cycle.join(" -> ")));
                return;
            }
            None => {}
        }
        marks.insert(n, Mark::Temp);
        stack.push(n);
        for d in deps.get(n).into_iter().flatten() {
            if deps.contains_key(d) {
                visit(d, deps, marks, stack, out);
            }
        }
        stack.pop();
        marks.insert(n, Mark::Done);
    }

    let mut marks = BTreeMap::new();
    let mut out = Vec::new();
    for n in deps.keys() {
        visit(n, &deps, &mut marks, &mut Vec::new(), &mut out);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task::TaskDef;
    use serde_json::json;

    fn task(v: serde_json::Value) -> TaskDef {
        serde_json::from_value(v).unwrap()
    }

    fn ctx_sets() -> (BTreeSet<String>, BTreeSet<String>, BTreeSet<String>) {
        (
            ["digest".to_string(), "triage".to_string()].into(),
            ["gc".to_string(), "notify".to_string()].into(),
            ["anthropic/claude-haiku-4.5".to_string()].into(),
        )
    }

    #[test]
    fn catches_cycles_unknowns_and_budget() {
        let (ai, b, m) = ctx_sets();
        let ctx = LintContext { ai_capabilities: &ai, builtins: &b, allowed_models: &m, gateway_daily_cap_usd: 0.10 };
        let tasks = vec![
            task(json!({"name":"a","uses":"ai/digest","trigger":{"after":["b"]},
                "models":["anthropic/claude-haiku-4.5"],"budget":{"per_run_usd":0.05,"per_day_usd":0.08}})),
            task(json!({"name":"b","uses":"ai/triage","trigger":{"after":["a"]},
                "models":["openai/some-big-model"],"budget":{"per_run_usd":0.05,"per_day_usd":0.08}})),
            task(json!({"name":"c","uses":"builtin/nope","trigger":{"cron":"not cron"}})),
        ];
        let errs = lint(&tasks, &ctx);
        assert!(errs.iter().any(|e| matches!(e, LintError::Cycle(..))));
        assert!(errs.iter().any(|e| matches!(e, LintError::ModelNotAllowed(..))));
        assert!(errs.iter().any(|e| matches!(e, LintError::UnknownPlugin(..))));
        assert!(errs.iter().any(|e| matches!(e, LintError::BadCron(..))));
        assert!(errs.iter().any(|e| matches!(e, LintError::OverDailyCap(..))));
    }

    #[test]
    fn clean_graph_passes() {
        let (ai, b, m) = ctx_sets();
        let ctx = LintContext { ai_capabilities: &ai, builtins: &b, allowed_models: &m, gateway_daily_cap_usd: 0.50 };
        let tasks = vec![
            task(json!({"name":"gc","uses":"builtin/gc","trigger":{"cron":"30 3 * * *"}})),
            task(json!({"name":"digest","uses":"ai/digest","trigger":{"cron":"0 21 * * *"},
                "models":["anthropic/claude-haiku-4.5"],"budget":{"per_run_usd":0.05,"per_day_usd":0.10}})),
        ];
        assert!(lint(&tasks, &ctx).is_empty());
    }
}
