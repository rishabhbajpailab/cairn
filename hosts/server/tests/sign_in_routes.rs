//! `deploy/sign-in.cfg` lets exactly `cairn_app::api::MACHINE_ROUTES` past the GitHub
//! sign-in (ADR 0015): no more, no less, and each one must exist in the router.

use cairn_app::api::MACHINE_ROUTES;

/// The bypass list as written in sign-in.cfg: `"METHOD=^regex$"` lines.
fn skip_routes() -> Vec<String> {
    let cfg = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../deploy/sign-in.cfg"))
        .expect("deploy/sign-in.cfg");
    let start = cfg.find("skip_auth_routes = [").expect("skip_auth_routes in sign-in.cfg");
    // The list closes with `]` on a line of its own; the regexes themselves contain `]`.
    let end = start + cfg[start..].find("\n]").expect("closing ] on its own line");
    cfg[start..end]
        .lines()
        .skip(1)
        .filter_map(|l| l.trim().strip_prefix('"').and_then(|l| l.split('"').next()))
        .map(str::to_owned)
        .collect()
}

/// A route path as the anchored regex sign-in.cfg uses: path parameters match one segment
/// of safe characters only, so `..`, `/` and encoded tricks can't ride along.
fn as_skip_route((method, path): (&str, &str)) -> String {
    let mut re = String::from("^");
    for (i, seg) in path.split('/').enumerate() {
        if i > 0 {
            re.push('/');
        }
        if seg.starts_with('{') && seg.ends_with('}') {
            re.push_str("[A-Za-z0-9_-]+");
        } else {
            re.push_str(seg);
        }
    }
    format!("{method}={re}$")
}

#[test]
fn sign_in_bypasses_exactly_the_machine_routes() {
    let mut want: Vec<String> = MACHINE_ROUTES.iter().copied().map(as_skip_route).collect();
    let mut have = skip_routes();
    want.sort();
    have.sort();
    assert_eq!(have, want, "deploy/sign-in.cfg skip_auth_routes must equal cairn_app::api::MACHINE_ROUTES");
}

#[test]
fn every_machine_route_exists_in_the_router() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/cairn-app/src/api.rs"))
        .expect("api.rs");
    for (method, path) in MACHINE_ROUTES {
        let line = src
            .lines()
            .find(|l| l.contains(&format!(".route(\"{path}\"")))
            .unwrap_or_else(|| panic!("{path} is in MACHINE_ROUTES but not routed in api.rs"));
        assert!(line.contains(&format!("{}(", method.to_lowercase())), "{method} {path} not routed: {line}");
    }
}
