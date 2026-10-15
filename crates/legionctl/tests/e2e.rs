//! End-to-end: the binary as the agent plugin uses it (hook dispatch, MCP over stdio), in an isolated HOME.

mod common;
#[path = "common/provider.rs"]
mod provider;

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::Operator;
use serde_json::{Value, json};

/// A hook payload as Claude Code sends it (recorded 2026-10-13, anonymized in tests/fixtures/claude-code-hooks), with
/// the test's own fields set over it. A `pre-tool` payload is the one recorded for its `tool_name`.
fn claude_hook(event: &str, fields: &Value) -> String {
    let name = match fields["tool_name"].as_str() {
        Some(tool) => format!("{event}-{}", tool.to_lowercase()),
        None => event.to_string(),
    };
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(format!("tests/fixtures/claude-code-hooks/{name}.json"));
    let mut payload: Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    for (key, value) in fields.as_object().unwrap() {
        payload[key] = value.clone();
    }
    payload.to_string()
}

/// What an agent gets from `legionctl hook <event>` (Claude Code's hook JSON, the fixture session) in `cwd`.
fn hook_out(op: &Operator, cwd: &Path, event: &str) -> String {
    let out = op.run(
        cwd,
        &[],
        &["hook", event],
        &claude_hook(event, &json!({"cwd": cwd})),
    );
    String::from_utf8_lossy(&out.stdout).to_string()
}

