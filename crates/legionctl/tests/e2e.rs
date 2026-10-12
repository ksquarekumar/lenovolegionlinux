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
    // an empty machine: no /etc/legionctl unless a test writes one
    let root = home.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let operator = Operator::start(
        tag,
        &[
            ("LEGIONCTL_SYSROOT", root.into()),
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
    assert_eq!(skills.len(), 3, "{skills:?}");
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

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

#[test]
fn config_layers_merge_in_order_and_etc_locks_win() {
    let (op, home) = operator("config");
    // the site locks a key and a reminder; the user tries to change both; a repository adds its own rules
    write(
        &home.join("root/etc/legionctl/10-site.jsonc"),
        r#"{ "config": { "locked": ["load", "reminders.ste100"] }, "worktree": { "root": "/srv/worktrees" } }"#,
    );
    write(
        &home.join(".config/legionctl/mine.jsonc"),
        r#"{
          // tries a locked key and the layer order: both ignored and reported
          "load": { "busy": { "cpu_pressure": 1 } },
          "config": { "precedence": ["defaults", "user"] },
          "reminders": { "load-aware": { "enabled": true } }
        }"#,
    );
    let repo = home.join("repo");
    std::fs::create_dir_all(repo.join(".git/objects")).unwrap();
    write(
        &repo.join(".legionctl/rules.jsonc"),
        r#"{
          "reminders": { "repo-rule": { "text": "Use the make targets.", "event": "prompt", "enabled": true } },
          "hooks": { "bindings": { "prompt+": ["load"] } }
        }"#,
    );
    let sub = repo.join("crates/a");
    std::fs::create_dir_all(&sub).unwrap();
    let cli = |cwd: &Path, args: &[&str]| {
        let out = op.cli_in(cwd, args);
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned()
                + &String::from_utf8_lossy(&out.stderr),
        )
    };
    let json = |cwd: &Path, args: &[&str]| -> Value {
        let (ok, text) = cli(cwd, &[args, &["--json"]].concat());
        assert!(ok, "{args:?}: {text}");
        serde_json::from_str(&text).unwrap()
    };

    // the repository layer applies anywhere inside the repository, and only there
    let (_, inside) = cli(&sub, &["reminders", "emit", "--event", "prompt"]);
    assert!(inside.contains("Use the make targets."), "{inside}");
    let (_, outside) = cli(&home, &["reminders", "emit", "--event", "prompt"]);
    assert!(!outside.contains("make targets"), "{outside}");
    assert_eq!(
        json(&repo, &["config", "explain", "hooks.bindings.prompt"])["value"],
        json!(["reminders", "load-if-busy", "load"]),
        "prompt+ appends to the defaults"
    );

    // /etc/legionctl wins: the locked key keeps its default, the layer order stays, and both are reported
    let explained = json(&repo, &["config", "explain", "load.busy.cpu_pressure"]);
    assert_eq!(explained["value"], 20.0);
    assert_eq!(explained["locked"], true);
    let layers = json(&repo, &["config", "layers"]);
    let names: Vec<&str> = layers["layers"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["defaults", "system", "user", "repo"]);
    let problems = layers["problems"].to_string();
    assert!(
        problems.contains("mine.jsonc: load is set only by"),
        "{problems}"
    );
    assert!(
        problems.contains("mine.jsonc: config is set only by"),
        "{problems}"
    );
    assert_eq!(
        json(&repo, &["config", "explain", "worktree.root"])["value"],
        "/srv/worktrees"
    );

    // a change through legionctl goes to managed.jsonc, which merges after the user's own files
    let (ok, text) = cli(&repo, &["reminders", "off", "load-aware"]);
    assert!(ok, "{text}");
    let reminders = json(&repo, &["reminders", "list"]);
    let load_aware = reminders
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["id"] == "load-aware")
        .unwrap();
    assert_eq!(
        load_aware["enabled"], false,
        "managed.jsonc wins over mine.jsonc"
    );
    assert!(home.join(".config/legionctl/managed.jsonc").exists());

    // a locked key cannot be changed through legionctl either
    let (ok, text) = cli(&repo, &["reminders", "off", "ste100"]);
    assert!(!ok && text.contains("locked by /etc/legionctl"), "{text}");
}

