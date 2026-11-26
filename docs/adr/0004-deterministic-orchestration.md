# 0004: Deterministic orchestration: you define tasks, the scheduler runs them

- **Status:** accepted
- **Date:** 2026-11-23

## Context
When an LLM orchestrates fan-out, costs and behaviour become unpredictable.

## Decision
Tasks declare their trigger, input query, model list, fan-out, budget and apply mode. The scheduler is plain code with no LLM in it. Runs can't create tasks or runs. Model choice is limited to the task's own list (`cheapest` or `first-available`).

## Consequences
- What runs and what it costs can be predicted and linted before anything executes.
- Agent-style behaviour (multi-step reasoning) has to be written out as an explicit `after:` chain.