/// An operator with an empty HOME and the repo's plugin skills.
/// `PATH` with this build's binaries first: the operator finds this `legionctl` (the git gate), not an installed one.
fn path_with_build() -> OsString {
    let bin = Path::new(env!("CARGO_BIN_EXE_legionctl"))
        .parent()
        .unwrap()
        .to_path_buf();
    std::env::join_paths(
        [bin]
            .into_iter()
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap()
}

fn operator(tag: &str) -> (Operator, PathBuf) {
    // outside /tmp (private in a session sandbox) and outside any git repository, like a real home
    let home = Path::new("/var/tmp").join(format!("legionctl-e2e-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    (operator_in(tag, &home), home)
}

/// The operator of `home`, which keeps its files (an operator that starts again reads the config anew).
fn operator_in(tag: &str, home: &Path) -> Operator {
    operator_with(tag, home, &[])
}

/// An operator that reaches this machine's Docker, for the tests that build and run their own images and containers.
fn docker_operator(tag: &str) -> (Operator, PathBuf) {
    let (op, home) = operator(tag);
    drop(op);
    (operator_with(tag, &home, &docker()), home)
}

/// This machine's Docker (the harness points every other operator nowhere).
fn docker() -> [(&'static str, OsString); 1] {
    [(
        "DOCKER_HOST",
        std::env::var_os("DOCKER_HOST").unwrap_or_else(|| "unix:///var/run/docker.sock".into()),
    )]
}

fn operator_with(tag: &str, home: &Path, extra: &[(&str, OsString)]) -> Operator {
    let skills =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/agent-plugin/skills");
    // an empty machine: no /etc/legionctl unless a test writes one
    let root = home.join("root");
    std::fs::create_dir_all(&root).unwrap();
    let mut env = vec![
        ("LEGIONCTL_SYSROOT", root.into()),
        ("HOME", home.into()),
        ("PATH", path_with_keyring(home)),
        ("LEGIONCTL_PLUGIN_SKILLS", OsString::from(skills)),
    ];
    env.extend(extra.iter().cloned());
    Operator::start(tag, &env)
}

/// This build's `PATH` behind a stand-in `secret-tool` that keeps entries as files in `home/keyring`
/// (`<attribute value with / as _>`): no test reaches the user's real keyring.
fn path_with_keyring(home: &Path) -> OsString {
    let bin = home.join("keyring-bin");
    write(
        &bin.join("secret-tool"),
        &format!(
            "#!/bin/sh\nk={}; mkdir -p \"$k\"\ncase \"$1\" in\n  lookup) cat \"$k/$(printf %s \"$3\" | tr / _)\" 2>/dev/null || exit 1;;\n  store) cat > \"$k/$(printf %s \"$4\" | tr / _)\";;\nesac\n",
            home.join("keyring").display()
        ),
    );
    std::fs::set_permissions(
        bin.join("secret-tool"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path_with_build())))
        .unwrap()
}

#[test]
fn prompt_hook_injects_skill_reminders_and_user_actions_as_configured() {
    let (op, home) = operator("hooks");
    let run = |args: &[&str]| op.ok(args);
    // the session of the hook fixtures has a dossier, so the dossier gate lets its prompts through
    assert!(
        op.run(
            &home,
            &[("LEGIONCTL_SESSION", "SESSION")],
            &["dossier", "use", "hooks"],
            ""
        )
        .status
        .success()
    );
    let fresh = hook_out(&op, &home, "prompt");
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
    run(&["hooks", "bind", "prompt", "branch"]);
    let changed = hook_out(&op, &home, "prompt");
    assert!(
        !changed.contains("ste100"),
        "reminder turned off: {changed}"
    );
    assert!(changed.contains("glanceable-style"));
    assert!(
        changed.trim_end().ends_with("on-branch-main"),
        "user action runs after built-ins: {changed}"
    );

    let start = hook_out(&op, &home, "session-start");
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
    fn start(operator: &Operator, env: &[(&str, &str)]) -> Mcp {
        let mut child = Command::new(env!("CARGO_BIN_EXE_legionctl"))
            .arg("mcp")
            .env_clear()
            .env("XDG_RUNTIME_DIR", &operator.runtime)
            .envs(env.iter().copied())
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

/// A misspelled field is refused, not dropped: an agent's `dossier_edit` with `lines` for `content` (as happened on
/// 2026-10-14) ran as an empty replace and deleted the plan row it was to change. Through the MCP server, as agents
/// call it: the call fails with the operation's real fields, and the row stays.
#[test]
fn an_unknown_input_field_is_refused_before_the_operation_runs() {
    let (op, home) = operator("fields");
    let repo = repository(&home);
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "s1"),
    ];
    for (args, stdin) in [
        (&["dossier", "use", "plan"][..], ""),
        (
            &["dossier", "write", "plan/plan.md", "--overwrite"][..],
            "# Plan\n| todo-1 | keep me |\n",
        ),
    ] {
        let o = op.run(&repo, &env, args, stdin);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    }
    let read = String::from_utf8_lossy(
        &op.run(&repo, &env, &["dossier", "read", "plan/plan.md"], "")
            .stdout,
    )
    .to_string();
    let anchor = read
        .lines()
        .find(|l| l.contains("todo-1"))
        .unwrap()
        .split('|')
        .next()
        .unwrap();

    let mut mcp = Mcp::start(&op, &env);
    let result = mcp.call(
        "tools/call",
        &json!({"name": "dossier_edit", "arguments": {
            "path": "plan/plan.md", "mode": "replace", "start": anchor, "lines": ["| todo-1 | changed |"]
        }}),
    );
    assert_eq!(result["isError"], true, "{result}");
    let error: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(
        error["error"]["message"],
        "dossier.edit has no field [\"lines\"]; its fields: [\"content\", \"end\", \"mode\", \"path\", \"session\", \"start\"]",
        "the refusal names the field and the real ones"
    );
    let after = op.run(&repo, &env, &["dossier", "read", "plan/plan.md"], "");
    assert!(
        String::from_utf8_lossy(&after.stdout).contains("| todo-1 | keep me |"),
        "the row stays"
    );
}

#[test]
fn mcp_server_has_one_tool_per_operation_and_calls_the_operator() {
    let (op, _home) = operator("mcp");
    let mut mcp = Mcp::start(&op, &[]);

    let tools = mcp.call("tools/list", &json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    let expected: Vec<String> = legionctl_api::OPS
        .iter()
        .filter(|o| !o.internal())
        .map(|o| o.name.replace(['.', '-'], "_"))
        .collect();
    assert_eq!(
        names, expected,
        "the registry's public operations are the tool list"
    );

    let result = mcp.call("tools/call", &json!({"name": "skills", "arguments": {}}));
    assert_ne!(result["isError"], true, "{result}");
    let skills: Value =
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
    let skills = skills.as_array().unwrap();
    let names: Vec<&str> = skills.iter().map(|s| s["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "dossier-practice",
            "glanceable-style",
            "machine-tools",
            "sessions-and-agents",
            "ste100-style",
            "tool-lifecycle",
            "verify-and-ship",
            "workspace-practice"
        ],
        "every plugin skill, sorted"
    );
    for s in skills {
        assert!(
            !s["description"].as_str().unwrap().is_empty(),
            "SKILL.md front matter parsed: {s}"
        );
        // the style and dossier skills apply to every reply, so a reminder keeps them in force; the tool skills
        // are loaded when their description matches the task
        let kept = matches!(
            s["name"].as_str().unwrap(),
            "dossier-practice" | "glanceable-style" | "ste100-style"
        );
        assert_eq!(
            s["reminder_on"],
            if kept { json!(true) } else { Value::Null },
            "reminder of {s}"
        );
        assert!(
            s["claude"].is_null(),
            "no plugin registry in a fresh HOME: {s}"
        );
    }

    // an agent's relative path is its own working directory, not the operator's
    let result = mcp.call(
        "tools/call",
        &json!({"name": "agents_list", "arguments": {"project": "some-project"}}),
    );
    let text = result["content"][0]["text"].as_str().unwrap();
    let here = std::env::current_dir()
        .unwrap()
        .join("some-project/.claude/settings.json");
    assert!(text.contains(&here.display().to_string()), "{text}");
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
        r#"{ "config": { "locked": ["load", "reminders.ste100"] }, "workspace": { "root": "/srv/worktrees" } }"#,
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
    // the session of the hook fixtures has a dossier, so the dossier gate lets its prompts through
    assert!(
        op.run(
            &home,
            &[("LEGIONCTL_SESSION", "SESSION")],
            &["dossier", "use", "layers"],
            ""
        )
        .status
        .success()
    );
    let inside = hook_out(&op, &sub, "prompt");
    assert!(inside.contains("Use the make targets."), "{inside}");
    let outside = hook_out(&op, &home, "prompt");
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
        json(&repo, &["config", "explain", "workspace.root"])["value"],
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
    let hook = |event: &str, input: Value| {
        out(op.run(&home, &[], &["hook", event], &claude_hook(event, &input)))
    };
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
            .contains("/legionctl:dossier use <name>"),
        "{blocked}"
    );
    let passing = hook(
        "prompt",
        json!({"session_id": "s1", "cwd": home, "prompt": "/legionctl:dossier use port"}),
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
    let bound = as_session("s1", &["dossier", "use", "port"], "");
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
    let hook = |event: &str, input: Value| {
        out(op.run(&home, &[], &["hook", event], &claude_hook(event, &input)))
    };
    let as_session = |session: &str, args: &[&str], stdin: &str| {
        let o = op.run(&home, &[("LEGIONCTL_SESSION", session)], args, stdin);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        out(o)
    };

    as_session("s1", &["dossier", "use", "port"], "");
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

    // a second session cannot take the dossier while s1 holds it. s1 quiet for two hours (within stale_hours) still
    // holds it, and its next prompt shows it active again
    sql(
        &home,
        "UPDATE bindings SET updated = updated - 7200 WHERE session = 's1'",
    );
    let take = || {
        let o = op.run(
            &home,
            &[("LEGIONCTL_SESSION", "s2")],
            &["dossier", "use", "port"],
            "",
        );
        assert!(!o.status.success());
        String::from_utf8_lossy(&o.stderr).to_string()
    };
    let refused = take();
    assert!(
        refused.contains("in use by session s1 (cli), last active 120 min ago"),
        "{refused}"
    );
    hook(
        "prompt",
        json!({"session_id": "s1", "cwd": home, "prompt": "go on"}),
    );
    assert!(
        take().contains("last active 0 min ago"),
        "the prompt hook did not refresh the binding"
    );
    // in a legionctl session the agent's hooks carry the agent's own session id; its end releases the legionctl
    // session's dossier
    op.run(
        &home,
        &[("LEGIONCTL_SESSION", "s1")],
        &["hook", "session-end"],
        &claude_hook("session-end", &json!({"session_id": "agent-inside"})),
    );
    assert!(
        as_session("s2", &["dossier", "use", "port"], "")
            .contains("newest memory:\n  MEMORY/01-intake.md  Intake")
    );
}

/// The agent has ended, and its scope with it (the test session ran without one, so its record is marked as
/// scoped): after the minute of grace, the next session takes the dossier without --steal.
fn ended_scope_frees_the_dossier(op: &Operator, home: &Path, notes: &Path, id: &str) {
    sql(home, "UPDATE sessions SET scoped = 1");
    sql(home, "UPDATE bindings SET updated = updated - 120");
    let next = op.run(
        notes,
        &[
            ("HOME", home.to_str().unwrap()),
            ("LEGIONCTL_SESSION", "next"),
        ],
        &["dossier", "use", "notes"],
        "",
    );
    let said = String::from_utf8_lossy(&next.stdout);
    assert!(
        said.contains(&format!("was held by session {id} (stale)")),
        "{said}{}",
        String::from_utf8_lossy(&next.stderr)
    );
}

/// Dossier nudges: replies that change the workspace without a dossier write raise the level (here every reply:
/// `nudge_turns` 1). Info shows on the next prompt, warn stops the end of one reply, block stops each reply; a reply
/// that changed nothing does not count; a dossier write answers it; `dossier nudges` replays it all.
#[test]
fn a_session_that_works_without_writing_its_dossier_is_nudged_until_it_writes() {
    let (op, home) = operator("nudge");
    let repo = repository(&home);
    write(
        &home.join(".config/legionctl/nudge.jsonc"),
        r#"{ "dossier": { "nudge_turns": 1 } }"#,
    );
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "s1"),
    ];
    let ok = |args: &[&str], stdin: &str| {
        let o = op.run(&repo, &env, args, stdin);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let hook = |event: &str| {
        let o = op.run(
            &repo,
            &[],
            &["hook", event],
            &claude_hook(event, &json!({"session_id": "s1", "cwd": repo})),
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let blocked = |out: &str| -> Option<String> {
        serde_json::from_str::<Value>(out)
            .ok()
            .map(|v| v["reason"].as_str().unwrap().to_string())
    };
    ok(&["dossier", "use", "port"], "");
    ok(&["workspace", "open"], "");
    let file = home.join("worktrees/repo/port/work.txt");
    let mut n = 0;
    let mut work = |stop: bool| {
        n += 1;
        write(&file, &format!("change {n}\n"));
        if stop { hook("stop") } else { String::new() }
    };

    assert_eq!(hook("stop"), "", "the first reply sets the base");
    assert_eq!(work(true), "", "info does not stop the reply");
    assert!(
        hook("prompt")
            .contains("1 replies changed the workspace since this session last wrote dossier port"),
        "info is in the context of the next prompt"
    );
    let warn = blocked(&work(true)).expect("warn stops the end of the reply");
    assert!(warn.contains("2 replies changed"), "{warn}");
    assert_eq!(
        hook("stop"),
        "",
        "a reply that changed nothing does not count, and warn stops only once"
    );
    let block = blocked(&work(true)).expect("block");
    assert!(block.contains("stops the end of each reply"), "{block}");
    assert!(
        blocked(&hook("stop")).is_some(),
        "block stops each reply until a dossier write"
    );

    // a ledger line starts with '-': the CLI takes it as the value of --content, not as an option
    ok(
        &[
            "dossier",
            "append",
            "port/plan.md",
            "--content",
            "- state recorded",
        ],
        "",
    );
    assert_eq!(hook("stop"), "", "the write answered the nudge");
    let page: Value =
        serde_json::from_str(&ok(&["dossier", "nudges", "--of", "s1", "--json"], "")).unwrap();
    let replay = page["items"].as_array().unwrap();
    let seen: Vec<(String, Value, Value)> = replay
        .iter()
        .map(|e| {
            (
                e["kind"].as_str().unwrap().to_string(),
                e["payload"]["level"].clone(),
                e["payload"]["turns"].clone(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("dossier.nudge".into(), json!("info"), json!(1)),
            ("dossier.nudge".into(), json!("warn"), json!(2)),
            ("dossier.nudge".into(), json!("block"), json!(3)),
            ("dossier.ack".into(), Value::Null, json!(0)),
        ]
    );
}

/// The operator audits every call that is not a read, with its input, outcome code and time: a refused change has
/// the code of its error (in the CLI's error and in the audit), a secret input stays out of the audit, and the
/// metrics count the error by code.
#[test]
fn every_change_is_audited_with_its_outcome_and_errors_have_codes() {
    let (op, home) = operator("audit");
    let run =
        |session: &str, args: &[&str]| op.run(&home, &[("LEGIONCTL_SESSION", session)], args, "");
    assert!(run("s1", &["dossier", "use", "port"]).status.success());
    let refused = run("s2", &["dossier", "use", "port"]);
    assert!(!refused.status.success());
    let token = "sk-not-for-the-audit";
    let set = run(
        "s1",
        &[
            "agents",
            "set",
            "claude",
            "user",
            "env.TOKEN",
            token,
            "--dry-run",
        ],
    );
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    assert!(run("s1", &["dossier", "list"]).status.success());

    let page: Value = serde_json::from_slice(&run("s1", &["events", "--json"]).stdout).unwrap();
    let events = page["items"].as_array().unwrap();
    let audit: Vec<(String, Value)> = events
        .iter()
        .map(|e| {
            (
                e["kind"].as_str().unwrap().to_string(),
                e["payload"].clone(),
            )
        })
        .collect();
    let kinds: Vec<&str> = audit.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(
        kinds,
        ["dossier.use", "dossier.use", "agents.set"],
        "reads are not audited"
    );
    assert_eq!(
        (
            audit[0].1["outcome"].clone(),
            audit[1].1["outcome"].clone(),
            audit[1].1["input"]["name"].clone()
        ),
        (Value::Null, json!("conflict"), json!("port"))
    );
    assert_eq!(audit[2].1["input"]["value"], "<secret>");
    assert!(!serde_json::to_string(&events).unwrap().contains(token));

    let metrics = get(&op, "/v1/metrics");
    assert!(
        metrics.contains("legionctl_op_errors_total{op=\"dossier.use\",code=\"conflict\"} 1")
            && metrics.contains("legionctl_op_calls_total{op=\"dossier.use\"} 2"),
        "{metrics}"
    );
}

/// `legionctl trace` follows the operator's event stream: what happened before it started stays out, and a change
/// made while it runs shows at once (no polling in the client).
#[test]
fn trace_shows_each_change_as_it_happens() {
    let (op, home) = operator("trace");
    let use_dossier = |name: &str| {
        let out = op.run(
            &home,
            &[("LEGIONCTL_SESSION", "s1")],
            &["dossier", "use", name],
            "",
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    use_dossier("before");
    let mut trace = Command::new(env!("CARGO_BIN_EXE_legionctl"))
        .arg("trace")
        .env_clear()
        .env("XDG_RUNTIME_DIR", &op.runtime)
        .env("PATH", std::env::var_os("PATH").unwrap())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let (lines, seen) = std::sync::mpsc::channel();
    let out = BufReader::new(trace.stdout.take().unwrap());
    std::thread::spawn(move || {
        for line in out.lines().map_while(Result::ok) {
            if lines.send(line).is_err() {
                break;
            }
        }
    });
    // the stream starts at the newest event; give it a moment to connect
    std::thread::sleep(std::time::Duration::from_millis(500));
    let started = std::time::Instant::now();
    use_dossier("during");
    let line = seen
        .recv_timeout(std::time::Duration::from_secs(5))
        .expect("trace showed nothing");
    let _ = trace.kill();
    let _ = trace.wait();
    assert!(
        line.contains("dossier.use") && line.contains("during"),
        "{line}"
    );
    assert!(!line.contains("before"), "the backlog stays out: {line}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
}

/// The wire format is reviewed: `docs/api/schema.json` holds every operation's class, visibility, input and output
/// schema, and the code must match it, so a change to a type on the wire shows up as a diff of that file
/// (`LEGIONCTL_WRITE_SCHEMA=1` writes it after a deliberate change). The schemas of the planning files
/// (`planning/*.schema.json`) are reviewed the same way.
#[test]
fn the_wire_format_matches_the_committed_schema() {
    for (rel, schema) in legionctl_core::planning::schemas() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(rel);
        let text = serde_json::to_string_pretty(&schema).unwrap() + "\n";
        if std::env::var_os("LEGIONCTL_WRITE_SCHEMA").is_some() {
            std::fs::write(&path, &text).unwrap();
        }
        assert!(
            std::fs::read_to_string(&path).unwrap_or_default() == text,
            "{rel} differs from the planning types; if the change is meant, run \
             LEGIONCTL_WRITE_SCHEMA=1 cargo test -p legionctl --test e2e the_wire_format and commit the file"
        );
    }
    let schema: serde_json::Map<String, Value> = legionctl_api::OPS
        .iter()
        .map(|op| {
            (
                op.name.to_string(),
                json!({
                    "class": op.class,
                    "internal": op.internal(),
                    "input": (op.input)(),
                    "output": (op.output)(),
                }),
            )
        })
        .collect();
    let text = serde_json::to_string_pretty(&schema).unwrap() + "\n";
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/api/schema.json");
    if std::env::var_os("LEGIONCTL_WRITE_SCHEMA").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &text).unwrap();
    }
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    if committed != text {
        let changed: Vec<&String> = schema
            .iter()
            .filter(|(name, now)| {
                serde_json::from_str::<Value>(&committed)
                    .ok()
                    .and_then(|c| c.get(name.as_str()).cloned())
                    .as_ref()
                    != Some(*now)
            })
            .map(|(name, _)| name)
            .collect();
        panic!(
            "the wire format differs from docs/api/schema.json in {changed:?}; if the change is meant, run \
             LEGIONCTL_WRITE_SCHEMA=1 cargo test -p legionctl --test e2e the_wire_format and commit the file"
        );
    }
}

/// Logs come in pages (DESIGN/03): `next` is the cursor of the following page, and the last page has none; a limit
/// outside 1-1000 is invalid input. Retention: rows older than `events.keep_days` are gone after the operator's
/// next start.
#[test]
fn the_event_log_pages_and_keeps_only_its_days() {
    let (op, home) = operator("pages");
    for name in ["one", "two", "three"] {
        let out = op.run(
            &home,
            &[("LEGIONCTL_SESSION", "s1")],
            &["dossier", "use", name],
            "",
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let page = |args: &[&str]| -> Value {
        serde_json::from_str(&op.ok(&[&["events", "--json"], args].concat())).unwrap()
    };
    let first = page(&["--limit", "2"]);
    assert_eq!(first["items"].as_array().unwrap().len(), 2);
    let next = first["next"]
        .as_str()
        .expect("a cursor to the rest")
        .to_string();
    let rest = page(&["--limit", "2", "--cursor", &next]);
    let names: Vec<&str> = [&first, &rest]
        .iter()
        .flat_map(|p| p["items"].as_array().unwrap())
        .map(|e| e["payload"]["input"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["one", "two", "three"]);
    assert_eq!(rest["next"], Value::Null, "the last page");
    let refused = op.cli(&["events", "--limit", "0"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("--limit must be 1-1000"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );

    // two of the rows are 100 days old; the next operator on this home removes them at its start (default 90 days)
    let aged = Command::new("python3")
        .args([
            "-c",
            "import sqlite3, sys; c = sqlite3.connect(sys.argv[1]); \
             c.execute('UPDATE events SET ts = ts - 100 * 86400 WHERE id IN (SELECT id FROM events ORDER BY id LIMIT 2)'); c.commit()",
        ])
        .arg(home.join(".local/state/legionctl/operator.db"))
        .status()
        .unwrap();
    assert!(aged.success());
    drop(op);
    let again = Operator::start(
        "pages2",
        &[
            ("HOME", home.clone().into()),
            ("LEGIONCTL_SYSROOT", home.join("root").into()),
            ("PATH", path_with_build()),
        ],
    );
    let kept: Value = serde_json::from_str(&again.ok(&["events", "--json"])).unwrap();
    let names: Vec<&str> = kept["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["payload"]["input"]["name"].as_str().unwrap_or(""))
        .collect();
    assert_eq!(names, ["three"], "{kept}");
}

/// The body of `GET <path>` on the operator's socket, with the user token.
fn get(op: &Operator, path: &str) -> String {
    let token = std::fs::read_to_string(op.runtime.join(legionctl_api::USER_TOKEN)).unwrap();
    let mut stream =
        std::os::unix::net::UnixStream::connect(op.runtime.join("legionctl/api.sock")).unwrap();
    write!(
        stream,
        "GET {path} HTTP/1.1\r\nhost: l\r\nconnection: close\r\nauthorization: Bearer {token}\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    std::io::Read::read_to_string(&mut stream, &mut response).unwrap();
    response.split_once("\r\n\r\n").unwrap().1.to_string()
}

/// Run one statement on the dossier database of the test's home (to age a binding; nothing else changes it).
fn sql(home: &Path, statement: &str) {
    let out = Command::new("python3")
        .args([
            "-c",
            "import sqlite3, sys; c = sqlite3.connect(sys.argv[1]); c.execute(sys.argv[2]); c.commit()",
        ])
        .arg(home.join(".agentworkspace/workspace.db"))
        .arg(statement)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// git with the test's own HOME (its identity and default branch), in `dir`.
fn git(home: &Path, dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("HOME", home)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository with one commit on main, and workspaces configured under `home/worktrees`.
fn repository(home: &Path) -> PathBuf {
    write(
        &home.join(".gitconfig"),
        "[user]\n  name = Test\n  email = test@example.com\n[init]\n  defaultBranch = main\n",
    );
    write(
        &home.join(".config/legionctl/workspace.jsonc"),
        &format!(
            r#"{{ "workspace": {{ "root": "{}" }} }}"#,
            home.join("worktrees").display()
        ),
    );
    let repo = home.join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(home, &repo, &["init", "--quiet"]);
    write(&repo.join("README.md"), "repo\n");
    git(home, &repo, &["add", "."]);
    git(home, &repo, &["commit", "--quiet", "-m", "start"]);
    repo
}

#[test]
fn workspace_opens_checkpoints_reports_drift_merges_and_closes_in_a_real_repository() {
    let (op, home) = operator("workspace");
    let repo = repository(&home);
    let git_env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "s1"),
    ];
    let run_git = |dir: &Path, args: &[&str]| git(&home, dir, args);
    let ok = |cwd: &Path, args: &[&str]| {
        let o = op.run(cwd, &git_env, args, "");
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let hook = |event: &str| {
        let o = op.run(
            &repo,
            &[],
            &["hook", event],
            &claude_hook(event, &json!({"session_id": "s1", "cwd": repo})),
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    ok(&repo, &["dossier", "use", "port"]);

    // no workspace yet: the agent is told at session start, and before a reply ends once the main checkout changed
    assert!(hook("session-start").contains(&format!("no workspace for {} yet", repo.display())));
    assert_eq!(
        hook("stop"),
        "",
        "a clean main checkout needs no workspace yet"
    );
    write(&repo.join("README.md"), "changed in the main checkout\n");
    let stop: Value = serde_json::from_str(&hook("stop")).unwrap();
    assert!(
        stop["reason"]
            .as_str()
            .unwrap()
            .contains("has changes but dossier port has no workspace"),
        "{stop}"
    );
    run_git(&repo, &["checkout", "--quiet", "README.md"]);

    // open: a worktree on legionctl/port with the dossier linked in and its ignore line committed on the branch
    let ws = home.join("worktrees/repo/port");
    assert!(ok(&repo, &["workspace", "open"]).contains(&ws.display().to_string()));
    assert!(
        ws.join(".agentworkspace/plan.md").is_file(),
        "the dossier is linked into the workspace"
    );
    assert_eq!(
        run_git(&ws, &["status", "--porcelain"]),
        "",
        "the link is ignored"
    );
    assert!(hook("session-start").contains(&format!(
        "legionctl workspace: {} on legionctl/port (base main)",
        ws.display()
    )));

    // checkpoint, status, drift
    write(&ws.join("feature.rs"), "fn feature() {}\n");
    assert!(ok(&repo, &["workspace", "checkpoint", "add feature"]).ends_with("add feature"));
    let status: Value =
        serde_json::from_str(&ok(&repo, &["workspace", "status", "--json"])).unwrap();
    assert_eq!(
        (status["ahead"].as_u64(), status["behind"].as_u64()),
        (Some(2), Some(0)),
        "{status}"
    );
    run_git(&ws, &["switch", "--quiet", "-c", "elsewhere"]);
    let drifted = op.run(&repo, &git_env, &["workspace", "status"], "");
    assert!(!drifted.status.success(), "drift fails the status command");
    assert!(
        String::from_utf8_lossy(&drifted.stdout)
            .contains("drift: the branch is elsewhere, recorded legionctl/port")
    );
    run_git(&ws, &["switch", "--quiet", "legionctl/port"]);

    // merge: rebase onto main, then main moves forward; close keeps the branch
    run_git(
        &repo,
        &["commit", "--quiet", "--allow-empty", "-m", "main moved"],
    );
    ok(&repo, &["workspace", "merge"]);
    assert_eq!(
        run_git(&repo, &["log", "-1", "--format=%s"]),
        "add feature",
        "main has the dossier's work on top"
    );
    assert_eq!(
        run_git(&repo, &["rev-list", "--count", "HEAD"]),
        "4",
        "linear: start, main moved, ignore line, feature"
    );
    assert!(ok(&repo, &["workspace", "close"]).contains("branch legionctl/port kept"));
    assert!(!ws.exists());
    assert_eq!(
        run_git(&repo, &["branch", "--list", "legionctl/port"]),
        "legionctl/port"
    );
}

/// Topic workspaces of one dossier, open at once for agents that work in parallel: each has its own worktree and
/// branch, edits stay apart, checkpoint and status act on the workspace that contains the working directory (or on
/// `--topic`), merge of one moves main while the other stays, close refuses uncommitted work, and a reopened topic
/// continues on its branch.
#[test]
fn topic_workspaces_of_one_dossier_work_in_parallel_and_merge_one_by_one() {
    let (op, home) = operator("topics");
    let repo = repository(&home);
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "s1"),
    ];
    let run = |cwd: &Path, args: &[&str]| op.run(cwd, &env, args, "");
    let ok = |cwd: &Path, args: &[&str]| {
        let o = run(cwd, args);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    let status = |cwd: &Path, args: &[&str]| -> Value {
        serde_json::from_str(&ok(
            cwd,
            &[&["workspace", "status", "--json"], args].concat(),
        ))
        .unwrap()
    };
    let subject = |branch: &str| git(&home, &repo, &["log", "-1", "--format=%s", branch]);
    ok(&repo, &["dossier", "use", "port"]);
    let a = home.join("worktrees/repo/port--a");
    let b = home.join("worktrees/repo/port--b");
    let opened: Value =
        serde_json::from_str(&ok(&repo, &["workspace", "open", "--topic", "a", "--json"])).unwrap();
    assert_eq!(
        (opened["path"].as_str(), opened["branch"].as_str()),
        (a.to_str(), Some("legionctl/port--a"))
    );
    ok(&repo, &["workspace", "open", "--topic", "b"]);
    assert!(
        b.join(".agentworkspace/plan.md").is_file(),
        "each topic has the dossier linked"
    );
    let list = ok(&repo, &["workspace", "list"]);
    assert!(
        list.contains("legionctl/port--a") && list.contains("legionctl/port--b"),
        "{list}"
    );

    // edits stay apart; checkpoint from inside each (b from a subfolder) commits to its own branch
    write(&a.join("a.txt"), "a");
    write(&b.join("sub/b.txt"), "b");
    assert!(!b.join("a.txt").exists() && !a.join("sub").exists());
    assert!(ok(&a, &["workspace", "checkpoint", "work a"]).ends_with("work a"));
    assert!(ok(&b.join("sub"), &["workspace", "checkpoint", "work b"]).ends_with("work b"));
    assert_eq!(
        (subject("legionctl/port--a"), subject("legionctl/port--b")),
        ("work a".into(), "work b".into())
    );

    // status from inside each, and by topic from the main checkout
    write(&b.join("loose.txt"), "not committed");
    let (sa, sb) = (status(&a, &[]), status(&b, &[]));
    assert_eq!(
        (sa["branch"].as_str(), sa["untracked"].as_u64()),
        (Some("legionctl/port--a"), Some(0)),
        "{sa}"
    );
    assert_eq!(
        (sb["branch"].as_str(), sb["untracked"].as_u64()),
        (Some("legionctl/port--b"), Some(1)),
        "{sb}"
    );
    assert_eq!(
        status(&repo, &["--topic", "b"])["untracked"].as_u64(),
        Some(1)
    );

    // merge of a moves main; b stays on its own work
    ok(&a, &["workspace", "merge"]);
    assert_eq!(subject("main"), "work a");
    assert!(repo.join("a.txt").exists() && !repo.join("sub").exists());
    assert_eq!(subject("legionctl/port--b"), "work b");
    assert!(
        status(&b, &[])["behind"].as_u64() > Some(0),
        "b is behind main by a's work"
    );

    close_with_changes_needs_force_and_keeps_an_unmerged_branch(&repo, &b, run, ok);

    // reopen: the same branch with its committed work, without the discarded file
    ok(&repo, &["workspace", "open", "--topic", "b"]);
    assert!(b.join("sub/b.txt").exists() && !b.join("loose.txt").exists());
    assert_eq!(subject("legionctl/port--b"), "work b");
    ok(&b, &["workspace", "merge"]);
    assert_eq!(subject("main"), "work b");
    assert_eq!(
        git(&home, &repo, &["rev-list", "--count", "HEAD"]),
        "4",
        "linear: start, ignore line, work a, work b (b's own ignore line is the same patch)"
    );
    close_deletes_merged_topic_branches(&home, &repo, ok);
}

/// Close of b with uncommitted work is refused; `--force` closes it and keeps the unmerged branch; a stays open.
fn close_with_changes_needs_force_and_keeps_an_unmerged_branch(
    repo: &Path,
    b: &Path,
    run: impl Fn(&Path, &[&str]) -> Output,
    ok: impl Fn(&Path, &[&str]) -> String,
) {
    let refused = run(repo, &["workspace", "close", "--topic", "b"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("uncommitted changes"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(b.join("loose.txt").exists());
    let closed = ok(repo, &["workspace", "close", "--topic", "b", "--force"]);
    assert!(
        closed.contains("branch legionctl/port--b kept: it is not merged into main"),
        "{closed}"
    );
    assert!(!b.exists());
    assert!(
        ok(repo, &["workspace", "list"]).contains("legionctl/port--a"),
        "closing b leaves a open"
    );
}

/// Close of a merged topic deletes its branch (`git branch -d`); no topic branch stays after its merge and close.
fn close_deletes_merged_topic_branches(
    home: &Path,
    repo: &Path,
    ok: impl Fn(&Path, &[&str]) -> String,
) {
    for t in ["a", "b"] {
        let closed = ok(repo, &["workspace", "close", "--topic", t]);
        assert!(
            closed.contains(&format!("branch legionctl/port--{t} merged and deleted")),
            "{closed}"
        );
    }
    assert_eq!(git(home, repo, &["branch", "--list", "legionctl/*"]), "");
}

/// The MCP proxy between a session's agent and a server (here legionctl's own MCP server): the agent sees only the
/// allowed tools, a call to another tool is refused, and the operator logs both calls with the session id.
/// A program in the operator's `PATH` (the stand-in folder of `path_with_keyring`), written after the operator started.
fn stand_in(home: &Path, name: &str, script: &str) {
    let path = home.join("keyring-bin").join(name);
    write(&path, script);
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// A `mutants.out` as cargo-mutants 27.1 writes it (shape recorded from a real run on a fixture crate, see
/// `cargo_mutants_on_a_fixture_crate_records_the_tests_that_kill_its_mutants`): `outcomes.json` with the baseline
/// first, then one outcome per mutant (`file`, `line`, `name`, the tests that fail in its log; none: missed).
fn mutants_out(dir: &Path, mutants: &[(&str, u32, &str, &[&str])]) {
    let phases = json!([
        {"phase": "Build", "duration": 0.1, "process_status": "Success", "argv": ["cargo", "test", "--no-run"]},
        {"phase": "Test", "duration": 0.05, "process_status": "Success", "argv": ["cargo", "test"]}
    ]);
    let mut outcomes = vec![
        json!({"scenario": "Baseline", "summary": "Success", "log_path": "log/baseline.log",
        "diff_path": null, "phase_results": phases}),
    ];
    for (i, (file, line, name, killed)) in mutants.iter().enumerate() {
        let log = format!("log/m{i}.log");
        let tests = killed
            .iter()
            .fold(String::new(), |all, t| all + "test " + t + " ... FAILED\n");
        write(
            &dir.join(&log),
            &format!("\nrunning 3 tests\ntest other ... ok\n{tests}\n*** result\n"),
        );
        outcomes.push(json!({
            "scenario": {"Mutant": {"name": format!("{file}:{line}:5: {name}"), "package": "fx", "file": file,
                "span": {"start": {"line": line, "column": 5}, "end": {"line": line, "column": 9}},
                "replacement": "0", "genre": "FnValue"}},
            "summary": if killed.is_empty() { "MissedMutant" } else { "CaughtMutant" },
            "log_path": log, "diff_path": format!("diff/m{i}.diff"), "phase_results": phases,
        }));
    }
    let caught = mutants.iter().filter(|m| !m.3.is_empty()).count();
    write(
        &dir.join("outcomes.json"),
        &json!({"outcomes": outcomes, "total_mutants": mutants.len(), "caught": caught,
            "missed": mutants.len() - caught, "timeout": 0, "unviable": 0, "success": 0,
            "cargo_mutants_version": "27.1.0"})
        .to_string(),
    );
}

/// A stand-in `cargo-mutants` in the operator's `PATH`: it logs its arguments to `home/mutants-argv` and writes
/// `mutants` (see [`mutants_out`]) into the `mutants.out` of its `-o` folder.
fn mutants_stand_in(home: &Path, mutants: &[(&str, u32, &str, &[&str])]) {
    let fixture = home.join("mutants-fixture");
    mutants_out(&fixture, mutants);
    stand_in(
        home,
        "cargo-mutants",
        &format!(
            "#!/bin/sh\necho \"$@\" > {argv}\nwhile [ $# -gt 0 ]; do [ \"$1\" = -o ] && out=$2; shift; done\nmkdir -p \"$out\" && cp -r {fixture} \"$out/mutants.out\"\nexit 2\n",
            argv = home.join("mutants-argv").display(),
            fixture = fixture.display()
        ),
    );
}

/// An agent's session (s1) on dossier `gate` with a workspace of a new repository, and `double` in its `src/lib.rs`.
struct GateAgent {
    op: Operator,
    home: PathBuf,
    ws: PathBuf,
}

impl GateAgent {
    fn start(tag: &str) -> GateAgent {
        let (op, home) = operator(tag);
        let repo = repository(&home);
        let agent = GateAgent {
            ws: home.join("worktrees/repo/gate"),
            op,
            home,
        };
        agent.ok_in(&repo, &["dossier", "use", "gate"]);
        agent.ok_in(&repo, &["workspace", "open"]);
        write(
            &agent.ws.join("src/lib.rs"),
            "pub fn double(x: i32) -> i32 {\n    x * 2\n}\n",
        );
        agent
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        let env = [
            ("HOME", self.home.to_str().unwrap()),
            ("LEGIONCTL_SESSION", "s1"),
        ];
        self.op.run(&self.ws, &env, args, "")
    }

    fn ok_in(&self, cwd: &Path, args: &[&str]) -> String {
        let env = [
            ("HOME", self.home.to_str().unwrap()),
            ("LEGIONCTL_SESSION", "s1"),
        ];
        let o = self.op.run(cwd, &env, args, "");
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    }

    fn ok(&self, args: &[&str]) -> String {
        self.ok_in(&self.ws, args)
    }

    /// The error of a refused checkpoint.
    fn refused(&self, args: &[&str]) -> String {
        let o = self.run(args);
        assert!(!o.status.success(), "{args:?} must be refused");
        String::from_utf8_lossy(&o.stderr).to_string()
    }

    fn last_commit(&self) -> String {
        git(&self.home, &self.ws, &["log", "-1", "--format=%s"])
    }

    /// git as the user runs it in the workspace: its hooks find this build's `legionctl` and this test's operator.
    fn git_user(&self, args: &[&str]) -> std::process::Output {
        let bin = Path::new(env!("CARGO_BIN_EXE_legionctl")).parent().unwrap();
        let path = std::env::join_paths(
            std::iter::once(bin.to_path_buf())
                .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        Command::new("git")
            .arg("-C")
            .arg(&self.ws)
            .args(args)
            .env_clear()
            .env("HOME", &self.home)
            .env("PATH", path)
            .env("XDG_RUNTIME_DIR", &self.op.runtime)
            .output()
            .unwrap()
    }

    /// The review of the working tree: each reviewed test's name and its assertions.
    fn reviewed(&self) -> Vec<(String, u64)> {
        let review: Value =
            serde_json::from_str(&self.ok(&["code", "tests", "review", "--json"])).unwrap();
        review["tests"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["name"].as_str().unwrap().to_string(),
                    t["assertions"].as_u64().unwrap(),
                )
            })
            .collect()
    }

    /// Record a hand mutation of `double` that `tests` caught.
    fn kill(&self, tests: &[&str]) -> String {
        self.ok(&[
            &[
                "code",
                "tests",
                "mutant",
                "record",
                "src/lib.rs",
                "2",
                "x * 2 -> x + 2",
            ],
            tests,
        ]
        .concat())
    }
}

/// One test of each form that the review must find (the file need not compile: the review parses it).
// a .txt file: the test gate of the installed legionctl (a line parser) would read its tests as tests of e2e.rs
const FORMS: &str = include_str!("fixtures/test-forms.txt");

const GATE_TESTS: &str =
    "use fx::double;\n\n#[test]\nfn doubles_three() {\n    assert_eq!(double(3), 6);\n}\n";

/// The test gate of `workspace checkpoint` (design 23, A7): a new test without an assertion and a test that kills no
/// mutant are refused with their reasons and nothing is committed; an agent's recorded mutation is evidence;
/// `--accept-test` needs `--reason`, lets one test through, and the reason is kept with the test and is an event.
#[test]
fn the_checkpoint_gate_refuses_tests_without_evidence_and_takes_recorded_kills_and_accepts() {
    let agent = GateAgent::start("testgate");
    write(
        &agent.ws.join("tests/t.rs"),
        &format!("{GATE_TESTS}\n#[test]\nfn only_calls() {{\n    let _ = double(1);\n}}\n"),
    );

    // no evidence: both tests refused with their reasons, nothing committed
    let start = agent.last_commit();
    let why = agent.refused(&["workspace", "checkpoint", "double"]);
    assert!(
        why.contains("only_calls (tests/t.rs): no assertion"),
        "{why}"
    );
    assert!(
        why.contains("doubles_three (tests/t.rs): kills no mutant"),
        "{why}"
    );
    assert!(why.contains("nothing committed"), "{why}");
    assert_eq!(
        agent.last_commit(),
        start,
        "a refused checkpoint commits nothing"
    );

    // an agent's recorded mutation is evidence for the test that caught it
    agent.ok(&[
        "code",
        "tests",
        "mutant",
        "record",
        "src/lib.rs",
        "2",
        "x * 2 -> x + 2",
        "doubles_three",
    ]);
    let why = agent.refused(&["workspace", "checkpoint", "double"]);
    assert!(
        !why.contains("doubles_three"),
        "the recorded kill counts: {why}"
    );
    assert!(why.contains("only_calls"), "{why}");
    assert_eq!(agent.last_commit(), start);

    // --accept-test lets one through; the reason is kept with the test and is an event
    let reason = "smoke test of the public path; its value is that it links";
    let why = agent.refused(&[
        "workspace",
        "checkpoint",
        "double",
        "--accept-test",
        "only_calls",
    ]);
    assert!(why.contains("--reason"), "an accept needs a reason: {why}");
    agent.ok(&[
        "workspace",
        "checkpoint",
        "double",
        "--accept-test",
        "only_calls",
        "--reason",
        reason,
    ]);
    assert_eq!(agent.last_commit(), "double");
    let events: Value = serde_json::from_str(&agent.ok(&["events", "--json"])).unwrap();
    let accept = events["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "code.tests.accept")
        .expect("the accept is an event");
    assert_eq!(
        (
            accept["payload"]["test"].as_str(),
            accept["payload"]["reason"].as_str()
        ),
        (Some("only_calls"), Some(reason))
    );
    let shown: Value =
        serde_json::from_str(&agent.ok(&["code", "tests", "show", "only_calls", "--json"]))
            .unwrap();
    assert_eq!(shown["accepts"][0]["reason"], reason);
}

/// The pre-commit hook (`scripts/test-gate.sh`) through a real `git commit -a` and `git commit <path>`: both commit a
/// temporary index of their own (`GIT_INDEX_FILE`), not the repository's index. The hook reviews that index: a new
/// test without a kill is refused and nothing is committed; with its kill both forms commit.
#[test]
fn the_pre_commit_hook_reviews_the_index_of_git_commit_a_and_git_commit_path() {
    let agent = GateAgent::start("hookgate");
    write(&agent.ws.join("tests/t.rs"), GATE_TESTS);
    agent.kill(&["doubles_three"]);
    agent.ok(&["workspace", "checkpoint", "double"]);
    let hooks = git(
        &agent.home,
        &agent.ws,
        &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
    );
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../scripts/test-gate.sh");
    std::fs::create_dir_all(&hooks).unwrap();
    std::fs::copy(script, Path::new(&hooks).join("pre-commit")).unwrap();

    // a tracked test file changed in the working tree only: the repository's index has nothing
    let more =
        format!("{GATE_TESTS}\n#[test]\nfn zero_stays() {{\n    assert_eq!(double(0), 0);\n}}\n");
    write(&agent.ws.join("tests/t.rs"), &more);
    for args in [
        &["commit", "-a", "-m", "all"][..],
        &["commit", "-m", "path", "tests/t.rs"][..],
    ] {
        let out = agent.git_user(args);
        let text = String::from_utf8_lossy(&out.stdout).to_string()
            + &String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success(),
            "git {args:?} must be refused: {text}"
        );
        assert!(text.contains("REFUSED  zero_stays (tests/t.rs)"), "{text}");
        assert_eq!(
            agent.last_commit(),
            "double",
            "git {args:?} committed nothing"
        );
    }

    agent.kill(&["zero_stays"]);
    let out = agent.git_user(&["commit", "-m", "path", "tests/t.rs"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(agent.last_commit(), "path");
    write(
        &agent.ws.join("tests/t.rs"),
        &format!("{more}\n#[test]\nfn four() {{\n    assert_eq!(double(2), 4);\n}}\n"),
    );
    agent.kill(&["four"]);
    let out = agent.git_user(&["commit", "-a", "-m", "all"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(agent.last_commit(), "all");
}

/// The forms of a test function (`#[test]` on the line of the `fn`, `#[rstest]`, `#[test_case(..)]`,
/// `#[tokio::test]`, `#[sqlx::test]`, a comment after the attribute, a test in a module) are each found, by libtest
/// name; assertions are the assert macros, `unwrap_err` and `#[should_panic]`, not a comment, a string or
/// `my_assert!`; a `"}"` in a string does not end a test, so a change after it is a change of that test.
#[test]
fn the_review_finds_each_test_form_and_counts_assertions_in_code_only() {
    let agent = GateAgent::start("testforms");
    write(&agent.ws.join("tests/forms.rs"), FORMS);
    let mut found = agent.reviewed();
    found.sort();
    let expected: Vec<(String, u64)> = [
        ("case", 1),
        ("commented", 0),
        ("inner::nested", 1),
        ("one_line", 1),
        ("panics", 1),
        ("rs_test", 1),
        ("sqlx_form", 1),
        ("tokio_form", 1),
    ]
    .iter()
    .map(|(n, a)| ((*n).to_string(), *a))
    .collect();
    assert_eq!(found, expected);

    git(&agent.home, &agent.ws, &["add", "tests/forms.rs"]);
    git(
        &agent.home,
        &agent.ws,
        &["commit", "--quiet", "-m", "forms"],
    );
    write(
        &agent.ws.join("tests/forms.rs"),
        &FORMS.replace("assert!(!open.is_empty());", "assert!(open.len() == 1);"),
    );
    assert_eq!(
        agent.reviewed(),
        [("inner::nested".to_string(), 1)],
        "the line after the string is in the test"
    );
}

/// A test removed from below another test (a diff hunk `+c,0` at the other test's last line) changes no test.
#[test]
fn removing_a_test_does_not_mark_the_test_above_it_as_changed() {
    let agent = GateAgent::start("testremoved");
    write(&agent.ws.join("tests/forms.rs"), FORMS);
    git(&agent.home, &agent.ws, &["add", "tests/forms.rs"]);
    git(
        &agent.home,
        &agent.ws,
        &["commit", "--quiet", "-m", "forms"],
    );
    let without = FORMS.replace(
        "#[test]\n#[should_panic]\nfn panics() {\n    double(i32::MAX);\n}\n",
        "",
    );
    assert_ne!(without, FORMS);
    write(&agent.ws.join("tests/forms.rs"), &without);
    assert_eq!(agent.reviewed(), [], "no new or changed test");
}

/// A kill and an accept count for the test as it was when they were recorded: a test with the same name in another
/// file has no evidence; a name that names two tests is refused; a change of the test's body makes the gate ask again.
#[test]
fn evidence_counts_only_for_the_test_version_it_was_recorded_for() {
    let agent = GateAgent::start("stale");
    write(&agent.ws.join("tests/t.rs"), GATE_TESTS);
    agent.kill(&["doubles_three"]);
    agent.ok(&["workspace", "checkpoint", "double"]);

    // the same name and body in another file
    write(&agent.ws.join("tests/u.rs"), GATE_TESTS);
    let why = agent.refused(&["workspace", "checkpoint", "copy"]);
    assert!(
        why.contains("doubles_three (tests/u.rs): kills no mutant"),
        "{why}"
    );
    let ambiguous = agent.run(&[
        "code",
        "tests",
        "mutant",
        "record",
        "src/lib.rs",
        "2",
        "x",
        "doubles_three",
    ]);
    let text = String::from_utf8_lossy(&ambiguous.stderr);
    assert!(
        !ambiguous.status.success() && text.contains("names 2 tests"),
        "{text}"
    );
    agent.kill(&["tests/u.rs::doubles_three"]);
    agent.ok(&["workspace", "checkpoint", "copy"]);

    // a changed body: the kill was for the old one
    let changed = GATE_TESTS.replace("assert_eq!(double(3), 6);", "assert!(double(3) > 0);");
    write(&agent.ws.join("tests/t.rs"), &changed);
    let why = agent.refused(&["workspace", "checkpoint", "weaker"]);
    assert!(
        why.contains("doubles_three (tests/t.rs): kills no mutant"),
        "{why}"
    );
    agent.ok(&[
        "workspace",
        "checkpoint",
        "weaker",
        "--accept-test",
        "tests/t.rs::doubles_three",
        "--reason",
        "a smoke check",
    ]);
    assert_eq!(agent.last_commit(), "weaker");

    // the accept was for that body too
    write(
        &agent.ws.join("tests/t.rs"),
        &changed.replace("assert!(double(3) > 0);", "assert!(double(3) > 1);"),
    );
    let why = agent.refused(&["workspace", "checkpoint", "again"]);
    assert!(
        why.contains("doubles_three (tests/t.rs): kills no mutant"),
        "{why}"
    );
}

/// cargo-mutants (a stand-in that writes its real output) runs as a job on the branch's source files only and its
/// kills count; a test that kills nothing is refused; the review shows a test whose kills another test has as a
/// duplicate (a note, not a refusal); `--staged` reviews what `git commit` takes (the pre-commit hook).
#[test]
fn cargo_mutants_kills_count_for_the_gate_and_the_review_shows_duplicates() {
    let agent = GateAgent::start("mutantrun");
    write(&agent.ws.join("tests/t.rs"), GATE_TESTS);
    agent.ok(&[
        "code",
        "tests",
        "mutant",
        "record",
        "src/lib.rs",
        "2",
        "x * 2 -> x - 2",
        "doubles_three",
    ]);
    agent.ok(&["workspace", "checkpoint", "double"]);
    write(
        &agent.ws.join("tests/t.rs"),
        &format!(
            "{GATE_TESTS}\n#[test]\nfn doubles_two() {{\n    assert_eq!(double(2), 4);\n}}\n\n#[test]\nfn zero_stays() {{\n    assert_eq!(double(0), 0);\n}}\n"
        ),
    );
    let home = &agent.home;
    mutants_stand_in(
        home,
        &[
            (
                "src/lib.rs",
                2,
                "replace double -> i32 with 1",
                &["doubles_three", "doubles_two"],
            ),
            ("src/lib.rs", 2, "replace * with +", &["doubles_three"]),
            ("src/lib.rs", 2, "replace double -> i32 with 0", &[]),
        ],
    );
    let ran = agent.ok(&["code", "tests", "mutant", "run"]);
    assert!(ran.contains("2 caught, 1 missed"), "{ran}");
    assert!(
        ran.contains("missed: src/lib.rs:2:5: replace double -> i32 with 0"),
        "{ran}"
    );
    let argv = std::fs::read_to_string(home.join("mutants-argv")).unwrap();
    assert!(
        argv.contains("--file src/lib.rs")
            && !argv.contains("tests/t.rs")
            && !argv.contains("README"),
        "only the branch's source files: {argv}"
    );
    let jobs: Value = serde_json::from_str(&agent.ok(&["jobs", "list", "--json"])).unwrap();
    assert!(
        jobs["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["kind"] == "code.tests.mutant.run" && j["state"] == "done"),
        "{jobs}"
    );

    assert!(
        !agent.run(&["code", "tests", "review"]).status.success(),
        "a refused test fails the review"
    );
    let review: Value =
        serde_json::from_str(&agent.ok(&["code", "tests", "review", "--json"])).unwrap();
    let tests = review["tests"].as_array().unwrap();
    assert_eq!(tests.len(), 2, "only the new tests: {review}");
    let test = |name: &str| tests.iter().find(|t| t["name"] == name).unwrap();
    let two = test("doubles_two");
    assert_eq!(
        (two["kills"].as_u64(), two["duplicate_of"].as_str()),
        (Some(1), Some("doubles_three")),
        "{two}"
    );
    assert!(two["refused"].as_array().unwrap().is_empty(), "{two}");
    let zero = test("zero_stays");
    assert_eq!(
        (zero["kills"].as_u64(), zero["assertions"].as_u64()),
        (Some(0), Some(1)),
        "{zero}"
    );
    assert!(
        zero["refused"][0]
            .as_str()
            .unwrap()
            .starts_with("kills no mutant"),
        "{zero}"
    );

    let why = agent.refused(&["workspace", "checkpoint", "more"]);
    assert!(
        why.contains("zero_stays") && !why.contains("doubles_two"),
        "{why}"
    );
    assert_eq!(agent.last_commit(), "double", "nothing committed");
    // what a plain `git commit` takes: the pre-commit hook's review of the index
    let staged = agent.run(&["code", "tests", "review", "--staged"]);
    assert!(!staged.status.success());
    assert!(String::from_utf8_lossy(&staged.stdout).contains("REFUSED  zero_stays (tests/t.rs)"));
}

/// A stand-in `cargo` that logs its arguments to `home/cargo-argv` and gives the JSON messages and results of two
/// crates' `tests/e2e.rs` (the same names) and a unit test.
fn cargo_of_two_crates(home: &Path, repo: &Path) {
    let artifact = |src: &str, exe: &str| {
        json!({"reason": "compiler-artifact", "target": {"name": "t", "src_path": repo.join(src)},
            "profile": {"test": true}, "executable": format!("/build/target/release/deps/{exe}"), "fresh": true})
        .to_string()
    };
    stand_in(
        home,
        "cargo",
        &format!(
            "#!/bin/sh\necho \"$@\" > {argv}\necho '{a}'\necho '{b}'\necho '{x}'\n\
             echo '     Running tests/e2e.rs (target/release/deps/e2e-1)' >&2\necho 'test a_flow_works ... ok'\necho 'test b_flow_breaks ... FAILED'\n\
             echo '     Running tests/e2e.rs (target/release/deps/e2e-3)' >&2\necho 'test a_flow_works ... FAILED'\n\
             echo '     Running unittests src/lib.rs (target/release/deps/x-2)' >&2\necho 'test db::tests::c_needs_root ... ignored'\n\
             echo 'test result: FAILED. 1 passed; 1 failed; 1 ignored'\nexit 101\n",
            argv = home.join("cargo-argv").display(),
            a = artifact("crates/a/tests/e2e.rs", "e2e-1"),
            b = artifact("crates/b/tests/e2e.rs", "e2e-3"),
            x = artifact("crates/a/src/lib.rs", "x-2"),
        ),
    );
}

/// `code tests run` runs the suite as a job and records each test with the source of its test binary (relative to
/// the repository: two crates' `tests/e2e.rs` are two files), its result and the ref; the filter reaches libtest;
/// `code tests list` and `show` give them back.
#[test]
fn a_test_run_records_each_test_with_its_file_result_and_ref() {
    let (op, home) = operator("testruns");
    let repo = repository(&home);
    cargo_of_two_crates(&home, &repo);
    let out = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &["code", "tests", "run", "--filter", "flow"],
        "",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("2 failed, 1 ignored, 1 ok"));
    assert!(
        std::fs::read_to_string(home.join("cargo-argv"))
            .unwrap()
            .trim()
            .ends_with("-j8 -- flow")
    );
    let head = git(&home, &repo, &["rev-parse", "HEAD"]);
    let list: Value = serde_json::from_str(&op.ok(&[
        "code",
        "tests",
        "list",
        "--repo",
        repo.to_str().unwrap(),
        "--json",
    ]))
    .unwrap();
    let rows: Vec<(&str, &str, &str, &str)> = list
        .as_array()
        .unwrap()
        .iter()
        .map(|t| {
            (
                t["name"].as_str().unwrap(),
                t["file"].as_str().unwrap(),
                t["result"].as_str().unwrap(),
                t["git_ref"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                "db::tests::c_needs_root",
                "crates/a/src/lib.rs",
                "ignored",
                head.as_str()
            ),
            ("a_flow_works", "crates/a/tests/e2e.rs", "ok", head.as_str()),
            (
                "b_flow_breaks",
                "crates/a/tests/e2e.rs",
                "failed",
                head.as_str()
            ),
            (
                "a_flow_works",
                "crates/b/tests/e2e.rs",
                "failed",
                head.as_str()
            ),
        ]
    );
    let shown: Value = serde_json::from_str(&op.ok(&[
        "code",
        "tests",
        "show",
        "c_needs_root",
        "--repo",
        repo.to_str().unwrap(),
        "--json",
    ]))
    .unwrap();
    assert_eq!(
        shown["runs"][0]["result"], "ignored",
        "the function name finds libtest's name: {shown}"
    );
    let jobs: Value = serde_json::from_str(&op.ok(&["jobs", "list", "--json"])).unwrap();
    assert_eq!(jobs["items"][0]["kind"], "code.tests.run");
    assert_eq!(
        jobs["items"][0]["input"]["results"]["failed"], 2,
        "the job shows its progress: {jobs}"
    );
}

/// A test run that builds nothing (cargo exits with a compile error and no test ran) fails its job with the
/// compiler's error, and records no test.
#[test]
fn a_test_run_that_does_not_build_fails_with_the_compiler_error() {
    let (op, home) = operator("testbuild");
    let repo = repository(&home);
    let error = json!({"reason": "compiler-message", "message": {"rendered": "error[E0425]: cannot find value `y` in this scope"}});
    stand_in(
        &home,
        "cargo",
        &format!("#!/bin/sh\necho '{error}'\necho 'error: could not compile `fx`' >&2\nexit 101\n"),
    );
    let out = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &["code", "tests", "run"],
        "",
    );
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    assert!(
        text.contains("no test ran") && text.contains("error[E0425]: cannot find value `y`"),
        "{text}"
    );
    let jobs: Value = serde_json::from_str(&op.ok(&["jobs", "list", "--json"])).unwrap();
    assert_eq!(jobs["items"][0]["state"], "failed", "{jobs}");
}

/// The real cargo-mutants (not in CI; run it here with `cargo install cargo-mutants --locked`) on a fixture crate in a
/// workspace: its output parses, each mutant names the tests that killed it, and the review refuses the test that
/// kills nothing.
#[test]
#[ignore = "needs cargo-mutants; CI does not install it"]
fn cargo_mutants_on_a_fixture_crate_records_the_tests_that_kill_its_mutants() {
    let (op, home) = operator("realmutants");
    drop(op);
    let toolchain: Vec<(&str, OsString)> = ["RUSTUP_HOME", "CARGO_HOME"]
        .iter()
        .map(|k| {
            let real = std::env::var_os("HOME").map(PathBuf::from).unwrap();
            (
                *k,
                std::env::var_os(k).unwrap_or_else(|| {
                    real.join(if *k == "RUSTUP_HOME" {
                        ".rustup"
                    } else {
                        ".cargo"
                    })
                    .into()
                }),
            )
        })
        .collect();
    let op = operator_with("realmutants", &home, &toolchain);
    let repo = repository(&home);
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "s1"),
    ];
    let ok = |cwd: &Path, args: &[&str]| {
        let o = op.run(cwd, &env, args, "");
        (
            o.status.success(),
            String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr),
        )
    };
    write(
        &repo.join("Cargo.toml"),
        "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[workspace]\n",
    );
    write(&repo.join(".gitignore"), "/target\nCargo.lock\n");
    git(&home, &repo, &["add", "."]);
    git(&home, &repo, &["commit", "--quiet", "-m", "crate"]);
    assert!(ok(&repo, &["dossier", "use", "real"]).0);
    assert!(ok(&repo, &["workspace", "open"]).0);
    let ws = home.join("worktrees/repo/real");
    write(
        &ws.join("src/lib.rs"),
        "pub fn double(x: i32) -> i32 {\n    x * 2\n}\n",
    );
    write(
        &ws.join("tests/t.rs"),
        "#[test]\nfn doubles() {\n    assert_eq!(fx::double(3), 6);\n}\n\n#[test]\nfn calls() {\n    assert!(fx::double(1) != 7);\n}\n",
    );
    let (ran, out) = ok(&ws, &["code", "tests", "mutant", "run"]);
    assert!(ran && out.contains("caught"), "{out}");
    let (_, review) = ok(&ws, &["code", "tests", "review", "--json"]);
    let review: Value = serde_json::from_str(&review).unwrap();
    let kills = |name: &str| {
        review["tests"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == name)
            .unwrap()["kills"]
            .as_u64()
            .unwrap()
    };
    assert!(kills("doubles") >= 1, "{review}");
    assert_eq!(kills("calls"), 0, "{review}");
}

#[test]
fn the_mcp_proxy_offers_only_allowed_tools_and_logs_every_call() {
    use rmcp::ServiceExt;
    use rmcp::model::CallToolRequestParams;

    let (op, home) = operator("proxy");
    let exe = env!("CARGO_BIN_EXE_legionctl");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (tools, load, doctor) = runtime.block_on(async {
        let mut proxy = tokio::process::Command::new(exe);
        proxy
            .args([
                "mcp-proxy",
                "--server",
                "self",
                "--allow",
                "load",
                "--allow",
                "capabilities",
                "--",
                exe,
                "mcp",
            ])
            .env("XDG_RUNTIME_DIR", &op.runtime)
            .env("HOME", &home)
            .env("LEGIONCTL_SESSION", "s-proxy");
        let agent =
            ().serve(rmcp::transport::TokioChildProcess::new(proxy).unwrap())
                .await
                .unwrap();
        let mut tools: Vec<String> = agent
            .peer()
            .list_all_tools()
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        tools.sort();
        let load = agent
            .peer()
            .call_tool(CallToolRequestParams::new("load"))
            .await
            .unwrap();
        let doctor = agent
            .peer()
            .call_tool(CallToolRequestParams::new("doctor"))
            .await;
        (tools, load, doctor)
    });
    assert_eq!(tools, ["capabilities", "load"]);
    assert_ne!(load.is_error, Some(true), "{load:?}");
    assert!(
        doctor
            .as_ref()
            .is_err_and(|e| e.to_string().contains("doctor is not allowed for self")),
        "{doctor:?}"
    );

    let calls = op.run(
        &home,
        &[("HOME", home.to_str().unwrap())],
        &["tools", "calls", "--json"],
        "",
    );
    let calls: Value = serde_json::from_slice::<Value>(&calls.stdout).unwrap()["items"].clone();
    let summary: Vec<(String, String, String)> = calls
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["session"].as_str().unwrap().into(),
                c["tool"].as_str().unwrap().into(),
                c["outcome"].as_str().unwrap().into(),
            )
        })
        .collect();
    assert_eq!(
        summary,
        [
            ("s-proxy".into(), "load".into(), "ok".into()),
            ("s-proxy".into(), "doctor".into(), "refused".into())
        ]
    );
    assert_eq!(calls[0]["args_hash"].as_str().unwrap().len(), 32, "{calls}");
    assert!(calls[0]["bytes"].as_u64().unwrap() > 0, "{calls}");
}

/// A remote MCP server (streamable HTTP) behind the proxy: its header reaches it, only the allowed tool is offered,
/// and the call is logged like a local one.
#[test]
fn the_proxy_reaches_a_remote_server_with_its_header() {
    use rmcp::ServiceExt;
    use rmcp::model::CallToolRequestParams;

    let (op, home) = operator("remote");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-remote-mcp.py");
    // the stand-in picks its own port and says which: listening by then, and no race for the port
    let mut server = Command::new("python3")
        .arg(fixture)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut port = String::new();
    BufReader::new(server.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let port: u16 = port.trim().parse().expect("the stand-in's port");
    let url = format!("http://127.0.0.1:{port}/mcp");
    let (tools, answer) = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut proxy = tokio::process::Command::new(env!("CARGO_BIN_EXE_legionctl"));
        proxy
            .args([
                "mcp-proxy",
                "--server",
                "docs",
                "--allow",
                "docs",
                "--url",
                &url,
                "--header",
                "x-api-key=abc",
            ])
            .env("XDG_RUNTIME_DIR", &op.runtime)
            .env("HOME", &home)
            .env("LEGIONCTL_SESSION", "s-remote");
        let agent =
            ().serve(rmcp::transport::TokioChildProcess::new(proxy).unwrap())
                .await
                .unwrap();
        let tools: Vec<String> = agent
            .peer()
            .list_all_tools()
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect();
        let answer = agent
            .peer()
            .call_tool(CallToolRequestParams::new("docs"))
            .await
            .unwrap();
        (tools, answer)
    });
    let _ = server.kill();
    let _ = server.wait();
    assert_eq!(tools, ["docs"]);
    let text = serde_json::to_string(&answer.content).unwrap();
    assert!(text.contains("key=abc"), "{text}");
    let calls = op.run(
        &home,
        &[("HOME", home.to_str().unwrap())],
        &["tools", "calls", "--json"],
        "",
    );
    let calls: Value = serde_json::from_slice::<Value>(&calls.stdout).unwrap()["items"].clone();
    assert_eq!(
        (calls[0]["server"].as_str(), calls[0]["outcome"].as_str()),
        (Some("docs"), Some("ok"))
    );
}

/// The cache of the proxy: a tool its config lets it cache answers from the cache with the same result, until the
/// working copy changes; the log tells cached calls from real ones.
#[test]
fn the_proxy_reuses_a_cached_result_until_the_working_copy_changes() {
    use rmcp::ServiceExt;
    use rmcp::model::CallToolRequestParams;

    let (op, home) = operator("cache");
    let repo = repository(&home);
    let exe = env!("CARGO_BIN_EXE_legionctl");
    let results = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut proxy = tokio::process::Command::new(exe);
        proxy
            .args([
                "mcp-proxy",
                "--server",
                "self",
                "--cache",
                "hooks_list=60:workspace",
                "--",
                exe,
                "mcp",
            ])
            .current_dir(&repo)
            .env("XDG_RUNTIME_DIR", &op.runtime)
            .env("HOME", &home)
            .env("LEGIONCTL_SESSION", "s-cache");
        let agent =
            ().serve(rmcp::transport::TokioChildProcess::new(proxy).unwrap())
                .await
                .unwrap();
        let mut results = Vec::new();
        for change in [false, false, true] {
            if change {
                write(&repo.join("README.md"), "changed\n");
            }
            results.push(
                agent
                    .peer()
                    .call_tool(CallToolRequestParams::new("hooks_list"))
                    .await
                    .unwrap(),
            );
        }
        results
    });
    assert_ne!(results[0].is_error, Some(true), "{:?}", results[0]);
    assert_eq!(results[0], results[1], "the cached result is the same");
    let calls = op.run(
        &home,
        &[("HOME", home.to_str().unwrap())],
        &["tools", "calls", "--json"],
        "",
    );
    let outcomes: Vec<String> = serde_json::from_slice::<Value>(&calls.stdout).unwrap()["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["outcome"].as_str().unwrap().into())
        .collect();
    assert_eq!(
        outcomes,
        ["ok", "cached", "ok"],
        "a changed working copy is a new key"
    );
}

/// The cache contract (DESIGN/03): the operator builds tool-cache keys with the caller's session, so a sandbox does
/// not get, or plant, another session's entries; a namespace keeps to `cache.<ns>.max_bytes` by dropping the least
/// recently used; `cache stats` shows it and `cache purge` empties it.
#[test]
fn tool_cache_entries_belong_to_a_session_and_fit_the_limit() {
    let (op, home) = operator("cachescope");
    let repo = repository(&home);
    op.ok(&["events"]);
    for (token, session) in [("token-a", "s-a"), ("token-b", "s-b")] {
        add_session_token(&home, token, session);
    }
    // one hooks_list call through the proxy, as the session of `token` (none: the user); its outcome
    let call = |token: Option<&str>| -> String {
        use rmcp::ServiceExt;
        let exe = env!("CARGO_BIN_EXE_legionctl");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut proxy = tokio::process::Command::new(exe);
            proxy
                .args([
                    "mcp-proxy",
                    "--server",
                    "self",
                    "--cache",
                    "hooks_list=60",
                    "--",
                    exe,
                    "mcp",
                ])
                .current_dir(&repo)
                .env("XDG_RUNTIME_DIR", &op.runtime)
                .env("HOME", &home);
            if let Some(token) = token {
                proxy.env("LEGIONCTL_TOKEN", token);
            }
            let agent =
                ().serve(rmcp::transport::TokioChildProcess::new(proxy).unwrap())
                    .await
                    .unwrap();
            agent
                .peer()
                .call_tool(rmcp::model::CallToolRequestParams::new("hooks_list"))
                .await
                .unwrap();
        });
        let calls: Value = serde_json::from_str(&op.ok(&["tools", "calls", "--json"])).unwrap();
        calls["items"].as_array().unwrap().last().unwrap()["outcome"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let stats = || -> Value {
        let all: Value = serde_json::from_str(&op.ok(&["cache", "stats", "--json"])).unwrap();
        all.as_array()
            .unwrap()
            .iter()
            .find(|n| n["ns"] == "tools")
            .unwrap()
            .clone()
    };
    assert_eq!(
        [
            call(Some("token-a")),
            call(Some("token-a")),
            call(Some("token-b")),
            call(None)
        ],
        ["ok", "cached", "ok", "ok"].map(String::from),
        "an entry serves its own session only"
    );
    assert_eq!(stats()["entries"], 3);

    // room for one entry: the next put keeps only the newest
    let one = stats()["bytes"].as_u64().unwrap() / 3;
    write(
        &home.join(".config/legionctl/cache.jsonc"),
        &json!({"cache": {"tools": {"max_bytes": one + one / 2}}}).to_string(),
    );
    assert_eq!(call(Some("token-a")), "cached", "a's entry is still there");
    write(&repo.join("README.md"), "changed\n");
    assert_eq!(
        call(Some("token-b")),
        "cached",
        "no workspace rule: the git state is not part of this key"
    );
    let after = call(None);
    assert_eq!(after, "cached");
    // a fourth caller's put shrinks the namespace to its newest entry
    add_session_token(&home, "token-c", "s-c");
    assert_eq!(call(Some("token-c")), "ok");
    assert_eq!(stats()["entries"], 1, "{}", stats());
    assert_eq!(call(Some("token-a")), "ok", "the least recently used went");

    assert!(
        op.ok(&["cache", "purge", "--ns", "tools"])
            .contains("entries removed from tools")
    );
    assert_eq!(stats()["entries"], 0);
}

/// A session's token in the test operator's database, as `session start` issues it (only its hash is kept).
fn add_session_token(home: &Path, token: &str, session: &str) {
    let added = Command::new("python3")
        .args([
            "-c",
            "import hashlib, sqlite3, sys; c = sqlite3.connect(sys.argv[1]); \
             c.execute('INSERT INTO tokens VALUES (?, ?, 0)', \
             (hashlib.blake2b(sys.argv[2].encode(), digest_size=32).hexdigest(), sys.argv[3])); c.commit()",
        ])
        .arg(home.join(".local/state/legionctl/operator.db"))
        .args([token, session])
        .status()
        .unwrap();
    assert!(added.success());
}

/// A git repository with tag v1: a server that notes the network it sees, then serves legionctl's own MCP tools;
/// configured as package `probe` and as MCP server `probe` allowing only `load`.
fn probe_package(home: &Path) {
    let source = home.join("probe-server");
    std::fs::create_dir_all(&source).unwrap();
    write(
        &source.join("server.sh"),
        // /proc/net/dev follows the reader's network namespace (/sys/class/net would show the host's)
        "cut -d: -f1 /proc/net/dev | tail -n +3 | tr -d ' ' > \"$HOME/net.txt\"\nexec legionctl mcp\n",
    );
    for args in [
        &["init", "--quiet"][..],
        &["add", "."],
        &["commit", "--quiet", "-m", "v1"],
        &["tag", "v1"],
    ] {
        git(home, &source, args);
    }
    write(
        &home.join(".config/legionctl/store.jsonc"),
        &json!({
            "packages": {"probe": {"kind": "git", "source": source, "version": "v1", "run": ["sh", "{dir}/server.sh"]}},
            "mcp": {"servers": {"probe": {"package": "probe", "allow": ["load"]}}}
        })
        .to_string(),
    );
}

/// Start `legionctl <args>` as an agent would start an MCP server, and list its tools.
fn mcp_tools(op: &Operator, home: &Path, args: &[String]) -> Vec<String> {
    use rmcp::ServiceExt;
    let exe = Path::new(env!("CARGO_BIN_EXE_legionctl"));
    let path = path_with_build();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut proxy = tokio::process::Command::new(exe);
        proxy
            .args(args)
            .env("XDG_RUNTIME_DIR", &op.runtime)
            .env("HOME", home)
            .env("PATH", &path);
        let agent =
            ().serve(rmcp::transport::TokioChildProcess::new(proxy).unwrap())
                .await
                .unwrap();
        agent
            .peer()
            .list_all_tools()
            .await
            .unwrap()
            .into_iter()
            .map(|t| t.name.to_string())
            .collect()
    })
}

/// A package from the store behind the proxy: fetched and pinned, run from the store without network by the command
/// that `session start` wrote, offering only the allowed tool; a changed store file is reported and stops the next
/// session before anything is bound.
#[test]
fn a_store_package_runs_pinned_and_offline_and_a_changed_one_is_refused() {
    let (op, home) = operator("store");
    let repo = repository(&home);
    let env = [("HOME", home.to_str().unwrap())];
    let cli = |args: &[&str]| op.run(&repo, &env, args, "");
    probe_package(&home);
    assert!(String::from_utf8_lossy(&cli(&["packages", "list"]).stdout).contains("not fetched"));
    let fetched = cli(&["packages", "fetch", "probe"]);
    assert!(
        fetched.status.success(),
        "{}",
        String::from_utf8_lossy(&fetched.stderr)
    );
    assert!(String::from_utf8_lossy(&cli(&["packages", "list"]).stdout).contains(" ok "));

    let started = cli(&["session", "start", "claude", "--dossier", "port", "--json"]);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let id = serde_json::from_slice::<Value>(&started.stdout).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let mcp: Value = serde_json::from_slice(
        &std::fs::read(op.runtime.join(format!("legionctl/sessions/{id}/mcp.json"))).unwrap(),
    )
    .unwrap();
    let args: Vec<String> = mcp["mcpServers"]["probe"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().into())
        .collect();
    assert!(
        args.contains(&"--no-network".into()) && args.iter().any(|a| a.contains("store/probe/v1")),
        "{args:?}"
    );
    assert_eq!(mcp_tools(&op, &home, &args), ["load"]);
    assert_eq!(
        std::fs::read_to_string(home.join("net.txt")).unwrap(),
        "lo\n",
        "the server has no network"
    );

    // a changed store file: reported, and the next session stops before it binds a dossier
    write(
        &home.join(".local/share/legionctl/store/probe/v1/server.sh"),
        "exec legionctl mcp # changed\n",
    );
    let listed = cli(&["packages", "list"]);
    assert!(
        !listed.status.success()
            && String::from_utf8_lossy(&listed.stdout).contains("CHANGED since fetch")
    );
    let refused = cli(&["session", "start", "claude", "--dossier", "other"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("package probe changed since it was fetched"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    assert!(!String::from_utf8_lossy(&cli(&["dossier", "locks"]).stdout).contains("other"));
}

/// `~/kit` with skills alpha and beta, and a skill of the user's own in `~/.claude/skills`.
fn skills_kit(home: &Path) {
    let kit = home.join("kit");
    write(
        &kit.join("skills/alpha/SKILL.md"),
        "---\nname: alpha\n---\nALPHA_BODY\n",
    );
    write(&kit.join("skills/beta/SKILL.md"), "---\nname: beta\n---\n");
    write(
        &home.join(".claude/skills/mine/SKILL.md"),
        "the user's own\n",
    );
}

/// Package `kit` (the folder `~/kit`) and agent `probe`, which lists both skills folders, reads skill alpha and
/// tries to write into it; `session.skills` as given.
fn configure_skills(home: &Path, skills: &Value) {
    let probe = "ls \"$HOME/.claude/skills\" \"$HOME/.codex/skills\"; cat \"$HOME/.claude/skills/alpha/SKILL.md\"; \
                 touch \"$HOME/.claude/skills/alpha/x\" 2>/dev/null || echo SKILL_READONLY";
    write(
        &home.join(".config/legionctl/session.jsonc"),
        &json!({
            "packages": {"kit": {"kind": "path", "source": home.join("kit")}},
            "session": {"agents": {"probe": ["sh", "-c", probe]}, "writable": [], "skills": skills}
        })
        .to_string(),
    );
}

/// Skills from the store: the session's agents see exactly the configured skills (Claude Code's and Codex's skills
/// folders), read-only, and not the user's own; an entry that is not a hash-checked skill stops the session first.
#[test]
fn session_skills_are_exactly_the_configured_store_skills_read_only() {
    let (op, home) = operator("skills");
    let repo = repository(&home);
    let env = [("HOME", home.to_str().unwrap())];
    let cli = |args: &[&str]| op.run(&repo, &env, args, "");
    skills_kit(&home);
    let configure = |skills: Value| configure_skills(&home, &skills);
    let start = || cli(&["session", "start", "probe", "--dossier", "skills", "--json"]);
    configure(json!(["kit/skills/alpha"]));
    let not_fetched = start();
    assert!(String::from_utf8_lossy(&not_fetched.stderr).contains("package kit is not fetched"));
    assert!(cli(&["packages", "fetch", "kit"]).status.success());
    for (skills, error) in [
        (json!(["kit"]), "session.skills: kit has no SKILL.md"),
        (json!(["kit/../kit/skills/alpha"]), "leaves its package"),
        (json!(["nope/alpha"]), "session.skills: no package nope"),
        (
            json!(["kit/skills/alpha", "kit/skills/alpha"]),
            "two skills are named alpha",
        ),
    ] {
        configure(skills);
        let refused = start();
        assert!(
            !refused.status.success() && String::from_utf8_lossy(&refused.stderr).contains(error),
            "{error}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }

    configure(json!(["kit/skills/alpha"]));
    let started = start();
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let out = run_launch(
        &op,
        &home,
        &serde_json::from_slice(&started.stdout).unwrap(),
    );
    let text = String::from_utf8_lossy(&out.stdout);
    let listed: Vec<&str> = text
        .lines()
        .filter(|l| !l.is_empty() && !l.ends_with(':'))
        .take(2)
        .collect();
    assert_eq!(
        listed,
        ["alpha", "alpha"],
        "{text}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.contains("ALPHA_BODY") && text.contains("SKILL_READONLY") && !text.contains("mine"),
        "{text}"
    );
    assert!(
        home.join(".claude/skills/mine/SKILL.md").exists(),
        "the user's own skill stays outside"
    );

    // a changed store file stops the next session before it binds a dossier
    write(
        &home
            .join(".local/share/legionctl/store/kit")
            .read_dir()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path()
            .join("skills/alpha/SKILL.md"),
        "changed\n",
    );
    let refused = cli(&["session", "start", "probe", "--dossier", "other"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("package kit changed since it was fetched"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
}

/// A headless run through an ACP adapter (a stand-in agent): it gets the session's MCP servers at session/new, its
/// permission ask is answered and logged, it works inside the same sandbox and git gate as an interactive session,
/// and its answer comes back.
#[test]
fn a_headless_run_drives_an_acp_agent_in_the_session_sandbox() {
    let (op, home) = operator("run");
    let repo = repository(&home);
    let agent =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-acp-agent.py");
    write(
        &home.join(".config/legionctl/run.jsonc"),
        &json!({"session": {"acp": {"fake": ["python3", agent]}, "scope": false, "writable": []}})
            .to_string(),
    );
    let env = [("HOME", home.to_str().unwrap())];
    let out = op.run(
        &repo,
        &env,
        &[
            "session",
            "run",
            "fake",
            "fix the bug",
            "--dossier",
            "port",
            "--json",
        ],
        "",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let run: Value = serde_json::from_slice(&out.stdout).unwrap();
    let text = run["text"].as_str().unwrap();
    for expected in [
        "prompt: fix the bug",
        "mcp: legionctl",
        "permission: allow",
        "COMMIT_OK",
        "PUSH_REFUSED",
    ] {
        assert!(text.contains(expected), "{expected} missing:\n{text}");
    }
    assert_eq!(run["permissions"], json!(["git push (Execute): allowed"]));
    let workspace = home.join("worktrees/repo/port");
    assert_eq!(
        git(&home, &workspace, &["log", "-1", "--format=%s"]),
        "headless",
        "the commit is in the workspace"
    );
    let events: Value =
        serde_json::from_str(&op.ok(&["events", "--limit", "1000", "--json"])).unwrap();
    assert!(
        events["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "session.output"
                && e["payload"]["tool"] == "git push"
                && e["payload"]["session"] == run["id"]),
        "the tool call is an output event: {events}"
    );
    // the CLI shows the answer while the run goes on: the stand-in holds after its first line until it is released
    let mut again = Command::new(env!("CARGO_BIN_EXE_legionctl"))
        .args(["session", "run", "fake", "WAIT go on", "--dossier", "port"])
        .current_dir(&repo)
        .env_clear()
        .env("XDG_RUNTIME_DIR", &op.runtime)
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", &home)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let (lines, seen) = std::sync::mpsc::channel();
    let out = BufReader::new(again.stdout.take().unwrap());
    std::thread::spawn(move || {
        out.lines()
            .map_while(Result::ok)
            .try_for_each(|l| lines.send(l))
    });
    let first = seen.recv_timeout(std::time::Duration::from_secs(20));
    write(&workspace.join("release"), "");
    assert_eq!(
        first.as_deref(),
        Ok("prompt: WAIT go on"),
        "the first line of the answer comes before the run ends"
    );
    let done = again.wait_with_output().unwrap();
    let rest = seen.iter().collect::<Vec<_>>().join("\n");
    assert!(
        done.status.success(),
        "a run that ended releases its dossier: {}",
        String::from_utf8_lossy(&done.stderr)
    );
    assert!(
        !rest.contains("prompt:") && rest.contains("PUSH_REFUSED") && rest.ends_with(" ms"),
        "each line once, then the result: {rest}"
    );
}

/// A repository whose own image (its Dockerfile) has Python, with the stand-in ACP agent in it (a container sees only
/// the mounted folders), and a config for sessions in a container with the static client of `make static` and the
/// gateway on `gateway` in front of the stand-in provider on `provider`; the interactive agent `probe` tries what an
/// agent would, also a model call to the gateway at 127.0.0.1, as its base URL would say on the host.
fn container_repository(home: &Path, (gateway, provider): (u16, u16)) -> PathBuf {
    let probe = "echo x > note.txt && git add note.txt && git commit -qm probe && echo COMMIT_OK; \
                 git push origin main 2>&1 | grep -q gates.git.deny && echo PUSH_REFUSED; \
                 touch \"$HOME/outside\" && touch .agentworkspace/MEMORY/probe.md && echo WROTE; \
                 [ -f /.dockerenv ] && echo IN_CONTAINER; echo \"SESSION=$LEGIONCTL_SESSION\"; \
                 grep -q logged-in \"$HOME/.agent-state.json\" && echo STATE_READ; python3 ask-gateway.py";
    let client =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/static/release/legionctl");
    assert!(
        client.is_file(),
        "{} is missing: run `make static`",
        client.display()
    );
    let repo = repository(home);
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-acp-agent.py"),
        repo.join("fake-acp-agent.py"),
    )
    .unwrap();
    write(
        &repo.join("Dockerfile"),
        "FROM public.ecr.aws/docker/library/python:3.12-slim\n",
    );
    write(
        &repo.join("ask-gateway.py"),
        &format!(
            "import json, urllib.request as u\n\
             body = json.dumps({{'model': 'in-sandbox', 'max_tokens': 5, 'messages': [{{'role': 'user', 'content': 'hi'}}]}})\n\
             headers = {{'x-api-key': 'caller-key', 'anthropic-version': '2023-06-01', 'content-type': 'application/json'}}\n\
             r = u.urlopen(u.Request('http://127.0.0.1:{gateway}/v1/messages', body.encode(), headers), timeout=20)\n\
             print('GATEWAY_' + str(r.status))\n"
        ),
    );
    git(home, &repo, &["add", "."]);
    git(home, &repo, &["commit", "--quiet", "-m", "agent and image"]);
    // a writable file of the home, like ~/.claude.json
    let state = home.join(".agent-state.json");
    write(&state, "{\"logged-in\": true}");
    write(
        &home.join(".config/legionctl/run.jsonc"),
        &json!({"session": {
            "acp": {"fake": ["python3", "fake-acp-agent.py"]},
            "agents": {"probe": ["sh", "-c", probe]},
            "scope": false,
            "writable": [state],
            "container": {"base": "unused", "npm": [], "client": client},
        },
        "gateway": {
            "port": gateway,
            "accounts": {"main": {"provider": "anthropic", "base_url": format!("http://127.0.0.1:{provider}/ok")}},
            "pool": {"*": ["main"]},
        }})
        .to_string(),
    );
    repo
}

/// The stand-in provider and the gateway's port for `container_repository`; the operator must start again to serve
/// the gateway of the config.
fn gateway_ports(home: &Path) -> (provider::Kill, (u16, u16)) {
    let port = provider::free_port();
    (
        provider::provider(port, &home.join("provider.log")),
        (provider::free_port(), port),
    )
}

/// The calls that reached the stand-in provider through the gateway from inside a sandbox.
fn sandbox_calls(home: &Path) -> usize {
    std::fs::read_to_string(home.join("provider.log"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with("ok in-sandbox caller-key"))
        .count()
}

/// W5 (note 18): `workspace pack` puts the agent layer on the repository's own image (its Dockerfile, built by I1),
/// and sessions `--in container` run there, headless and interactive (through the real client): the workspace, its
/// git data and the dossier are mounted at their own paths, so the agent's commits land in the workspace on the host;
/// the image's `/usr/bin/git` is the static client, which asks the operator through the mounted socket and refuses
/// the push; the home is private; the sessions are recorded as containers'. Without a packed image, a run says how
/// to make one. Needs docker and `make static`.
#[test]
fn sessions_in_a_container_work_on_the_mounted_workspace_behind_the_git_gate() {
    let (op, home) = docker_operator("container");
    let (_provider, ports) = gateway_ports(&home);
    let repo = container_repository(&home, ports);
    drop(op);
    let op = operator_with("container2", &home, &docker());
    let run = |args: &[&str]| op.run(&repo, &[("HOME", home.to_str().unwrap())], args, "");
    let words = |line: &'static str| line.split(' ').collect::<Vec<_>>();
    let in_container = words("session run fake fix --dossier port --in container --json");
    let image = "localhost/repo/legionctl-agent:latest";
    let rmi = || {
        std::process::Command::new("docker")
            .args(["rmi", "-f", image])
            .output()
    };
    let _ = rmi();

    let unpacked = run(&in_container);
    assert!(
        String::from_utf8_lossy(&unpacked.stderr)
            .contains("make it with `legionctl workspace pack`"),
        "{}",
        String::from_utf8_lossy(&unpacked.stderr)
    );
    let packed = run(&["workspace", "pack"]);
    assert!(
        String::from_utf8_lossy(&packed.stdout)
            .contains(&format!("packed {image} from localhost/repo/repo:")),
        "the agent layer goes on the repository's own image: {}",
        String::from_utf8_lossy(&packed.stderr)
    );

    let out = run(&in_container);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = serde_json::from_slice::<Value>(&out.stdout).unwrap()["text"].to_string();
    for expected in [
        "prompt: fix",
        "inside: container",
        "COMMIT_OK",
        "PUSH_REFUSED",
    ] {
        assert!(text.contains(expected), "{expected} missing:\n{text}");
    }
    assert_eq!(
        git(
            &home,
            &home.join("worktrees/repo/port"),
            &["log", "-1", "--format=%s"]
        ),
        "headless",
        "the commit is in the workspace on the host"
    );

    let started = run(&words("session start probe --dossier talk --in container"));
    let text = String::from_utf8_lossy(&started.stdout);
    for expected in [
        "COMMIT_OK",
        "PUSH_REFUSED",
        "WROTE",
        "IN_CONTAINER",
        "SESSION=",
        "STATE_READ",
        "GATEWAY_200",
    ] {
        assert!(
            text.contains(expected),
            "{expected} missing:\n{text}\n{}",
            String::from_utf8_lossy(&started.stderr)
        );
    }
    assert!(
        !home.join("outside").exists(),
        "the container's home is its own"
    );
    assert_eq!(
        sandbox_calls(&home),
        1,
        "the model call from the container went through the host's gateway"
    );
    let workspace = home.join("worktrees/repo/talk");
    assert_eq!(
        git(&home, &workspace, &["log", "-1", "--format=%s"]),
        "probe"
    );
    assert!(
        workspace.join(".agentworkspace/MEMORY/probe.md").exists(),
        "the dossier is mounted"
    );

    let sessions: Value =
        serde_json::from_slice(&run(&["session", "list", "--json"]).stdout).unwrap();
    for session in sessions.as_array().unwrap() {
        assert_eq!(
            (&session["inside"], &session["state"]),
            (&json!("container"), &json!("inactive")),
            "a container's session, and the container is gone: {sessions}"
        );
    }
    let _ = rmi();
}

/// The Debian 13 cloud image of the VM tests and its SHA512SUMS, downloaded once into /var/tmp/legionctl-test-cache.
fn debian_cloud_image() -> PathBuf {
    let cache = Path::new("/var/tmp/legionctl-test-cache");
    let url = "https://cloud.debian.org/images/cloud/trixie/latest";
    let file = "debian-13-genericcloud-amd64.qcow2";
    std::fs::create_dir_all(cache).unwrap();
    for name in [file, "SHA512SUMS"] {
        if !cache.join(name).is_file() {
            let got = Command::new("curl")
                .args(["-fsSL", "--retry", "3", "-o"])
                .arg(cache.join(name))
                .arg(format!("{url}/{name}"))
                .status()
                .unwrap();
            assert!(got.success(), "downloading {url}/{name}");
        }
    }
    cache.join(file)
}

/// A repository for sessions in a VM (`container_repository` with its gateway, the operator started again to serve
/// it) and a config whose VM base is the Debian cloud image.
fn vm_repository(
    tag: &'static str,
    again: &'static str,
) -> (Operator, PathBuf, PathBuf, provider::Kill) {
    let image = debian_cloud_image();
    let (op, home) = operator(tag);
    let (provider, ports) = gateway_ports(&home);
    let repo = container_repository(&home, ports);
    drop(op);
    let op = operator_in(again, &home);
    let virtiofsd = ["/usr/lib/virtiofsd", "/usr/libexec/virtiofsd"]
        .into_iter()
        .find(|p| Path::new(p).exists())
        .expect("virtiofsd");
    write(
        &home.join(".config/legionctl/vm.jsonc"),
        &json!({"session": {"vm": {
            "base": format!("file://{}", image.display()),
            "disk_gib": 8, "memory_mib": 2048, "cpus": 2, "virtiofsd": virtiofsd,
        }}})
        .to_string(),
    );
    (op, home, repo, provider)
}

/// W5 part 3 (note 18): `workspace pack --as vm` makes the repository's disk from a cloud image (checked against its
/// SHA512SUMS) in one cloud-init boot, and sessions `--in vm` run on a throwaway layer over it, headless and through
/// the real client: the workspace, its git data and the dossier are virtiofs shares at their own paths, so commits
/// land in the workspaces on the host; the disk's `/usr/bin/git` is the static client, which reaches the operator
/// through the guest's relay and QEMU's forward, and refuses the push. Boots VMs: needs KVM, QEMU, virtiofsd,
/// xorriso, `make static` and the Debian cloud image (downloaded once into /var/tmp/legionctl-test-cache).
#[test]
#[ignore = "boots VMs (KVM, QEMU, virtiofsd, a cloud image): CI runs it in its own step"]
fn sessions_in_a_vm_work_on_the_shared_workspace_behind_the_git_gate() {
    let (op, home, repo, _provider) = vm_repository("vm", "vm2");
    let run = |args: &[&str]| op.run(&repo, &[("HOME", home.to_str().unwrap())], args, "");

    let packed = run(&["workspace", "pack", "--as", "vm"]);
    assert!(
        String::from_utf8_lossy(&packed.stdout).contains("legionctl/vm/repo.qcow2 from file://"),
        "{}{}",
        String::from_utf8_lossy(&packed.stdout),
        String::from_utf8_lossy(&packed.stderr)
    );
    let words = |line: &'static str| line.split(' ').collect::<Vec<_>>();
    let gpu = run(&words("session run fake fix --dossier port --in vm --gpu"));
    assert!(
        String::from_utf8_lossy(&gpu.stderr).contains(
            "has no NVIDIA driver: make it with `legionctl workspace pack --as vm --gpu`"
        ),
        "{}",
        String::from_utf8_lossy(&gpu.stderr)
    );
    let out = run(&words("session run fake fix --dossier port --in vm --json"));
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = serde_json::from_slice::<Value>(&out.stdout).unwrap()["text"].to_string();
    for expected in ["prompt: fix", "inside: vm", "COMMIT_OK", "PUSH_REFUSED"] {
        assert!(text.contains(expected), "{expected} missing:\n{text}");
    }
    assert_eq!(
        git(
            &home,
            &home.join("worktrees/repo/port"),
            &["log", "-1", "--format=%s"]
        ),
        "headless",
        "the commit is in the workspace on the host"
    );

    let started = run(&words("session start probe --dossier talk --in vm"));
    let text = String::from_utf8_lossy(&started.stdout);
    for expected in [
        "COMMIT_OK",
        "PUSH_REFUSED",
        "WROTE",
        "SESSION=",
        "STATE_READ",
        "GATEWAY_200",
    ] {
        assert!(
            text.contains(expected),
            "{expected} missing:\n{text}\n{}",
            String::from_utf8_lossy(&started.stderr)
        );
    }
    assert!(!home.join("outside").exists(), "the VM's home is its own");
    assert_eq!(
        sandbox_calls(&home),
        1,
        "the model call from the VM went through the host's gateway"
    );
    let talk = home.join("worktrees/repo/talk");
    assert_eq!(git(&home, &talk, &["log", "-1", "--format=%s"]), "probe");
    assert!(
        talk.join(".agentworkspace/MEMORY/probe.md").exists(),
        "the dossier is shared"
    );
    let left = Command::new("pgrep")
        .args(["-af", &format!("qemu-system.*{}", home.display())])
        .output()
        .unwrap();
    assert!(
        left.stdout.is_empty(),
        "the VMs end with their sessions: {}",
        String::from_utf8_lossy(&left.stdout)
    );
}

/// Jobs (DESIGN/02): a headless run is a job; when the operator stops in the middle of it, the next operator goes on
/// with it in the same session (its ACP session loaded, told that it was cut off) and records how it ended.
#[test]
fn a_headless_run_cut_off_by_a_restart_goes_on_in_its_session() {
    let (op, home) = operator("jobs");
    let repo = repository(&home);
    let agent =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-acp-agent.py");
    write(
        &home.join(".config/legionctl/run.jsonc"),
        &json!({"session": {"acp": {"fake": ["python3", agent]}, "scope": false, "writable": []}})
            .to_string(),
    );
    // a long run (the stand-in waits 30 s on SLOW), from a client that stays connected
    let mut client = Command::new(env!("CARGO_BIN_EXE_legionctl"))
        .args([
            "session",
            "run",
            "fake",
            "SLOW port the parser",
            "--dossier",
            "port",
        ])
        .current_dir(&repo)
        .env_clear()
        .env("XDG_RUNTIME_DIR", &op.runtime)
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", &home)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let job = |op: &Operator| -> Value {
        let jobs: Value = serde_json::from_str(&op.ok(&["jobs", "list", "--json"])).unwrap();
        jobs["items"]
            .as_array()
            .unwrap()
            .first()
            .cloned()
            .unwrap_or(Value::Null)
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    // running, in an ACP session the agent made
    while !job(&op)["input"]["acp"].is_string() {
        assert!(std::time::Instant::now() < deadline, "no running job");
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
    drop(op);
    let _ = client.kill();
    let _ = client.wait();

    let again = Operator::start(
        "jobs2",
        &[
            ("HOME", home.clone().into()),
            ("LEGIONCTL_SYSROOT", home.join("root").into()),
            ("PATH", path_with_keyring(&home)),
        ],
    );
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    let ended = loop {
        let now = job(&again);
        if now["state"] != "running" {
            break now;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the job did not end: {now}"
        );
        std::thread::sleep(std::time::Duration::from_millis(300));
    };
    assert_eq!(
        (
            ended["kind"].as_str(),
            ended["state"].as_str(),
            ended["attempts"].as_u64()
        ),
        (Some("session.run"), Some("done"), Some(2)),
        "{ended}"
    );
    let shown: Value =
        serde_json::from_str(&again.ok(&["jobs", "show", &ended["id"].to_string(), "--json"]))
            .unwrap();
    let text = shown["result"]["text"].as_str().unwrap();
    assert!(
        text.contains("prompt: A restart of legionctl cut off your work")
            && text.contains("SLOW port the parser"),
        "{text}"
    );
    assert_eq!(
        shown["result"]["id"], shown["input"]["session"],
        "the same session went on"
    );
    // in its ACP session: loaded, and its replayed history is not part of the answer
    let acp = shown["input"]["acp"].as_str().unwrap();
    assert!(
        text.contains(&format!("acp: loaded {acp}")) && !text.contains("REPLAYED"),
        "{text}"
    );
}

/// Offload (S4 policy, decided 2026-10-13): with `session.policy.offload_headless`, a headless run stops while memory
/// is under pressure (available memory below the `load.busy` share, here from the machine's /proc/meminfo) and goes on
/// in its session when memory eases.
#[test]
fn a_headless_run_waits_out_memory_pressure() {
    let (op, home) = operator("offload");
    let repo = repository(&home);
    let agent =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-acp-agent.py");
    write(
        &home.join(".config/legionctl/run.jsonc"),
        &json!({"session": {"acp": {"fake": ["python3", agent]}, "scope": false, "writable": [],
                            "policy": {"idle_freeze_minutes": 0, "offload_headless": true}}})
        .to_string(),
    );
    let meminfo = home.join("root/proc/meminfo");
    let memory = |available_kib: u64| {
        write(
            &meminfo,
            &format!("MemTotal: 16000000 kB\nMemAvailable: {available_kib} kB\n"),
        );
    };
    memory(12_000_000);
    let client = Command::new(env!("CARGO_BIN_EXE_legionctl"))
        .args([
            "session",
            "run",
            "fake",
            "SLOW port the parser",
            "--dossier",
            "port",
        ])
        .current_dir(&repo)
        .env_clear()
        .env("XDG_RUNTIME_DIR", &op.runtime)
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", &home)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let state = || -> Value {
        let jobs: Value = serde_json::from_str(&op.ok(&["jobs", "list", "--json"])).unwrap();
        jobs["items"][0].clone()
    };
    let wait_for = |want: &str, seconds: u64| -> Value {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        loop {
            let job = state();
            if job["state"] == want {
                return job;
            }
            assert!(std::time::Instant::now() < deadline, "no {want} job: {job}");
            std::thread::sleep(std::time::Duration::from_millis(300));
        }
    };
    wait_for("running", 15);
    memory(800_000);
    wait_for("offloaded", 25);
    let out = client.wait_with_output().unwrap();
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("offloaded: memory is under pressure"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    memory(12_000_000);
    let done = wait_for("done", 30);
    assert_eq!(done["attempts"], 2, "{done}");
    assert!(
        done["result"]["text"]
            .as_str()
            .unwrap()
            .contains("prompt: A restart of legionctl cut off your work")
    );
}

/// Callers and their tokens (DESIGN/03): `{mcp}` in the agent's command is the session's MCP config; a session's
/// sandbox has its own token and cannot read the user's; without its
/// token it is denied (a token it makes up is not the user's); and a call that names another session still acts for
/// the sandbox's own session, so another session's dossier stays where it is.
#[test]
fn a_sandbox_acts_only_as_its_own_session() {
    let (op, home) = operator("tokens");
    let repo = repository(&home);
    let cli = env!("CARGO_BIN_EXE_legionctl");
    let probe = format!(
        "grep -q mcpServers \"$0\" && echo mcp-config; \
         cat \"$XDG_RUNTIME_DIR/legionctl.token\" >/dev/null 2>&1 && echo token-visible || echo token-hidden; \
         env -u LEGIONCTL_TOKEN {cli} dossier list >/dev/null 2>/tmp/err && echo user-allowed; \
         grep -o 'the token is not valid' /tmp/err; \
         LEGIONCTL_SESSION=victim {cli} dossier release"
    );
    write(
        &home.join(".config/legionctl/session.jsonc"),
        &json!({"session": {"agents": {"probe": ["sh", "-c", probe, "{mcp}"]}, "scope": false, "writable": []}}).to_string(),
    );
    let env = [("HOME", home.to_str().unwrap())];
    let victim = op.run(
        &repo,
        &[
            ("HOME", home.to_str().unwrap()),
            ("LEGIONCTL_SESSION", "victim"),
        ],
        &["dossier", "use", "loot"],
        "",
    );
    assert!(
        victim.status.success(),
        "{}",
        String::from_utf8_lossy(&victim.stderr)
    );
    let started = op.run(
        &repo,
        &env,
        &["session", "start", "probe", "--dossier", "box", "--json"],
        "",
    );
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let launch: Value = serde_json::from_slice(&started.stdout).unwrap();
    let out = run_launch(&op, &home, &launch);
    let said = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        said.lines().take(3).collect::<Vec<_>>(),
        ["mcp-config", "token-hidden", "the token is not valid"],
        "{said}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        said.contains("released box"),
        "the release acted for the sandbox's own session: {said}"
    );

    let locks: Value = serde_json::from_slice(
        &op.run(&repo, &env, &["dossier", "locks", "--json"], "")
            .stdout,
    )
    .unwrap();
    let held: Vec<(&str, &str)> = locks
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            (
                l["dossier"].as_str().unwrap(),
                l["session"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        held,
        [("loot", "victim")],
        "the other session keeps its dossier"
    );
}

/// Resume: an ended session starts again with the same id, dossier and workspace through the agent's resume command;
/// an agent without one cannot resume.
#[test]
fn an_ended_session_resumes_with_its_id_dossier_and_workspace() {
    let (op, home) = operator("resume");
    let repo = repository(&home);
    let report = "echo \"$0 SESSION=$LEGIONCTL_SESSION DIR=$(pwd)\"";
    write(
        &home.join(".config/legionctl/session.jsonc"),
        &json!({"session": {
            "agents": {"probe": ["sh", "-c", report, "first"], "noresume": ["true"]},
            "resume": {"probe": ["sh", "-c", report, "again"]},
            "scope": false,
            "writable": []
        }})
        .to_string(),
    );
    let env = [("HOME", home.to_str().unwrap())];
    let launch = |args: &[&str]| -> Value {
        let out = op.run(&repo, &env, &[args, &["--json"]].concat(), "");
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let first = launch(&["session", "start", "probe", "--dossier", "port"]);
    let id = first["id"].as_str().unwrap();
    let workspace = home.join("worktrees/repo/port");
    let expected = |word: &str| format!("{word} SESSION={id} DIR={}\n", workspace.display());
    assert_eq!(
        String::from_utf8_lossy(&run_launch(&op, &home, &first).stdout),
        expected("first")
    );
    // the start says the dossier is new, and where the session works
    let lines = |l: &Value| -> Vec<String> { serde_json::from_value(l["lines"].clone()).unwrap() };
    let said = lines(&first);
    assert!(said[0].starts_with("created "), "{said:?}");
    assert!(
        said.iter().any(|l| l.starts_with(&format!(
            "workspace: {} on legionctl/port (base main), ",
            workspace.display()
        ))),
        "{said:?}"
    );
    // the folder's last dossier is held by the live session: a start without a name says what to do
    let held = op.run(&repo, &env, &["session", "start", "probe"], "");
    let why = String::from_utf8_lossy(&held.stderr);
    assert!(
        why.contains(&format!("'port' is in use by session {id}"))
            && why.contains("--dossier <name>"),
        "{why}"
    );

    let again = launch(&["session", "resume", id]);
    assert_eq!(again["id"], first["id"]);
    let said = lines(&again);
    assert!(
        said[0].starts_with("active dossier: port")
            && !said.iter().any(|l| l.starts_with("created")),
        "{said:?}"
    );
    assert_eq!(
        String::from_utf8_lossy(&run_launch(&op, &home, &again).stdout),
        expected("again")
    );

    let other = launch(&["session", "start", "noresume", "--dossier", "other"]);
    let refused = op.run(
        &repo,
        &env,
        &["session", "resume", other["id"].as_str().unwrap()],
        "",
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("agent noresume cannot resume a session")
    );
}

/// A plain folder (not a repository): the session works on it through an overlay, so the folder stays as it was
/// until merge; status lists additions, edits, deletions and a replaced folder; merge applies them; close drops the
/// layer.
#[test]
fn a_plain_folder_workspace_keeps_the_source_until_merge() {
    let (op, home) = operator("folder");
    let notes = home.join("notes");
    for (file, text) in [
        ("keep", "k\n"),
        ("gone", "g\n"),
        ("edit", "e\n"),
        ("sub/old", "o\n"),
    ] {
        write(&notes.join(file), text);
    }
    let work = "rm gone; echo more >> edit; echo n > new; rm -r sub; mkdir sub; echo f > sub/fresh";
    write(
        &home.join(".config/legionctl/folder.jsonc"),
        &json!({
            "session": {"agents": {"probe": ["sh", "-c", work]}, "scope": false, "writable": []},
            "workspace": {"root": home.join("layers")}
        })
        .to_string(),
    );
    let started = op.run(
        &notes,
        &[("HOME", home.to_str().unwrap())],
        &["session", "start", "probe", "--dossier", "notes", "--json"],
        "",
    );
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let launch: Value = serde_json::from_slice(&started.stdout).unwrap();
    let out = run_launch(&op, &home, &launch);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let read = |file: &str| std::fs::read_to_string(notes.join(file)).ok();
    assert_eq!(
        (read("gone"), read("edit"), read("new")),
        (Some("g\n".into()), Some("e\n".into()), None),
        "the source is as it was"
    );

    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", launch["id"].as_str().unwrap()),
    ];
    let ok = |args: &[&str]| {
        let o = op.run(&notes, &env, args, "");
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    let status: Value = serde_json::from_str(&ok(&["workspace", "status", "--json"])).unwrap();
    assert_eq!(
        status["files"],
        json!(["M edit", "D gone", "A new", "R sub/"])
    );
    assert!(ok(&["workspace", "checkpoint", "first pass"]).contains("4 changes"));
    ok(&["workspace", "merge"]);
    assert_eq!(
        (read("gone"), read("edit"), read("new")),
        (None, Some("e\nmore\n".into()), Some("n\n".into()))
    );
    assert_eq!(
        (read("sub/old"), read("sub/fresh")),
        (None, Some("f\n".into())),
        "the replaced folder"
    );
    assert_eq!(read("keep").as_deref(), Some("k\n"));
    ok(&["workspace", "close"]);
    assert!(
        !home.join("layers/notes/notes").exists(),
        "the layer is gone"
    );
    ended_scope_frees_the_dossier(&op, &home, &notes, launch["id"].as_str().unwrap());
}

/// The work graph from a dossier's files: plan rows and artifacts with front matter; `dossier check` finds a link
/// to a row that does not exist, an acceptance row without EVIDENCE, and EVIDENCE whose code changed after it was
/// written; `dossier graph` shows a row with what supports it.
#[test]
fn dossier_check_finds_dangling_links_unverified_acceptance_and_stale_evidence() {
    let (op, home) = operator("graph");
    let repo = repository(&home);
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "s1"),
    ];
    let ok = |args: &[&str]| {
        let o = op.run(&repo, &env, args, "");
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    ok(&["dossier", "use", "port"]);
    ok(&["workspace", "open"]);
    let dossier = home.join(".agentworkspace/port");
    let plan = std::fs::read_to_string(dossier.join("plan.md")).unwrap().replace(
        "## Acceptance Criteria\n",
        "## Acceptance Criteria\n| id | criterion | status |\n| --- | --- | --- |\n\
         | acceptance-1 | builds | done |\n| acceptance-2 | tests pass | done |\n| acceptance-3 | docs | pending |\n",
    );
    write(&dossier.join("plan.md"), &plan);
    let artifact = |file: &str, front: &str| {
        write(
            &dossier.join(file),
            &format!("---\n{front}\n---\n# {file}\n"),
        );
    };
    artifact(
        "EVIDENCE/validation/01-build.md",
        "id: EV-01\nsupports: [plan#acceptance-1]\ncode: [README.md]",
    );
    artifact(
        "EVIDENCE/validation/02-tests.md",
        "id: EV-02\nsupports: [plan#acceptance-2]",
    );
    artifact("MEMORY/01-note.md", "supports: [plan#acceptance-9]");
    // a plan row that names an artifact by path reaches it by its id
    let plan = std::fs::read_to_string(dossier.join("plan.md"))
        .unwrap()
        .replace(
            "| acceptance-2 | tests pass | done |",
            "| acceptance-2 | tests pass (EVIDENCE/validation/02-tests.md) | done |",
        );
    write(&dossier.join("plan.md"), &plan);
    // EV-01 was written long before README.md's last commit
    std::fs::File::options()
        .write(true)
        .open(dossier.join("EVIDENCE/validation/01-build.md"))
        .unwrap()
        .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_hours(262_968))
        .unwrap();

    let check: Value = serde_json::from_str(&ok(&["dossier", "check", "--json"])).unwrap();
    assert_eq!(
        check["problems"],
        json!([
            "MEMORY/01-note supports plan#acceptance-9, which does not exist",
            "plan#acceptance-3 has no EVIDENCE that supports it",
            "EV-01 is stale: README.md changed after it was written"
        ])
    );
    let around: Value = serde_json::from_str(&ok(&[
        "dossier",
        "graph",
        "--node",
        "plan#acceptance-2",
        "--json",
    ]))
    .unwrap();
    assert_eq!(
        around["edges"],
        json!([
            {"from": "EV-02", "to": "plan#acceptance-2", "kind": "supports"},
            {"from": "plan#acceptance-2", "to": "EV-02", "kind": "refers-to"}
        ]),
        "the plan row names the artifact by path and reaches its id"
    );
    assert_eq!(around["nodes"].as_array().unwrap().len(), 2);
}

/// The work graph keeps each status change made through legionctl (write, edit, add), with the session, oldest
/// first; an edit that keeps the status adds nothing. `dossier graph --node` shows a plan row's and an artifact's.
#[test]
fn dossier_graph_keeps_each_status_change_with_its_session() {
    let (op, home) = operator("history");
    let repo = repository(&home);
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "s1"),
    ];
    let ok = |args: &[&str], stdin: &str| {
        let o = op.run(&repo, &env, args, stdin);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    ok(&["dossier", "use", "hist"], "");
    let plan = "# Plan\n\n## ToDo\n| id | item | status |\n| --- | --- | --- |\n| todo-1 | build it | planned |\n";
    ok(&["dossier", "write", "hist/plan.md", "--overwrite"], plan);
    // the row's anchor, then a new text for it
    let set_row = |row: &str| {
        let read = ok(&["dossier", "read", "hist/plan.md"], "");
        let anchor = read
            .lines()
            .find(|l| l.contains("| todo-1 |"))
            .and_then(|l| l.split('|').next())
            .unwrap()
            .to_string();
        ok(
            &["dossier", "edit", "hist/plan.md", "replace", &anchor],
            row,
        );
    };
    set_row("| todo-1 | build it | in-progress |\n");
    set_row("| todo-1 | build it well | in-progress |\n");
    set_row("| todo-1 | build it well | done: abc123 |\n");
    let note = ok(
        &["dossier", "add", "MEMORY", "note"],
        "---\nid: MEM-note\nstatus: pending\n---\n# Note\n",
    );
    let note = note.trim().trim_start_matches("hist/").to_string();
    let read = ok(&["dossier", "read", &format!("hist/{note}")], "");
    let anchor = read
        .lines()
        .nth(2)
        .unwrap()
        .split('|')
        .next()
        .unwrap()
        .to_string();
    ok(
        &[
            "dossier",
            "edit",
            &format!("hist/{note}"),
            "replace",
            &anchor,
        ],
        "status: done\n",
    );

    let history = |node: &str| -> Vec<(String, Value, Value)> {
        let graph: Value =
            serde_json::from_str(&ok(&["dossier", "graph", "--node", node, "--json"], "")).unwrap();
        graph["history"]
            .as_array()
            .unwrap()
            .iter()
            .map(|h| {
                (
                    h["session"].as_str().unwrap().into(),
                    h["from"].clone(),
                    h["to"].clone(),
                )
            })
            .collect()
    };
    let s1 = || String::from("s1");
    assert_eq!(
        history("plan#todo-1"),
        [
            (s1(), Value::Null, json!("planned")),
            (s1(), json!("planned"), json!("in-progress")),
            (s1(), json!("in-progress"), json!("done: abc123")),
        ]
    );
    assert_eq!(
        history("MEM-note"),
        [
            (s1(), Value::Null, json!("pending")),
            (s1(), json!("pending"), json!("done"))
        ]
    );
}

/// Plugins: only what `plugins.marketplaces` allows is installed, through `claude plugin install` from the
/// configured source and never with `-y`; the list marks installed plugins outside the allowlist. A stand-in `claude`
/// logs its arguments.
#[test]
fn plugin_installs_follow_the_allowlist() {
    let home = Path::new("/var/tmp").join(format!("legionctl-e2e-plugins-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    let bin = home.join("bin");
    write(
        &bin.join("claude"),
        &format!(
            "#!/bin/sh\necho \"$*\" >> {}\nif [ \"$2\" = list ]; then echo '[{{\"id\":\"fmt@acme\",\"version\":\"1.0.0\",\"scope\":\"user\",\
             \"enabled\":true}},{{\"id\":\"spy@elsewhere\",\"version\":\"0.1.0\",\"scope\":\"project\",\"enabled\":false}}]'; \
             else echo '{{\"ok\":true}}'; fi\n",
            home.join("claude.log").display()
        ),
    );
    std::fs::set_permissions(
        bin.join("claude"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    write(
        &home.join(".config/legionctl/plugins.jsonc"),
        &json!({"plugins": {"marketplaces": {
            "acme": {"source": "acme/tools", "allow": ["fmt"]},
            "open": {"source": "open/all", "allow": ["*"]}
        }}})
        .to_string(),
    );
    let path =
        std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&path_with_build())))
            .unwrap();
    let op = Operator::start("plugins", &[("HOME", home.clone().into()), ("PATH", path)]);
    let run = |args: &[&str]| op.run(&home, &[("HOME", home.to_str().unwrap())], args, "");

    for (id, why) in [
        ("lint@acme", "plugin lint is not allowed from acme"),
        (
            "fmt@other",
            "marketplace other is not in plugins.marketplaces",
        ),
        ("fmt", "write it as <plugin>@<marketplace>"),
        // a name that claude would read as its own flag (-y accepts a marketplace's command)
        ("-y@open", "write it as <plugin>@<marketplace>"),
    ] {
        let refused = run(&["agents", "plugins", "install", "--", id, "user"]);
        assert!(
            !refused.status.success() && String::from_utf8_lossy(&refused.stderr).contains(why),
            "{id}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
    // a repository's own config layer cannot widen the allowlist
    let repo = repository(&home);
    write(
        &repo.join(".legionctl/plugins.jsonc"),
        &json!({"plugins": {"marketplaces": {"evil": {"source": "evil/repo", "allow": ["*"]}}}})
            .to_string(),
    );
    let from_repo = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &["agents", "plugins", "install", "x@evil", "user"],
        "",
    );
    assert!(
        String::from_utf8_lossy(&from_repo.stderr)
            .contains("marketplace evil is not in plugins.marketplaces"),
        "{}",
        String::from_utf8_lossy(&from_repo.stderr)
    );
    assert!(
        !home.join("claude.log").exists(),
        "a refusal never reaches claude"
    );
    let installed = run(&["agents", "plugins", "install", "fmt@acme", "project-local"]);
    assert!(
        installed.status.success(),
        "{}",
        String::from_utf8_lossy(&installed.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(home.join("claude.log")).unwrap(),
        "plugin install fmt --marketplace acme/tools --scope local --json\n"
    );

    let listed: Value =
        serde_json::from_slice(&run(&["agents", "plugins", "list", "--json"]).stdout).unwrap();
    let marks: Vec<(&str, bool)> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|p| (p["id"].as_str().unwrap(), p["allowed"].as_bool().unwrap()))
        .collect();
    assert_eq!(marks, [("fmt@acme", true), ("spy@elsewhere", false)]);
}

/// Agent settings: a dry run shows only the changed lines and writes nothing; a write keeps a backup; TOML keeps its
/// comments; an unknown top-level key is written with a warning.
#[test]
fn agent_settings_edit_with_a_diff_and_a_backup() {
    let (op, home) = operator("agentset");
    let env = [("HOME", home.to_str().unwrap())];
    let ok = |args: &[&str]| {
        let o = op.run(&home, &env, args, "");
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        serde_json::from_slice::<Value>(&o.stdout).unwrap()
    };
    let claude = home.join(".claude/settings.json");
    write(
        &claude,
        "{\n  \"model\": \"opus\",\n  \"permissions\": {\n    \"allow\": [\"Read\"]\n  }\n}\n",
    );
    let original = std::fs::read_to_string(&claude).unwrap();
    let set = [
        "agents",
        "set",
        "claude",
        "user",
        "permissions.defaultMode",
        "acceptEdits",
        "--json",
    ];
    let dry = ok(&[&set[..], &["--dry-run"]].concat());
    // the JSON is written pretty: a one-line list becomes lines (TOML keeps its formatting)
    assert_eq!(
        dry["changes"],
        json!([
            "-     \"allow\": [\"Read\"]",
            "+     \"allow\": [",
            "+       \"Read\"",
            "+     ],",
            "+     \"defaultMode\": \"acceptEdits\""
        ])
    );
    assert_eq!(
        std::fs::read_to_string(&claude).unwrap(),
        original,
        "a dry run writes nothing"
    );
    let done = ok(&set);
    assert_eq!(
        std::fs::read_to_string(done["backup"].as_str().unwrap()).unwrap(),
        original
    );
    let now: Value = serde_json::from_str(&std::fs::read_to_string(&claude).unwrap()).unwrap();
    assert_eq!(
        now["permissions"],
        json!({"allow": ["Read"], "defaultMode": "acceptEdits"})
    );
    assert_eq!(
        ok(&["agents", "set", "claude", "user", "modle", "x", "--json"])["warnings"][0],
        "modle is not a top-level key legionctl knows; check the spelling"
    );

    let codex = home.join(".codex/config.toml");
    write(&codex, "# my settings\nmodel = \"gpt\"\n");
    let edit = ok(&[
        "agents",
        "set",
        "codex",
        "user",
        "mcp_servers.ctx.command",
        "ctx",
        "--json",
    ]);
    assert_eq!(
        edit["changes"],
        json!(["+ ", "+ [mcp_servers.ctx]", "+ command = \"ctx\""])
    );
    assert!(
        std::fs::read_to_string(&codex)
            .unwrap()
            .starts_with("# my settings\nmodel = \"gpt\"\n")
    );
}

/// Usage counts each Claude Code message once (its usage repeats on every content block) and each Codex turn, with
/// Codex's rate-limit use.
#[test]
fn agent_usage_from_transcripts() {
    let (op, home) = operator("agentuse");
    let env = [("HOME", home.to_str().unwrap())];
    let ok = |args: &[&str]| {
        let o = op.run(&home, &env, args, "");
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        serde_json::from_slice::<Value>(&o.stdout).unwrap()
    };
    let message = |id: &str, text: &str| {
        json!({"type": "assistant", "timestamp": "2026-10-13T01:00:00Z", "message": {"id": id, "model": "claude-x",
            "content": [{"type": "text", "text": text}],
            "usage": {"input_tokens": 3, "output_tokens": 7, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 5}}})
        .to_string()
    };
    write(
        &home.join(".claude/projects/p/s.jsonl"),
        &[message("m1", "a"), message("m1", "b"), message("m2", "c")].join("\n"),
    );
    let turn = |input: u64, used: f64| {
        json!({"timestamp": "2026-10-13T02:00:00Z", "type": "event_msg", "payload": {"type": "token_count",
            "info": {"last_token_usage": {"input_tokens": input, "cached_input_tokens": 1, "output_tokens": 2}},
            "rate_limits": {"primary": {"used_percent": used, "window_minutes": 300}}}})
        .to_string()
    };
    let context = json!({"timestamp": "2026-10-13T02:00:00Z", "type": "turn_context", "payload": {"model": "gpt-x"}});
    write(
        &home.join(".codex/sessions/2026/10/13/r.jsonl"),
        &[context.to_string(), turn(10, 3.0), turn(20, 4.0)].join("\n"),
    );
    let usage = ok(&["agents", "usage", "--json"]);
    assert_eq!(
        usage["rows"],
        json!([
            {"agent": "claude", "day": "2026-10-13", "model": "claude-x", "input": 6, "output": 14, "cache_read": 200, "cache_write": 10},
            {"agent": "codex", "day": "2026-10-13", "model": "gpt-x", "input": 30, "output": 4, "cache_read": 2, "cache_write": 0}
        ])
    );
    assert_eq!(usage["codex_limits"], json!({"primary (300 min)": 4.0}));
}

/// Images: a repository's Dockerfiles become the services and cache descriptions: the last stage is the target,
/// external bases only (not earlier stages, not scratch), ENTRYPOINT + CMD as run, HEALTHCHECK CMD as health,
/// continuation lines joined, the shell form through sh, and the lockfiles in each build context.
#[test]
fn images_scan_describes_services_and_their_build_cache() {
    let (op, home) = operator("images");
    let repo = home.join("app");
    write(
        &repo.join("Dockerfile"),
        "FROM rust:1.90 AS build\nCOPY . .\nRUN cargo build --release\n\n# the runtime\nFROM debian:13-slim AS runtime\n\
         COPY --from=build /target/release/api /app/api\nEXPOSE 8080\n\
         HEALTHCHECK --interval=30s CMD [\"/app/api\", \"--check\"]\nENTRYPOINT [\"/app/api\"]\nCMD [\"--serve\"]\n",
    );
    write(&repo.join("Cargo.lock"), "");
    write(
        &repo.join("services/web/Dockerfile"),
        "FROM node:22\nRUN npm ci \\\n    && npm run build\nEXPOSE 3000/tcp 9229\nCMD npm start\n",
    );
    write(&repo.join("services/web/package-lock.json"), "{}");
    write(
        &repo.join("tools/lint.Dockerfile"),
        "FROM scratch\nCOPY lint /lint\n",
    );
    let out = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &["images", "scan", "--write", "--json"],
        "",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let scan: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        scan["services"],
        json!({
            "app": {"build": {"context": ".", "dockerfile": "Dockerfile", "target": "runtime"},
                    "entrypoints": {"health": ["/app/api", "--check"], "run": ["/app/api", "--serve"]}, "ports": ["8080"]},
            "lint": {"build": {"context": "tools", "dockerfile": "tools/lint.Dockerfile", "target": null}, "entrypoints": {}, "ports": []},
            "web": {"build": {"context": "services/web", "dockerfile": "services/web/Dockerfile", "target": null},
                    "entrypoints": {"run": ["/bin/sh", "-c", "npm start"]}, "ports": ["3000/tcp", "9229"]}
        })
    );
    assert_eq!(
        scan["cache"],
        json!({
            "app": {"bases": ["rust:1.90", "debian:13-slim"], "lockfiles": ["Cargo.lock"]},
            "lint": {"bases": [], "lockfiles": []},
            "web": {"bases": ["node:22"], "lockfiles": ["services/web/package-lock.json"]}
        })
    );
    let written: Value = serde_json::from_str(
        &std::fs::read_to_string(repo.join(".legionctl/docker-compose-services.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(written, scan["services"]);
}

/// After an upgrade the running operator is the old build: an operation it does not know is a version mismatch that
/// says to restart it, while the same request from a client of its own build is not found; both in the error
/// envelope.
#[test]
fn an_unknown_operation_from_another_build_says_to_restart_the_operator() {
    let (op, _home) = operator("build");
    op.ok(&["load"]);
    let token = std::fs::read_to_string(op.runtime.join(legionctl_api::USER_TOKEN)).unwrap();
    let ask = |fingerprint: &str| {
        let mut stream =
            std::os::unix::net::UnixStream::connect(op.runtime.join("legionctl/api.sock")).unwrap();
        let request = format!(
            "POST /v1/ops/packages.newer HTTP/1.1\r\nhost: l\r\nconnection: close\r\ncontent-type: application/json\r\n\
             x-legionctl-api: {fingerprint}\r\nauthorization: Bearer {token}\r\ncontent-length: 2\r\n\r\n{{}}"
        );
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        std::io::Read::read_to_string(&mut stream, &mut response).unwrap();
        response
    };
    // the status line, and the error envelope of the body
    let answer = |response: String| -> (String, Value) {
        let (head, body) = response.split_once("\r\n\r\n").unwrap();
        let body: Value = serde_json::from_str(body).unwrap();
        (
            head.lines().next().unwrap().to_string(),
            body["error"].clone(),
        )
    };
    let (status, error) = answer(ask("0000000000000000"));
    assert!(status.contains("412"), "{status}");
    assert_eq!(
        (error["code"].as_str(), error["op"].as_str()),
        (Some("version_mismatch"), Some("packages.newer"))
    );
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("restart it (systemctl --user restart legionctl-operator)"),
        "{error}"
    );
    let (status, error) = answer(ask(legionctl_api::api_fingerprint()));
    assert!(status.contains("404"), "{status}");
    assert_eq!(
        (error["code"].as_str(), error["message"].as_str()),
        (Some("not_found"), Some("unknown operation packages.newer"))
    );
}

/// Removes a test's image when the test ends, a failed one too, so no run leaves images on the machine.
struct RemoveImage(String);

impl Drop for RemoveImage {
    fn drop(&mut self) {
        let _ = Command::new("docker").args(["rmi", "-f", &self.0]).output();
    }
}

/// Images with the real Docker (CI has it; no registry pull: the image builds `FROM scratch`): build tags it
/// `localhost/<repo>/<service>:<commit>`, run writes the compose file and passes compose's refusal on (a scratch
/// image has no command), status lists the repository's containers.
#[test]
fn images_build_tags_by_commit_and_run_reports_what_compose_says() {
    let (op, home) = docker_operator("imgbuild");
    let repo = repository(&home);
    write(
        &repo.join("svc/Dockerfile"),
        "FROM scratch\nCOPY note /note\nHEALTHCHECK CMD [\"/check\"]\n",
    );
    write(&repo.join("svc/note"), "x\n");
    git(&home, &repo, &["add", "."]);
    git(&home, &repo, &["commit", "--quiet", "-m", "svc"]);
    let commit = git(&home, &repo, &["rev-parse", "--short", "HEAD"]);
    let env = [("HOME", home.to_str().unwrap())];
    let built = op.run(&repo, &env, &["images", "build"], "");
    assert!(
        built.status.success(),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    let image = format!("localhost/repo/svc:{commit}");
    let _remove = RemoveImage(image.clone());
    assert_eq!(
        String::from_utf8_lossy(&built.stdout).trim(),
        format!("built {image}")
    );
    assert!(
        Command::new("docker")
            .args(["image", "inspect", &image])
            .output()
            .unwrap()
            .status
            .success()
    );

    let run = op.run(&repo, &env, &["images", "run"], "");
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        !run.status.success() && stderr.contains("docker compose up"),
        "{stderr}"
    );
    let compose: Value = serde_json::from_str(
        &std::fs::read_to_string(repo.join(".legionctl/compose.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        compose,
        json!({"name": "repo", "services": {"svc": {
            "image": image, "build": {"context": "svc", "dockerfile": "svc/Dockerfile"},
            "healthcheck": {"test": ["CMD", "/check"]}
        }}})
    );
    let _ = Command::new("docker")
        .args(["compose", "-p", "repo", "-f"])
        .arg(repo.join(".legionctl/compose.json"))
        .arg("down")
        .output();
    let status: Value = serde_json::from_slice(
        &op.run(&repo, &env, &["images", "status", "--json"], "")
            .stdout,
    )
    .unwrap();
    assert_eq!(status, json!([]), "down removed the container");
}

/// A repository with a compose file: service `backend` builds `services/api` (named `api` by its folder), with
/// published ports, a bind and a named volume, limits, a GPU and a shell health check over the Dockerfile's; `db` is
/// a prebuilt image that must not be pulled; `.env` holds a secret for the environment.
fn compose_repo(home: &Path) -> PathBuf {
    let repo = repository(home);
    write(
        &repo.join("services/api/Dockerfile"),
        "FROM scratch\nCOPY note /note\nEXPOSE 8080\nHEALTHCHECK CMD [\"/check\"]\n",
    );
    write(&repo.join("services/api/note"), "x\n");
    write(&repo.join("config/api.toml"), "");
    // a Dockerfile that no compose service builds: not a service of this project
    write(&repo.join("tools/lint.Dockerfile"), "FROM scratch\n");
    write(&repo.join(".env"), "API_SECRET=hunter2\n");
    write(
        &repo.join("compose.yaml"),
        "services:\n  backend:\n    build:\n      context: services/api\n    ports: [\"18080:8080\", \"9000\"]\n\
         \x20   expose: [\"7000\"]\n    volumes: [\"./config:/etc/api:ro\", \"data:/data\"]\n    depends_on: [db]\n\
         \x20   environment:\n      SECRET: ${API_SECRET}\n    healthcheck:\n      test: [\"CMD-SHELL\", \"true\"]\n\
         \x20   deploy:\n      resources:\n        limits: {cpus: \"0.5\", memory: 256M}\n\
         \x20       reservations:\n          devices:\n            - capabilities: [gpu]\n\
         \x20 db:\n    image: postgres:17\n    pull_policy: never\nvolumes:\n  data: {}\n",
    );
    git(home, &repo, &["add", "."]);
    git(home, &repo, &["commit", "--quiet", "-m", "compose"]);
    repo
}

/// Images of a compose project (the real `docker compose config` reads it): compose names the services and adds the
/// prebuilt image, ports, mounts, limits and dependencies over the Dockerfile scan; the environment's secret is not
/// in the description or the written files.
#[test]
fn images_scan_reads_the_compose_file_but_not_its_environment() {
    let (op, home) = docker_operator("compose");
    let repo = compose_repo(&home);
    let out = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &["images", "scan", "--write", "--json"],
        "",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let scan: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(
        scan["services"],
        json!({
            "backend": {
                "build": {"context": "services/api", "dockerfile": "services/api/Dockerfile", "target": null},
                "entrypoints": {"health": ["/bin/sh", "-c", "true"]},
                "ports": ["18080:8080", "9000", "7000"],
                "mounts": [
                    {"kind": "bind", "source": "config", "target": "/etc/api", "read_only": true},
                    {"kind": "volume", "source": "data", "target": "/data", "read_only": false}
                ],
                "resources": {"cpus": 0.5, "memory_bytes": 268_435_456, "gpu": true},
                "depends_on": ["db"]
            },
            "db": {"build": null, "image": "postgres:17", "entrypoints": {}, "ports": []}
        })
    );
    assert_eq!(
        scan["cache"],
        json!({"backend": {"bases": [], "lockfiles": []}})
    );
    assert_eq!(scan["compose_file"], "compose.yaml");
    let written =
        std::fs::read_to_string(repo.join(".legionctl/docker-compose-services.json")).unwrap();
    assert!(
        !String::from_utf8_lossy(&out.stdout).contains("hunter2") && !written.contains("hunter2"),
        "the environment stays out"
    );
}

/// `images run` of a compose project passes the repository's file first and legionctl's file second, which only
/// sets the built tag of each built service; the prebuilt image is not built.
#[test]
fn images_run_of_a_compose_project_overrides_only_the_built_tags() {
    let (op, home) = docker_operator("composerun");
    let repo = compose_repo(&home);
    let commit = git(&home, &repo, &["rev-parse", "--short", "HEAD"]);
    let env = [("HOME", home.to_str().unwrap())];
    let built = op.run(&repo, &env, &["images", "build"], "");
    let image = format!("localhost/repo/backend:{commit}");
    let _remove = RemoveImage(image.clone());
    assert_eq!(
        String::from_utf8_lossy(&built.stdout).trim(),
        format!("built {image}"),
        "{}",
        String::from_utf8_lossy(&built.stderr)
    );
    // db comes from the repository's file and may not be pulled: compose refuses it, and the refusal comes back
    let run = op.run(&repo, &env, &["images", "run"], "");
    assert!(
        !run.status.success()
            && String::from_utf8_lossy(&run.stderr).contains("No such image: postgres:17"),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let override_file: Value = serde_json::from_str(
        &std::fs::read_to_string(repo.join(".legionctl/compose.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        override_file,
        json!({"name": "repo", "services": {"backend": {"image": image}}})
    );
    let _ = Command::new("docker")
        .current_dir(&repo)
        .args([
            "compose",
            "-p",
            "repo",
            "-f",
            "compose.yaml",
            "-f",
            ".legionctl/compose.json",
            "down",
            "--volumes",
        ])
        .output();
}

/// Run a `session start` launch as the client does, without the systemd scope (CI has no user systemd): the
/// sandbox, this binary bound over git as the gate, then the agent.
fn run_launch(op: &Operator, home: &Path, launch: &Value) -> std::process::Output {
    let strings = |v: &Value| -> Vec<String> {
        v.as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_string())
            .collect()
    };
    let gate = [
        "--ro-bind",
        env!("CARGO_BIN_EXE_legionctl"),
        "/usr/bin/git",
        "--setenv",
        "LEGIONCTL_REAL_GIT",
        launch["real_git"].as_str().unwrap(),
    ]
    .map(String::from);
    let argv = [
        strings(&launch["sandbox"]),
        gate.to_vec(),
        strings(&launch["agent"]),
    ]
    .concat();
    Command::new(&argv[0])
        .args(&argv[1..])
        .env_clear()
        .env("HOME", home)
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("XDG_RUNTIME_DIR", &op.runtime)
        .output()
        .unwrap()
}

#[test]
fn a_session_runs_the_agent_in_a_sandbox_where_the_git_gate_and_read_only_home_hold() {
    let (op, home) = operator("session");
    let repo = repository(&home);
    // a stand-in agent that tries what an agent would: commit, push, rewrite history, write home and dossier
    let probe = "echo x > note.txt && git add note.txt && git commit -qm probe && echo COMMIT_OK; \
                 git push origin main 2>&1 | grep -q gates.git.deny && echo PUSH_REFUSED; \
                 git reset --hard HEAD 2>&1 | grep -q gates.git.deny && echo RESET_REFUSED; \
                 touch \"$HOME/outside\" 2>/dev/null || echo HOME_READONLY; \
                 touch .agentworkspace/MEMORY/probe.md && echo DOSSIER_WRITABLE; echo \"SESSION=$LEGIONCTL_SESSION\"; \
                 echo \"SETTINGS=$(cat \"$1\")\"; \
                 [ -e \"$XDG_RUNTIME_DIR/bus\" ] || echo BUS_HIDDEN; [ -S \"$HOME/decoy.sock\" ] || echo SOCKET_HIDDEN; \
                 [ -e /dev/nvidiactl ] && echo NVIDIA_NODES || echo NO_GPU";
    // a session bus in the runtime folder, and a listening socket like docker's: neither may reach the agent
    write(&op.runtime.join("bus"), "");
    let _decoy = std::os::unix::net::UnixListener::bind(home.join("decoy.sock")).unwrap();
    write(
        &home.join(".config/legionctl/session.jsonc"),
        &json!({"session": {
            "agents": {"probe": ["sh", "-c", probe, "probe", "{settings}"]},
            "writable": [],
            "hidden": [home.join("decoy.sock")]
        }})
        .to_string(),
    );
    let started = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &["session", "start", "probe", "--dossier", "port", "--json"],
        "",
    );
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let launch: Value = serde_json::from_slice(&started.stdout).unwrap();
    let out = run_launch(&op, &home, &launch);
    let text = String::from_utf8_lossy(&out.stdout);
    for expected in [
        "COMMIT_OK",
        "PUSH_REFUSED",
        "RESET_REFUSED",
        "HOME_READONLY",
        "DOSSIER_WRITABLE",
        "BUS_HIDDEN",
        "SOCKET_HIDDEN",
        "NO_GPU",
    ] {
        assert!(
            text.contains(expected),
            "{expected} missing:\n{text}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(!home.join("outside").exists());
    let id = launch["id"].as_str().unwrap();
    // the session id, and layer 1 of the gates: the git gate's refusals as the agent's own deny rules, in the sandbox
    let settings = r#"SETTINGS={"permissions":{"deny":["Bash(git push:*)","Bash(git fetch:*)""#;
    assert!(
        text.contains(&format!("SESSION={id}")) && text.contains(settings),
        "{text}"
    );
    let workspace = home.join("worktrees/repo/port");
    assert_eq!(
        git(&home, &workspace, &["log", "-1", "--format=%s"]),
        "probe",
        "the commit is on the dossier branch"
    );
    // the session is recorded, and its id holds the dossier (Claude Code gets it as --session-id)
    let active = op.run(
        &repo,
        &[("LEGIONCTL_SESSION", id)],
        &["dossier", "active"],
        "",
    );
    assert!(String::from_utf8_lossy(&active.stdout).contains("active dossier port"));
    let listed = op.run(&repo, &[], &["session", "list"], "");
    assert!(String::from_utf8_lossy(&listed.stdout).contains(id));
    gpu_on_request(&op, &home, &repo);
}

/// `--gpu` gives a host session the NVIDIA device nodes (the host's driver), which a session without it does not see;
/// a machine without them refuses the start with the reason.
fn gpu_on_request(op: &Operator, home: &Path, repo: &Path) {
    let args = [
        "session",
        "start",
        "probe",
        "--dossier",
        "gpu",
        "--gpu",
        "--json",
    ];
    let started = op.run(repo, &[("HOME", home.to_str().unwrap())], &args, "");
    if Path::new("/dev/nvidiactl").exists() {
        let launch: Value = serde_json::from_slice(&started.stdout).unwrap();
        let out = run_launch(op, home, &launch);
        assert!(String::from_utf8_lossy(&out.stdout).contains("NVIDIA_NODES"));
    } else {
        let refused = String::from_utf8_lossy(&started.stderr);
        assert!(
            refused.contains("no NVIDIA device nodes (/dev/nvidia*)"),
            "{refused}"
        );
    }
}

/// A session continues on another account: `session resume --account` copies Claude Code's transcript into that
/// profile and runs the agent there, and the session keeps the new account; an unknown profile is refused.
#[test]
fn a_resumed_session_continues_on_another_account_with_its_transcript() {
    let (op, home) = operator("switch");
    let repo = repository(&home);
    let env = [("HOME", home.to_str().unwrap())];
    let cli = |args: &[&str]| op.run(&repo, &env, args, "");
    // the agent appends a turn to its transcript; the resumed one reads it back and says where it runs
    let transcript = "$CLAUDE_CONFIG_DIR/projects/p/$LEGIONCTL_SESSION.jsonl";
    write(
        &home.join(".config/legionctl/session.jsonc"),
        &json!({"session": {
            // the agent also plants links to a file outside, which the copy must not follow
            "agents": {"probe": ["sh", "-c", format!("mkdir -p \"$CLAUDE_CONFIG_DIR/projects/p/$LEGIONCTL_SESSION\" && echo turn-1 >> \"{transcript}\" && \
                ln -s \"$HOME/secret\" \"$CLAUDE_CONFIG_DIR/projects/p/$LEGIONCTL_SESSION/leak\" && \
                ln -s \"$HOME/elsewhere\" \"$CLAUDE_CONFIG_DIR/projects/linked\"")]},
            "resume": {"probe": ["sh", "-c", format!("cat \"{transcript}\"; echo \"CONFIG=$CLAUDE_CONFIG_DIR\"")]},
            "writable": []
        }})
        .to_string(),
    );
    for name in ["first", "second"] {
        assert!(cli(&["agents", "accounts", "add", name]).status.success());
    }
    write(&home.join("secret"), "hidden from the sandbox\n");
    let started = cli(&[
        "session",
        "start",
        "probe",
        "--dossier",
        "switch",
        "--account",
        "first",
        "--json",
    ]);
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let launch: Value = serde_json::from_slice(&started.stdout).unwrap();
    run_launch(&op, &home, &launch);
    let id = launch["id"].as_str().unwrap();

    let refused = cli(&["session", "resume", id, "--account", "ghost"]);
    assert!(String::from_utf8_lossy(&refused.stderr).contains("no account profile ghost"));
    let resumed = cli(&["session", "resume", id, "--account", "second", "--json"]);
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let out = run_launch(
        &op,
        &home,
        &serde_json::from_slice(&resumed.stdout).unwrap(),
    );
    let second = home.join(".local/share/legionctl/accounts/second/claude");
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        format!("turn-1\nCONFIG={}\n", second.display()),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sessions: Value =
        serde_json::from_slice(&cli(&["session", "list", "--json"]).stdout).unwrap();
    assert_eq!(sessions[0]["account"], "second", "{sessions}");
    let planted = home.join(".local/share/legionctl/accounts/first/claude/projects");
    assert!(
        planted.join("linked").is_symlink() && planted.join(format!("p/{id}/leak")).is_symlink()
    );
    let copied = second.join("projects");
    assert!(
        !copied.join(format!("p/{id}/leak")).exists() && !copied.join("linked").exists(),
        "a planted link was followed"
    );
}

/// `agents accounts` sees the login in profile `work`; `agents usage` counts its transcript, not `~/.claude`.
fn profile_listed_and_counted(op: &Operator, repo: &Path) {
    let accounts: Value = serde_json::from_slice(
        &op.run(repo, &[], &["agents", "accounts", "list", "--json"], "")
            .stdout,
    )
    .unwrap();
    assert_eq!(accounts[0]["name"], "work");
    assert_eq!(
        (
            accounts[0]["claude"].as_bool(),
            accounts[0]["codex"].as_bool()
        ),
        (Some(true), Some(false))
    );
    let usage = op.run(
        repo,
        &[],
        &[
            "agents",
            "usage",
            "--account",
            "work",
            "--days",
            "100000",
            "--json",
        ],
        "",
    );
    let usage = String::from_utf8_lossy(&usage.stdout);
    assert!(usage.contains("claude-x") && usage.contains('7'), "{usage}");
}

/// Account logins rest in the keyring (A1): a login made in a profile moves into the keyring when no session of the
/// account runs; a session gets it in the profile; the token the agent refreshed goes back to the keyring, and off
/// the disk, when the operator next looks (another session's start, or the operator's start). A stand-in
/// `secret-tool` keeps entries as files.
#[test]
fn account_logins_rest_in_the_keyring_and_come_back_refreshed() {
    let home = Path::new("/var/tmp").join(format!("legionctl-e2e-keyring-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).unwrap();
    let keyring = home.join("keyring");
    let path = path_with_keyring(&home);
    std::fs::create_dir_all(home.join("root")).unwrap();
    let env = |tag: &str| {
        Operator::start(
            tag,
            &[
                ("HOME", home.clone().into()),
                ("LEGIONCTL_SYSROOT", home.join("root").into()),
                ("PATH", path.clone()),
            ],
        )
    };
    let op = env("keyring");
    let repo = repository(&home);
    // the agent prints the login it got, then "refreshes" it
    let probe = "f=\"$CLAUDE_CONFIG_DIR/.credentials.json\"; cat \"$f\"; echo; printf 'r%s' \"$(cat \"$f\")\" > \"$f\"";
    write(
        &home.join(".config/legionctl/session.jsonc"),
        &json!({"session": {"agents": {"probe": ["sh", "-c", probe]}, "writable": []}}).to_string(),
    );
    assert!(
        op.run(&repo, &[], &["agents", "accounts", "add", "work"], "")
            .status
            .success()
    );
    let login = home.join(".local/share/legionctl/accounts/work/claude/.credentials.json");
    write(&login, "login-1");
    let session = |op: &Operator, dossier: &str| -> String {
        let started = op.run(
            &repo,
            &[("HOME", home.to_str().unwrap())],
            &[
                "session",
                "start",
                "probe",
                "--dossier",
                dossier,
                "--account",
                "work",
                "--json",
            ],
            "",
        );
        assert!(
            started.status.success(),
            "{}",
            String::from_utf8_lossy(&started.stderr)
        );
        let launch: Value = serde_json::from_slice(&started.stdout).unwrap();
        String::from_utf8_lossy(&run_launch(op, &home, &launch).stdout)
            .trim()
            .to_string()
    };
    let kept = || std::fs::read_to_string(keyring.join("work_claude")).unwrap_or_default();
    assert_eq!(session(&op, "keys-1"), "login-1");
    assert_eq!(
        kept(),
        "login-1",
        "the login made in the profile went into the keyring first"
    );
    assert_eq!(
        session(&op, "keys-2"),
        "rlogin-1",
        "the next session gets the refreshed token"
    );
    assert_eq!(kept(), "rlogin-1");
    drop(op);
    // the first call starts the operator (socket activation), which settles the logins before it serves
    env("keyring2").ok(&["dossier", "list"]);
    assert!(!login.exists(), "at rest the login is not on the disk");
    assert_eq!(kept(), "rrlogin-1");
}

#[test]
fn an_account_profile_gives_the_session_its_own_agent_logins_and_usage() {
    let (op, home) = operator("account");
    let repo = repository(&home);
    // the agent "logs in" and writes a transcript where its variables point; $HOME stays read-only
    let probe = "printf '{}' > \"$CLAUDE_CONFIG_DIR/.credentials.json\" && mkdir -p \"$CLAUDE_CONFIG_DIR/projects/p\" && \
                 printf '%s\\n' \"$1\" > \"$CLAUDE_CONFIG_DIR/projects/p/t.jsonl\" && echo \"CODEX=$CODEX_HOME\"";
    let transcript = json!({"timestamp": "2026-10-13T10:00:00Z", "message": {"id": "m1", "model": "claude-x",
        "usage": {"input_tokens": 7, "output_tokens": 3}}});
    write(
        &home.join(".config/legionctl/session.jsonc"),
        &json!({"session": {"agents": {"probe": ["sh", "-c", probe, "probe", transcript.to_string()]}, "resume": {"probe": ["true"]}, "writable": []}})
            .to_string(),
    );
    let start = |account: &str| {
        op.run(
            &repo,
            &[("HOME", home.to_str().unwrap())],
            &[
                "session",
                "start",
                "probe",
                "--dossier",
                "acct",
                "--account",
                account,
                "--json",
            ],
            "",
        )
    };
    // a missing profile and a name that leaves the profiles folder are refused before any session is made
    for (account, error) in [
        ("work", "no account profile work"),
        ("..", "an account name is"),
    ] {
        let refused = start(account);
        assert!(
            !refused.status.success() && String::from_utf8_lossy(&refused.stderr).contains(error),
            "{account}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
    assert!(
        !op.run(&repo, &[], &["agents", "accounts", "add", "../x"], "")
            .status
            .success()
    );
    assert!(
        op.run(&repo, &[], &["agents", "accounts", "add", "work"], "")
            .status
            .success()
    );

    let started = start("work");
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let launch: Value = serde_json::from_slice(&started.stdout).unwrap();
    let out = run_launch(&op, &home, &launch);
    let profile = home.join(".local/share/legionctl/accounts/work");
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains(&format!("CODEX={}", profile.join("codex").display())),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        !home.join(".claude").exists(),
        "the login stayed in the profile"
    );

    profile_listed_and_counted(&op, &repo);
    // the account survives a resume
    let id = launch["id"].as_str().unwrap();
    let resumed: Value = serde_json::from_slice(
        &op.run(
            &repo,
            &[("HOME", home.to_str().unwrap())],
            &["session", "resume", id, "--json"],
            "",
        )
        .stdout,
    )
    .unwrap();
    assert!(
        resumed["sandbox"].to_string().contains("CLAUDE_CONFIG_DIR"),
        "{resumed}"
    );
}

/// `workspace pack --as vm --gpu` builds the NVIDIA open module (DKMS) for the guest's generic kernel, with the GSP
/// firmware and nvidia-smi, from Debian's non-free sections, and marks the disk for `--gpu` sessions. Slow (packages
/// and a module build), so neither CI nor the VM step runs it: run it when the guest's packages may have changed.
#[test]
#[ignore = "downloads the NVIDIA packages and builds the module in a VM: run by hand"]
fn a_vm_disk_packed_with_the_gpu_has_the_nvidia_module() {
    let (op, home, repo, _provider) = vm_repository("vmgpu", "vmgpu2");
    let packed = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &["workspace", "pack", "--as", "vm", "--gpu"],
        "",
    );
    let disk = home.join(".local/share/legionctl/vm/repo.qcow2");
    let log = std::fs::read_to_string(disk.with_extension("pack.log")).unwrap_or_default();
    assert!(
        String::from_utf8_lossy(&packed.stdout).contains("with the NVIDIA driver")
            && log.contains("updates/dkms/nvidia.ko"),
        "{}{}\n{}",
        String::from_utf8_lossy(&packed.stdout),
        String::from_utf8_lossy(&packed.stderr),
        &log[log.len().saturating_sub(3000)..]
    );
    assert!(
        disk.with_extension("gpu").exists(),
        "the disk is marked for --gpu"
    );
}

/// Run QEMU on `disk` with the read-only `iso` until the guest powers off (it may restart before); the serial console
/// goes to `log`. `test` adds a raw results disk and an ACPI table.
fn boot_until_poweroff(
    disk: &Path,
    iso: &Path,
    test: Option<(&Path, &Path)>,
    log: &Path,
    minutes: u64,
) {
    let mut qemu = Command::new("qemu-system-x86_64");
    qemu.args([
        "-machine",
        "q35,accel=kvm,smm=off",
        "-cpu",
        "host",
        "-m",
        "2048",
    ])
    .args([
        "-smp",
        "2",
        "-nodefaults",
        "-display",
        "none",
        "-monitor",
        "none",
    ])
    .arg("-serial")
    .arg(format!("file:{}", log.display()))
    .arg("-drive")
    .arg(format!("file={},if=none,id=root", disk.display()))
    // the firmware boots this disk, whatever the order of the PCI devices
    .args(["-device", "virtio-blk-pci,drive=root,bootindex=0"])
    .arg("-drive")
    .arg(format!(
        "file={},if=virtio,format=raw,readonly=on",
        iso.display()
    ))
    .args([
        "-netdev",
        "user,id=net",
        "-device",
        "virtio-net-pci,netdev=net",
    ])
    .stdin(Stdio::null());
    if let Some((results, table)) = test {
        qemu.arg("-acpitable")
            .arg(format!("file={}", table.display()))
            .arg("-drive")
            .arg(format!(
                "file={},if=none,id=results,format=raw",
                results.display()
            ))
            .args(["-device", "virtio-blk-pci,drive=results,serial=lwmiresults"]);
    }
    let mut child = qemu.spawn().expect("qemu-system-x86_64");
    let deadline = std::time::Instant::now() + std::time::Duration::from_mins(minutes);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
            let _ = child.kill();
            let console = std::fs::read_to_string(log).unwrap_or_default();
            panic!(
                "the VM ran over {minutes} min; console:\n{}",
                &console[console.len().saturating_sub(4000)..]
            );
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    };
    assert!(status.success(), "qemu-system-x86_64: {status}");
}

/// An ISO 9660 image of `dir` with the volume label `label`.
fn iso(dir: &Path, label: &str, out: &Path) {
    let made = Command::new("xorriso")
        .args(["-as", "mkisofs", "-quiet", "-V", label, "-J", "-r", "-o"])
        .arg(out)
        .arg(dir)
        .status()
        .unwrap();
    assert!(made.success(), "xorriso");
}

/// The disk of the legionctl-wmi test, made once over the cloud image and kept in the test cache: Debian's generic
/// kernel from trixie-backports (the cloud kernel has no WMI, and the module needs the WMI API of 7.x) with its
/// headers, DKMS, python3 and the unit that runs the test from the disk labelled LWMITEST, then powers off.
fn legionctl_wmi_base(image: &Path) -> PathBuf {
    // a new name when the preparation changes
    let base = image.with_file_name("legionctl-wmi-base-v1.qcow2");
    if base.exists() {
        return base;
    }
    let dir = Path::new("/var/tmp").join(format!("lc-kmod-base-{}", std::process::id()));
    let seed = dir.join("seed");
    std::fs::create_dir_all(&seed).unwrap();
    write(
        &seed.join("lwmi-test.service"),
        "[Unit]\nDescription=legionctl-wmi test\nAfter=local-fs.target systemd-udevd.service\n\n[Service]\n\
         Type=oneshot\nExecStart=/bin/sh -c 'mount -o ro LABEL=LWMITEST /mnt && bash /mnt/run.sh; \
         systemctl poweroff'\n\n[Install]\nWantedBy=multi-user.target\n",
    );
    write(
        &seed.join("meta-data"),
        "instance-id: lwmi-base\nlocal-hostname: lwmi\n",
    );
    write(
        &seed.join("user-data"),
        "#cloud-config\nruncmd:\n  - [sh, -ec, 'export DEBIAN_FRONTEND=noninteractive; \
         echo \"deb http://deb.debian.org/debian trixie-backports main\" > /etc/apt/sources.list.d/backports.list; \
         apt-get update; apt-get install -y -t trixie-backports linux-image-amd64 linux-headers-amd64; \
         apt-get install -y dkms python3; \
         apt-get purge -y \"linux-image-*-cloud-amd64\" linux-image-cloud-amd64; update-grub; \
         mount -o ro LABEL=cidata /mnt; install -m 644 /mnt/lwmi-test.service /etc/systemd/system/; umount /mnt; \
         systemctl enable lwmi-test.service; touch /etc/cloud/cloud-init.disabled; \
         echo lwmi-base-ok > /dev/ttyS0']\npower_state: {mode: poweroff, timeout: 30, condition: true}\n",
    );
    let seed_iso = dir.join("seed.iso");
    iso(&seed, "cidata", &seed_iso);
    let part = base.with_extension("part.qcow2");
    let made = Command::new("qemu-img")
        .args(["create", "-q", "-f", "qcow2", "-F", "qcow2", "-b"])
        .arg(image)
        .arg(&part)
        .arg("8G")
        .status()
        .unwrap();
    assert!(made.success(), "qemu-img create");
    let log = dir.join("base.log");
    boot_until_poweroff(&part, &seed_iso, None, &log, 25);
    let console = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        console.contains("lwmi-base-ok"),
        "the base disk was not prepared; console:\n{}",
        &console[console.len().saturating_sub(4000)..]
    );
    std::fs::rename(&part, &base).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    base
}

/// The checks of fixtures/legionctl-wmi/run.sh, in their order: each must report ok.
const LEGIONCTL_WMI_CHECKS: [&str; 46] = [
    "dkms-build",
    "only-legionctl-wmi",
    "profile-choices",
    "profile-start",
    "profile-set",
    "profile-call",
    "fw-attributes",
    "fw-bounds",
    "fw-display",
    "fw-not-custom",
    "fw-not-custom-warning",
    "fw-above-max",
    "fw-below-min",
    "fw-set",
    "fw-call",
    "fw-value-of-mode",
    "fan-rpm",
    "table-not-custom",
    "table-not-custom-warning",
    "table-set",
    "table-read",
    "table-bytes",
    "table-below-minimum",
    "table-above-10",
    "table-overflow",
    "table-decreasing",
    "table-not-a-number",
    "table-unchanged",
    "table-again-on-custom",
    "fnq-notify",
    "fnq-profile",
    "fnq-table-again",
    "full-speed-off",
    "full-speed-on",
    "full-speed-call",
    "full-speed-manual",
    "full-speed-ends",
    "full-speed-not-custom",
    "fan-max-on",
    "fan-max-call",
    "resume-fan-max",
    "resume-table",
    "led-micmute",
    "led-mute",
    "unload",
    "kernel-clean",
];

/// todo-79: the legionctl-wmi kernel module (kernel/legionctl-wmi), built by DKMS in a VM against Debian's 7.x
/// kernel and bound to a stand-in of the 82WQ firmware (fixtures/legionctl-wmi/ssdt.asl, a QEMU -acpitable) after a
/// restart with the package's blacklist in place. The guest (run.sh) uses it through sysfs only: `platform_profile`
/// and its Fn+Q notice, the firmware attributes within and outside their bounds and modes, the fan table byte for
/// byte as the firmware gets it, its refusals and its return after a mode change, Fn+Q and resume, full speed, fan
/// max mode, the mute LEDs and a clean unload.
#[test]
#[ignore = "boots VMs (KVM, QEMU, a cloud image) and builds a kernel module: CI runs it in the VM step"]
fn the_legionctl_wmi_module_gives_the_legion_wmi_through_sysfs() {
    let base = legionctl_wmi_base(&debian_cloud_image());
    let dir = Path::new("/var/tmp").join(format!("lc-kmod-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let test = dir.join("test");
    let module = test.join("legionctl-wmi");
    std::fs::create_dir_all(&module).unwrap();
    let kmod = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../kernel/legionctl-wmi");
    for name in [
        "legionctl-wmi.c",
        "firmware_attributes_class.h",
        "Kbuild",
        "Makefile",
        "dkms.conf",
    ] {
        std::fs::copy(kmod.join(name), module.join(name)).unwrap();
    }
    std::fs::copy(
        kmod.join("legionctl-wmi.conf"),
        test.join("legionctl-wmi.conf"),
    )
    .unwrap();
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/legionctl-wmi");
    std::fs::copy(fixtures.join("run.sh"), test.join("run.sh")).unwrap();
    std::fs::create_dir(test.join("aml-call")).unwrap();
    for name in ["aml_call.c", "Kbuild"] {
        std::fs::copy(
            fixtures.join("aml-call").join(name),
            test.join("aml-call").join(name),
        )
        .unwrap();
    }
    // the stand-in firmware, as QEMU gives it to the guest
    let compiled = Command::new("iasl")
        .arg("-p")
        .arg(dir.join("ssdt"))
        .arg(fixtures.join("ssdt.asl"))
        .output()
        .expect("iasl (acpica-tools)");
    assert!(
        compiled.status.success(),
        "iasl: {}",
        String::from_utf8_lossy(&compiled.stdout)
    );
    let test_iso = dir.join("test.iso");
    iso(&test, "LWMITEST", &test_iso);
    let disk = dir.join("disk.qcow2");
    let made = Command::new("qemu-img")
        .args(["create", "-q", "-f", "qcow2", "-F", "qcow2", "-b"])
        .arg(&base)
        .arg(&disk)
        .status()
        .unwrap();
    assert!(made.success(), "qemu-img create");
    let results = dir.join("results.img");
    std::fs::write(&results, vec![0u8; 1 << 20]).unwrap();
    let log = dir.join("console.log");
    boot_until_poweroff(
        &disk,
        &test_iso,
        Some((&results, &dir.join("ssdt.aml"))),
        &log,
        10,
    );

    let report = String::from_utf8_lossy(&std::fs::read(&results).unwrap())
        .trim_end_matches('\0')
        .to_string();
    let passed: Vec<&str> = report
        .lines()
        .filter_map(|l| l.strip_prefix("ok "))
        .collect();
    let console = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        passed == LEGIONCTL_WMI_CHECKS && report.contains("\ndone"),
        "results:\n{report}\nconsole:\n{}",
        &console[console.len().saturating_sub(3000)..]
    );
    let _ = std::fs::remove_dir_all(&dir);
}
