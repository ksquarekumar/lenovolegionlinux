//! End-to-end: the binary as the agent plugin uses it (hook dispatch, MCP over stdio), in an isolated HOME.

mod common;

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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

/// An operator with an empty HOME and the repo's plugin skills.
fn operator(tag: &str) -> (Operator, PathBuf) {
    // outside /tmp (private in a session sandbox) and outside any git repository, like a real home
    let home = Path::new("/var/tmp").join(format!("legionctl-e2e-{tag}-{}", std::process::id()));
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
        &json!({"name": "agents", "arguments": {"project": "some-project"}}),
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
    ok(&repo, &["dossier", "use", "port", "--create"]);

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
        (status[0]["ahead"].as_u64(), status[0]["behind"].as_u64()),
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

#[test]
fn a_session_runs_the_agent_in_a_sandbox_where_the_git_gate_and_read_only_home_hold() {
    let (op, home) = operator("session");
    let repo = repository(&home);
    // a stand-in agent that tries what an agent would: commit, push, rewrite history, write home and dossier
    let probe = "echo x > note.txt && git add note.txt && git commit -qm probe && echo COMMIT_OK; \
                 git push origin main 2>&1 | grep -q gates.git.deny && echo PUSH_REFUSED; \
                 git reset --hard HEAD 2>&1 | grep -q gates.git.deny && echo RESET_REFUSED; \
                 touch \"$HOME/outside\" 2>/dev/null || echo HOME_READONLY; \
                 touch .agentworkspace/MEMORY/probe.md && echo DOSSIER_WRITABLE; echo \"SESSION=$LEGIONCTL_SESSION\"";
    write(
        &home.join(".config/legionctl/session.jsonc"),
        &json!({"session": {"agents": {"probe": ["sh", "-c", probe]}, "writable": []}}).to_string(),
    );
    let started = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &[
            "session",
            "start",
            "probe",
            "--dossier",
            "port",
            "--create",
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
    // the client runs scope + sandbox + gate + agent; CI has no user systemd, so this runs sandbox + gate + agent
    let argv = [
        strings(&launch["sandbox"]),
        gate.to_vec(),
        strings(&launch["agent"]),
    ]
    .concat();
    let out = Command::new(&argv[0])
        .args(&argv[1..])
        .env_clear()
        .env("HOME", &home)
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("XDG_RUNTIME_DIR", &op.runtime)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    for expected in [
        "COMMIT_OK",
        "PUSH_REFUSED",
        "RESET_REFUSED",
        "HOME_READONLY",
        "DOSSIER_WRITABLE",
    ] {
        assert!(
            text.contains(expected),
            "{expected} missing:\n{text}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(!home.join("outside").exists());
    let id = launch["id"].as_str().unwrap();
    assert!(
        text.contains(&format!("SESSION={id}")),
        "the session id reaches the agent: {text}"
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
}