#[test]
fn dossier_gate_and_stop_hook_hold_the_agent_until_the_session_has_a_dossier() {
    let (op, home) = operator("dossier");
    let out = |o: std::process::Output| String::from_utf8_lossy(&o.stdout).trim().to_string();
    let hook =
        |event: &str, input: Value| out(op.run(&home, &[], &["hook", event], &input.to_string()));
    let as_session = |session: &str, args: &[&str], stdin: &str| {
        let o = op.run(&home, &[("LEGIONCTL_SESSION", session)], args, stdin);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        out(o)
    };

    // no dossier: the prompt is blocked with the way forward; the command that binds one passes
    let blocked: Value = serde_json::from_str(&hook(
        "prompt",
        json!({"session_id": "s1", "cwd": home, "prompt": "fix the bug"}),
    ))
    .unwrap();
    assert_eq!(blocked["decision"], "block");
    assert!(
        blocked["reason"]
            .as_str()
            .unwrap()
            .contains("/legionctl:dossier use <name> --create"),
        "{blocked}"
    );
    let passing = hook(
        "prompt",
        json!({"session_id": "s1", "cwd": home, "prompt": "/legionctl:dossier use port --create"}),
    );
    assert!(passing.starts_with("Reminder:"), "{passing}");

    // the agent hears it first: at session start, and before the reply ends (once, so the agent cannot loop)
    let start = hook("session-start", json!({"session_id": "s1", "cwd": home}));
    assert!(
        start.starts_with("legionctl: this session has no dossier yet. Set it up first"),
        "{start}"
    );
    let stop: Value =
        serde_json::from_str(&hook("stop", json!({"session_id": "s1", "cwd": home}))).unwrap();
    assert_eq!(stop["decision"], "block");
    assert!(
        stop["reason"].as_str().unwrap().contains("dossier_use"),
        "{stop}"
    );
    assert_eq!(
        hook(
            "stop",
            json!({"session_id": "s1", "cwd": home, "stop_hook_active": true})
        ),
        ""
    );

    // bind, add a numbered artifact from stdin, and the gate opens
    let bound = as_session("s1", &["dossier", "use", "port", "--create"], "");
    assert!(bound.contains("active dossier: port"), "{bound}");
    assert_eq!(
        as_session("s1", &["dossier", "add", "MEMORY", "intake"], "# Intake\n"),
        "port/MEMORY/01-intake.md"
    );
    assert_eq!(
        hook("stop", json!({"session_id": "s1", "cwd": home})),
        "",
        "a set-up session ends its reply"
    );
    let open = hook(
        "prompt",
        json!({"session_id": "s1", "cwd": home, "prompt": "fix the bug"}),
    );
    assert!(!open.contains("\"decision\""), "{open}");
    assert_eq!(
        as_session("s1", &["dossier", "check"], ""),
        "port/plan.md has every ledger section"
    );

    // the next session where s1 worked is bound at session start, so nobody binds again by hand
    hook("session-end", json!({"session_id": "s1"}));
    let next = hook(
        "session-start",
        json!({"session_id": "s3", "cwd": home, "source": "resume"}),
    );
    assert!(
        next.starts_with("legionctl dossier: port (bound at session start"),
        "{next}"
    );
    let prompt = hook(
        "prompt",
        json!({"session_id": "s3", "cwd": home, "prompt": "go on"}),
    );
    assert!(!prompt.contains("\"decision\""), "{prompt}");
    // a session elsewhere is not bound to it
    let other = home.join("other");
    std::fs::create_dir_all(&other).unwrap();
    let elsewhere = hook("session-start", json!({"session_id": "s4", "cwd": other}));
    assert!(
        elsewhere.starts_with("legionctl: this session has no dossier yet"),
        "{elsewhere}"
    );
}

#[test]
fn dossier_write_guard_and_takeover_work_through_the_claude_code_hooks() {
    let (op, home) = operator("guard");
    let out = |o: std::process::Output| String::from_utf8_lossy(&o.stdout).trim().to_string();
    let hook =
        |event: &str, input: Value| out(op.run(&home, &[], &["hook", event], &input.to_string()));
    let as_session = |session: &str, args: &[&str], stdin: &str| {
        let o = op.run(&home, &[("LEGIONCTL_SESSION", session)], args, stdin);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        out(o)
    };

    as_session("s1", &["dossier", "use", "port", "--create"], "");
    as_session("s1", &["dossier", "add", "MEMORY", "intake"], "# Intake\n");

    // a call without an absolute path (an outdated client) is refused, not stored with a relative place
    let refused = op.run(
        &home,
        &[],
        &["hook", "session-start"],
        &json!({"session_id": "s5"}).to_string(),
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("is not absolute"),
        "{refused:?}"
    );

    // the write guard: only the holder writes into the dossier; other paths are not legionctl's business
    let plan = home.join(".agentworkspace/port/plan.md");
    let denied: Value = serde_json::from_str(&hook(
        "pre-tool",
        json!({"session_id": "s2", "tool_name": "Write", "tool_input": {"file_path": plan, "content": "x"}}),
    ))
    .unwrap();
    assert_eq!(denied["hookSpecificOutput"]["permissionDecision"], "deny");
    assert!(
        denied["hookSpecificOutput"]["permissionDecisionReason"]
            .as_str()
            .unwrap()
            .contains("active in session s1")
    );
    assert_eq!(
        hook(
            "pre-tool",
            json!({"session_id": "s1", "tool_name": "Edit", "tool_input": {"file_path": plan}})
        ),
        ""
    );
    assert_eq!(
        hook(
            "pre-tool",
            json!({"session_id": "s2", "tool_name": "Write", "tool_input": {"file_path": home.join("x.rs")}})
        ),
        ""
    );

    // a second session cannot take the dossier while s1 holds it; after s1 ends, it can
    let refused = op.run(
        &home,
        &[("LEGIONCTL_SESSION", "s2")],
        &["dossier", "use", "port"],
        "",
    );
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("held by session s1"));
    hook("session-end", json!({"session_id": "s1"}));
    assert!(
        as_session("s2", &["dossier", "use", "port"], "")
            .contains("newest memory:\n  MEMORY/01-intake.md  Intake")
    );
}
