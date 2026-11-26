//! Rough CPU cost of building the plugin registry in a fresh process (a stand-in for a fresh
//! Worker isolate; wasm is typically slower than native). Run in release:
//!   cargo run --release -p cairn-plugins-embedded --example coldstart
//! Modes: `embedded` (build-time validators, what the Worker uses) or `runtime [plugins dir]`.
//! For numbers closer to a Worker, run it as wasm under Node (V8, like workerd) with
//! `make coldstart`; see docs/deploy.md § CPU limits.

use cairn_plugins::{FsSource, Registry};
use std::time::Instant;

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "embedded".into());
    let root = std::env::args()
        .nth(2)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../plugins"));
    let load = || match mode.as_str() {
        "runtime" => Registry::load_from(&FsSource(root.clone())),
        _ => Registry::load_from(&cairn_plugins_embedded::Embedded),
    };
    let t = Instant::now();
    let first = load().expect("load");
    let a = t.elapsed();
    let t = Instant::now();
    let _second = load().expect("load");
    let b = t.elapsed();
    println!("{mode}: first load {a:?}, warm reload {b:?}, runtime-compiled {:?}", first.runtime_compiled());
}
