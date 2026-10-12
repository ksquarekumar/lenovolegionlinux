//! End-to-end: the binary as the agent plugin uses it (hook dispatch, MCP over stdio), in an isolated HOME.

mod common;

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use common::Operator;
use serde_json::{Value, json};

/// An operator with an empty HOME and the repo's plugin skills.
fn operator(tag: &str) -> (Operator, PathBuf) {
    let home = std::env::temp_dir().join(format!("legionctl-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let skills =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/agent-plugin/skills");
    let operator = Operator::start(
        tag,
        &[
            ("HOME", home.clone().into()),
            ("PATH", std::env::var_os("PATH").unwrap()),
            ("LEGIONCTL_PLUGIN_SKILLS", OsString::from(skills)),
        ],
    );
    (operator, home)
}

#[test]
fn prompt_hook_injects_skill_reminders_and_user_actions_as_configured() {
    let (op, _home) = operator("hooks");
    let run = |args: &[&str]| op.ok(args);
    let fresh = run(&["hooks", "run", "--event", "prompt"]);
    assert!(
        fresh.contains("legionctl:ste100-style"),
        "fresh install: {fresh}"
    );
    assert!(
        fresh.contains("legionctl:glanceable-style"),
        "fresh install: {fresh}"
    );

    run(&["reminders", "off", "ste100"]);
    run(&["hooks", "add", "branch", "echo on-branch-main"]);
    run(&["hooks", "bind", "--event", "prompt", "branch"]);
    let changed = run(&["hooks", "run", "--event", "prompt"]);
    assert!(
        !changed.contains("ste100"),
        "reminder turned off: {changed}"
    );
    assert!(changed.contains("glanceable-style"));
    assert!(
        changed.ends_with("on-branch-main\n"),
        "user action runs after built-ins: {changed}"
    );

    let start = run(&["hooks", "run", "--event", "session-start"]);
    assert!(
        !start.contains("on-branch-main"),
        "bound to prompt only: {start}"
    );
}

struct Mcp {
    child: std::process::Child,
    out: BufReader<std::process::ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(operator: &Operator) -> Mcp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_legionctl"))
            .arg("mcp")
            .env_clear()
            .env("XDG_RUNTIME_DIR", &operator.runtime)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let out = BufReader::new(child.stdout.take().unwrap());
        let mut mcp = Mcp {
            child,
            out,
            next: 0,
        };
        mcp.call(
            "initialize",
            &json!({"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "e2e", "version": "0"}}),
        );
        mcp.send(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
        mcp
    }

    fn send(&mut self, msg: &Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{msg}").unwrap();
        stdin.flush().unwrap();
    }

    fn call(&mut self, method: &str, params: &Value) -> Value {
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}));
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], self.next, "{reply}");
        reply["result"].clone()
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

#[test]
fn mcp_server_has_one_tool_per_operation_and_calls_the_operator() {
    let (op, _home) = operator("mcp");
    let mut mcp = Mcp::start(&op);

    let tools = mcp.call("tools/list", &json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    let expected: Vec<String> = legionctl_api::OPS
        .iter()
        .map(|o| o.name.replace(['.', '-'], "_"))
        .collect();
    assert_eq!(names, expected, "the registry is the tool list");

    let result = mcp.call("tools/call", &json!({"name": "skills", "arguments": {}}));
    assert_ne!(result["isError"], true, "{result}");
    let skills: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    let skills = skills.as_array().unwrap();
    assert_eq!(skills.len(), 2, "{skills:?}");
    for s in skills {
        assert!(
            !s["description"].as_str().unwrap().is_empty(),
            "SKILL.md front matter parsed: {s}"
        );
        assert_eq!(
            s["reminder_on"], true,
            "each plugin skill has a reminder, on by default: {s}"
        );
        assert!(
            s["claude"].is_null(),
            "no plugin registry in a fresh HOME: {s}"
        );
    }

    // an agent's relative path is its own working directory, not the operator's
    let result = mcp.call(
        "tools/call",
        &json!({"name": "worktree_create", "arguments": {"name": "x", "repo": "no-such-repo"}}),
    );
    assert_eq!(result["isError"], true, "{result}");
    let message = result["content"][0]["text"].as_str().unwrap();
    assert!(
        message.contains(&format!(
            "{}/no-such-repo",
            std::env::current_dir().unwrap().display()
        )),
        "{message}"
    );
}
