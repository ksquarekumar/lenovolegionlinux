//! End-to-end: the binary as the agent plugin uses it (hook dispatch, MCP over stdio), in an isolated HOME.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use serde_json::{Value, json};

fn home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("legionctl-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn legion(home: &Path) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_legionctl"));
    cmd.env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", home)
        .env(
            "LEGIONCTL_PLUGIN_SKILLS",
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/agent-plugin/skills"),
        );
    cmd
}

fn run(home: &Path, args: &[&str]) -> String {
    let out = legion(home).args(args).output().unwrap();
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

#[test]
fn prompt_hook_injects_skill_reminders_and_user_actions_as_configured() {
    let h = home("hooks");
    let fresh = run(&h, &["hooks", "run", "--event", "prompt"]);
    assert!(
        fresh.contains("legionctl:ste100-style"),
        "fresh install: {fresh}"
    );
    assert!(
        fresh.contains("legionctl:glanceable-style"),
        "fresh install: {fresh}"
    );

    run(&h, &["reminders", "off", "ste100"]);
    run(&h, &["hooks", "add", "branch", "echo on-branch-main"]);
    run(&h, &["hooks", "bind", "prompt", "branch"]);
    let changed = run(&h, &["hooks", "run", "--event", "prompt"]);
    assert!(
        !changed.contains("ste100"),
        "reminder turned off: {changed}"
    );
    assert!(changed.contains("glanceable-style"));
    assert!(
        changed.ends_with("on-branch-main\n"),
        "user action runs after built-ins: {changed}"
    );

    let start = run(&h, &["hooks", "run", "--event", "session-start"]);
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
    fn start(home: &Path) -> Mcp {
        let mut child = legion(home)
            .arg("mcp")
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
fn mcp_server_exposes_every_tool_and_reads_the_plugin_skills() {
    let h = home("mcp");
    let mut mcp = Mcp::start(&h);

    let tools = mcp.call("tools/list", &json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for expected in [
        "system_status",
        "system_load",
        "doctor",
        "agent_configs",
        "worktree_list",
        "worktree_create",
        "worktree_remove",
        "request_action",
        "setup_check",
        "setup_apply",
        "reminders_list",
        "reminders_update",
        "hooks_list",
        "hooks_update",
        "skills_list",
    ] {
        assert!(
            names.contains(&expected),
            "missing tool {expected}; have {names:?}"
        );
    }

    let result = mcp.call(
        "tools/call",
        &json!({"name": "skills_list", "arguments": {}}),
    );
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
}
