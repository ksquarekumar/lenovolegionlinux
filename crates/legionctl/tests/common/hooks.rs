//! Claude Code hook payloads for the tests that drive `legionctl hook` (e2e.rs, threads.rs).

use std::path::Path;

use serde_json::Value;

/// A hook payload as Claude Code sends it (recorded 2026-10-13, anonymized in tests/fixtures/claude-code-hooks), with
/// the test's own fields set over it. A `pre-tool` payload is the one recorded for its `tool_name`.
pub fn claude_hook(event: &str, fields: &Value) -> String {
    let name = match fields["tool_name"].as_str() {
        Some(tool) => format!("{event}-{}", tool.to_lowercase()),
        None => event.to_string(),
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(format!(
        "../../tests/fixtures/claude-code-hooks/{name}.json"
    ));
    let mut payload: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (key, value) in fields.as_object().unwrap() {
        payload[key] = value.clone();
    }
    payload.to_string()
}
