//! End-to-end: threads (design 23, Part B, P5) through the real `legionctl hook` adapter with Claude Code's hook
//! payloads (recorded in tests/fixtures/claude-code-hooks), the CLI and the real operator: watch rules and their
//! counts, delivery of the outbox through the prompt, pre-tool and stop hooks, `SendMessage` in the thread log, pace,
//! an operator restart, and presence.

mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::Operator;
use serde_json::{Value, json};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// A hook payload as Claude Code sends it (`<event>-<tool>.json` for a tool event), with the test's fields over it.
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

/// An operator with its own HOME and a repository with one commit on main.
struct Lab {
    op: Operator,
    home: PathBuf,
    repo: PathBuf,
}

impl Lab {
    fn new(tag: &str) -> Lab {
        let home =
            Path::new("/var/tmp").join(format!("legionctl-thread-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
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
        write(&repo.join("README.md"), "repo\n");
        for args in [
            &["init", "--quiet"][..],
            &["add", "."],
            &["commit", "--quiet", "-m", "start"],
        ] {
            let out = Command::new("git")
                .arg("-C")
                .arg(&repo)
                .args(args)
                .env("HOME", &home)
                .output()
                .unwrap();
            assert!(
                out.status.success(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
        Lab {
            op: Self::operator(tag, &home),
            home,
            repo,
        }
    }

    fn operator(tag: &str, home: &Path) -> Operator {
        let bin = Path::new(env!("CARGO_BIN_EXE_legionctl"))
            .parent()
            .unwrap()
            .to_path_buf();
        let path: OsString = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        std::fs::create_dir_all(home.join("root")).unwrap();
        Operator::start(
            &format!("th-{tag}"),
            &[
                ("LEGIONCTL_SYSROOT", home.join("root").into()),
                ("HOME", home.into()),
                ("PATH", path),
            ],
        )
    }

    /// stdout of a CLI call of `session` in `cwd`, which must succeed.
    fn ok(&self, session: &str, cwd: &Path, args: &[&str]) -> String {
        let out = self.op.run(
            cwd,
            &[
                ("HOME", self.home.to_str().unwrap()),
                ("LEGIONCTL_SESSION", session),
            ],
            args,
            "",
        );
        assert!(
            out.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    /// stderr of a CLI call that must fail.
    fn refused(&self, session: &str, args: &[&str]) -> String {
        let out = self.op.run(
            &self.repo,
            &[
                ("HOME", self.home.to_str().unwrap()),
                ("LEGIONCTL_SESSION", session),
            ],
            args,
            "",
        );
        assert!(!out.status.success(), "{args:?} passed");
        String::from_utf8_lossy(&out.stderr).to_string()
    }

    fn json(&self, session: &str, args: &[&str]) -> Value {
        serde_json::from_str(&self.ok(session, &self.repo, &[args, &["--json"]].concat())).unwrap()
    }

    /// What `legionctl hook <event>` gives to the agent of `session` working in `cwd`.
    fn hook(&self, session: &str, cwd: &Path, event: &str, fields: Value) -> String {
        let mut fields = fields;
        fields["session_id"] = json!(session);
        fields["cwd"] = json!(cwd);
        let out = self
            .op
            .run(cwd, &[], &["hook", event], &claude_hook(event, &fields));
        assert!(
            out.stderr.is_empty(),
            "hook {event}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn edit(&self, session: &str, cwd: &Path, file: &Path) {
        let fields = json!({"tool_name": "Edit", "tool_input": {"file_path": file, "old_string": "a", "new_string": "b"}});
        assert_eq!(self.hook(session, cwd, "post-tool", fields), "");
    }

    fn bash(&self, session: &str, cwd: &Path, command: &str) {
        let fields =
            json!({"tool_name": "Bash", "tool_input": {"command": command, "description": "run"}});
        assert_eq!(self.hook(session, cwd, "post-tool", fields), "");
    }

    /// The stop hook's block reason, if it blocks.
    fn stop(&self, session: &str, again: bool) -> Option<String> {
        let out = self.hook(
            session,
            &self.repo,
            "stop",
            json!({"stop_hook_active": again}),
        );
        (!out.is_empty()).then(|| {
            let v: Value = serde_json::from_str(&out).unwrap();
            assert_eq!(v["decision"], "block", "{v}");
            v["reason"].as_str().unwrap().to_string()
        })
    }

    /// The pre-tool hook's refusal of a call of `tool`, if it refuses. The payload is the recorded Edit call with
    /// the tool's name set: the hook reads only the name, the session, the folder and a file path.
    fn pre_tool(&self, session: &str, tool: &str) -> Option<String> {
        let mut payload: Value = serde_json::from_str(&claude_hook(
            "pre-tool",
            &json!({"tool_name": "Edit", "session_id": session, "cwd": self.repo,
                    "tool_input": {"file_path": self.repo.join("README.md")}}),
        ))
        .unwrap();
        payload["tool_name"] = json!(tool);
        let out = self
            .op
            .run(&self.repo, &[], &["hook", "pre-tool"], &payload.to_string());
        let out = String::from_utf8(out.stdout).unwrap();
        (!out.trim().is_empty()).then(|| {
            let v: Value = serde_json::from_str(&out).unwrap();
            assert_eq!(v["hookSpecificOutput"]["permissionDecision"], "deny");
            v["hookSpecificOutput"]["permissionDecisionReason"]
                .as_str()
                .unwrap()
                .to_string()
        })
    }

    /// `legionctl thread <args>` of `session` in the repository.
    fn thread(&self, session: &str, args: &[&str]) -> String {
        self.ok(session, &self.repo, &[&["thread"], args].concat())
    }

    /// The wakes of a member of thread 1.
    fn wakes(&self, member: &str) -> Value {
        Self::member(&self.json("main", &["thread", "show", "1"]), member)["wakes"].clone()
    }

    fn member<'a>(view: &'a Value, name: &str) -> &'a Value {
        view["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["name"] == name)
            .unwrap()
    }
}

impl Drop for Lab {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

/// A thread on dossier port with main0 (session `main`) and `observer0` (session `obs`, a thread-observer with no
/// model: the test sends its messages).
fn thread_lab(tag: &str) -> Lab {
    let lab = Lab::new(tag);
    lab.ok("main", &lab.repo, &["dossier", "use", "port"]);
    let started = lab.thread("main", &["start", "port the parser"]);
    assert!(
        started.starts_with("thread 1 started on dossier port"),
        "{started}"
    );
    let join = [
        "join",
        "1",
        "observer0",
        "thread-observer",
        "--member-session",
        "obs",
    ];
    lab.thread("main", &join);
    lab
}

/// Main0 and a stand-in observer: a watch rule counts the edits of `crates/` with no `make lint` between them; the
/// lint sets the count back; the rule's reminder carries the count of misses since the rule started and reaches main0
/// at its next prompt exactly once; a notice of a step done for main0 counts it. Pace on-trigger: no wake for a stop
/// or a declare, one wake per firing rule; on-stop wakes at the stop with the log rows since the last wake.
#[test]
fn a_watch_rule_reminds_main_once_with_the_count_of_its_misses_and_wakes_only_on_its_trigger() {
    let lab = thread_lab("rule");
    let ws = PathBuf::from(
        lab.json("main", &["workspace", "open"])["path"]
            .as_str()
            .unwrap(),
    );
    let rule = [
        "rules",
        "add",
        "1",
        "observer0",
        "make lint",
        "--path",
        "crates/",
        "--every",
        "3",
    ];
    lab.thread("main", &rule);
    let code = ws.join("crates/core/src/lib.rs");
    let prompt = || lab.hook("main", &ws, "prompt", json!({"prompt": "go on"}));

    // two edits, the lint, two edits and an edit outside crates/: no reminder yet
    lab.edit("main", &ws, &code);
    lab.edit("main", &ws, &code);
    lab.bash("main", &ws, "cd crates && make lint");
    lab.edit("main", &ws, &code);
    lab.edit("main", &ws, &code);
    lab.edit("main", &ws, &ws.join("docs/notes.md"));
    let quiet = prompt();
    assert!(!quiet.contains("legionctl thread"), "{quiet}");
    assert_eq!(lab.stop("main", false), None, "nothing blocks the reply");
    lab.thread("main", &["declare", "tests-written"]);
    assert_eq!(
        lab.wakes("observer0"),
        0,
        "on-trigger: a stop or a declare is no trigger"
    );

    // the third edit since the lint fires the rule
    lab.edit("main", &ws, &code);
    let since = lab.json("main", &["thread", "rules", "list", "1"])[0]["created_at"]
        .as_i64()
        .unwrap();
    let clock = format!("{:02}:{:02} UTC", since % 86_400 / 3600, since % 3600 / 60);
    let told = prompt();
    let first =
        format!("observer0: no `make lint` after 3 edits of `crates/` — 1st time since {clock}");
    assert!(
        told.contains(&format!("reminder from observer0 (thread 1): {first}")),
        "{told}"
    );
    assert!(!prompt().contains("reminder"), "delivered once");
    assert_eq!(lab.wakes("observer0"), 1);

    // three more misses: the second; the observer ran the lint for main0 and says so with the counts
    for _ in 0..3 {
        lab.edit("main", &ws, &code);
    }
    lab.thread(
        "obs",
        &[
            "send",
            "main0",
            "notice",
            "ran `make lint` (clean)",
            "--rule",
            "1",
        ],
    );
    let told = prompt();
    assert!(told.contains(&format!("2nd time since {clock}")), "{told}");
    let done =
        format!("observer0: ran `make lint` (clean) — 2nd miss since {clock}, 1 done for you");
    assert!(told.contains(&done), "{told}");
    let rule = &lab.json("main", &["thread", "rules", "list", "1"])[0];
    let counts = (
        rule["misses"].as_u64(),
        rule["done"].as_u64(),
        rule["run"].as_u64(),
    );
    assert_eq!(counts, (Some(2), Some(1), Some(0)));
    assert_eq!(lab.wakes("observer0"), 2);

    // on-stop: the stop of main0 wakes it, with the log rows since its last wake
    lab.thread("main", &["pace", "1", "observer0", "on-stop"]);
    assert_eq!(lab.stop("main", false), None);
    let view = lab.json("main", &["thread", "show", "1"]);
    assert_eq!(Lab::member(&view, "observer0")["wakes"], 3);
    let log = view["log"].as_array().unwrap();
    let wake = log
        .iter()
        .rev()
        .find(|r| r["kind"] == "thread.wake")
        .unwrap();
    assert_eq!(wake["payload"]["reason"], "stop");
    assert_eq!(
        wake["payload"]["events"], 2,
        "the notice and the pace change: {wake}"
    );
}

/// A peer outside the thread and the main agent cannot block or remind; a peer sends a hint.
fn only_an_observer_of_the_thread_blocks(lab: &Lab) {
    assert!(lab.op.ok(&["thread", "list"]).contains("open    port"));
    for (session, kind) in [
        ("peer", "hard-block"),
        ("main", "hard-block"),
        ("peer", "reminder"),
    ] {
        let refused = lab.refused(
            session,
            &["thread", "send", "main0", kind, "stop", "--thread", "1"],
        );
        assert!(refused.contains("comes only from a"), "{refused}");
    }
    lab.thread(
        "peer",
        &[
            "send",
            "main0",
            "hint",
            "the parser has a fuzz corpus",
            "--thread",
            "1",
        ],
    );
}

/// Blocks and rights: a peer outside the thread and the main agent cannot block; a soft-block stops the end of one
/// reply; a hard-block stops each reply (also when the reply already goes on because of a stop hook) and refuses the
/// tool it names until main0 acks it; a member that leaves takes its blocks with it.
#[test]
fn blocks_hold_main_until_acked_and_only_an_observer_of_the_thread_sends_them() {
    let lab = thread_lab("blocks");
    only_an_observer_of_the_thread_blocks(&lab);

    // soft-block: one reply end, not when the reply goes on, not twice
    lab.thread(
        "obs",
        &[
            "send",
            "main0",
            "soft-block",
            "record the checkpoint in plan.md.",
        ],
    );
    assert_eq!(
        lab.stop("main", true),
        None,
        "a reply that goes on is not stopped by a soft-block"
    );
    let soft = lab
        .stop("main", false)
        .expect("the soft-block stops the reply end");
    assert!(
        soft.contains("soft-block #2 from observer0: record the checkpoint in plan.md."),
        "{soft}"
    );
    assert_eq!(
        lab.stop("main", false),
        None,
        "a soft-block stops one reply only"
    );

    // hard-block on the checkpoint tool: each reply end and each call of that tool, until the ack
    let tool = "mcp__plugin_legionctl_legionctl__workspace_checkpoint";
    let block_checkpoint = |text: &str| {
        lab.thread(
            "obs",
            &[
                "send",
                "main0",
                "hard-block",
                text,
                "--tool",
                "workspace_checkpoint",
            ],
        );
    };
    block_checkpoint("test fx::doubles kills no mutant.");
    let held = lab
        .pre_tool("main", tool)
        .expect("the named tool is refused");
    assert!(
        held.contains("observer0 holds") && held.contains("hard-block #3"),
        "{held}"
    );
    assert_eq!(lab.pre_tool("main", "Bash"), None, "another tool passes");
    assert_eq!(
        lab.pre_tool("other", tool),
        None,
        "another session's tool passes"
    );
    for again in [false, true, false] {
        let reason = lab
            .stop("main", again)
            .expect("a hard-block stops each reply end");
        assert!(
            reason.contains("test fx::doubles kills no mutant."),
            "{reason}"
        );
    }
    let inbox = lab.json("main", &["thread", "inbox"]);
    let kinds: Vec<&str> = inbox
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["kind"].as_str().unwrap())
        .collect();
    assert_eq!(kinds, ["hint", "soft-block", "hard-block"], "{inbox}");
    lab.thread("main", &["ack", "3", "--reply", "added the assertion"]);
    assert_eq!(lab.pre_tool("main", tool), None);
    assert_eq!(lab.stop("main", false), None);
    let note = lab.json("obs", &["thread", "inbox"]);
    let reply = (note[0]["kind"].as_str(), note[0]["body"].as_str());
    assert_eq!(reply, (Some("note"), Some("added the assertion")));

    // a member that leaves cannot hold main0
    block_checkpoint("the suite fails.");
    assert!(lab.pre_tool("main", tool).is_some());
    assert!(
        lab.thread("main", &["leave", "1", "observer0"])
            .contains("1 of its blocks expired")
    );
    assert_eq!(lab.pre_tool("main", tool), None);
    assert_eq!(lab.stop("main", false), None);
}

/// A `SendMessage` call of main0 (Claude Code's message to another session) lands in the thread log with the
/// receiver, the text and whether it was sent: the recorded call that failed, and one that was sent.
#[test]
fn a_send_message_call_of_main_lands_in_the_thread_log() {
    let lab = thread_lab("send");
    let post_send = |text: &str, response: Option<Value>| {
        let mut fields =
            json!({"tool_name": "SendMessage", "tool_input": {"to": "reviewer", "message": text}});
        if let Some(r) = response {
            fields["tool_response"] = r;
        }
        assert_eq!(lab.hook("main", &lab.repo, "post-tool", fields), "");
    };
    post_send("is lint clean?", None);
    post_send("lint is clean", Some(json!({"success": true})));
    lab.hook(
        "obs",
        &lab.repo,
        "post-tool",
        json!({"tool_name": "SendMessage", "tool_input": {"to": "x", "message": "y"}}),
    );
    let log = lab.json("main", &["thread", "show", "1"])["log"].clone();
    let logged: Vec<Value> = log
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "thread.message" && r["payload"]["channel"] == "claude-inbox")
        .map(|r| {
            json!([
                r["member"],
                r["payload"]["to"],
                r["payload"]["text"],
                r["payload"]["sent"]
            ])
        })
        .collect();
    let expected = [
        json!(["main0", "reviewer", "is lint clean?", false]),
        json!(["main0", "reviewer", "lint is clean", true]),
        json!(["observer0", "x", "y", false]),
    ];
    assert_eq!(logged, expected);
}

/// Two agents on dossier port in the topic workspaces a and b, and one on another dossier.
fn peers_lab() -> (Lab, PathBuf, PathBuf) {
    let lab = Lab::new("peers");
    lab.ok("a", &lab.repo, &["dossier", "use", "port"]);
    lab.ok("a", &lab.repo, &["workspace", "open", "--topic", "a"]);
    lab.ok("a", &lab.repo, &["workspace", "open", "--topic", "b"]);
    lab.thread("a", &["start", "port the parser"]);
    lab.thread(
        "a",
        &[
            "join",
            "1",
            "worker-b",
            "main-agent",
            "--member-session",
            "b",
        ],
    );
    let other = lab.home.join("other");
    std::fs::create_dir_all(&other).unwrap();
    lab.ok("c", &other, &["dossier", "use", "notes"]);
    lab.hook("c", &other, "prompt", json!({"prompt": "go on"}));
    let ws = |t: &str| lab.home.join(format!("worktrees/repo/port--{t}"));
    let (a, b) = (ws("a"), ws("b"));
    (lab, a, b)
}

/// Durability and presence: a message in the outbox is delivered after an operator restart, once. Two agents work on
/// one dossier in two topic workspaces: each lists the other with its topic and state (running at a prompt or a tool
/// call, idle after a stop, ended after session end), its last tool call and its open messages; a session on another
/// dossier is not a peer; `thread observe` gives a peer's events.
#[test]
fn the_outbox_survives_a_restart_and_peers_on_one_dossier_see_each_other_with_their_state() {
    let (mut lab, ws_a, ws_b) = peers_lab();
    lab.thread(
        "b",
        &["send", "main0", "hint", "the lexer is done on topic b"],
    );
    let home = lab.home.clone();
    drop(std::mem::replace(
        &mut lab.op,
        Lab::operator("peers2", &home),
    ));
    let prompt =
        |session: &str, ws: &Path| lab.hook(session, ws, "prompt", json!({"prompt": "go on"}));
    let told = prompt("a", &ws_a);
    assert!(
        told.contains("hint from worker-b (thread 1): the lexer is done on topic b"),
        "{told}"
    );
    assert!(!prompt("a", &ws_a).contains("lexer"), "delivered once");

    prompt("b", &ws_b);
    lab.edit("b", &ws_b, &ws_b.join("src/lexer.rs"));
    lab.thread("a", &["send", "worker-b", "note", "merge after me"]);
    let peers = |session: &str, cwd: &Path| -> Value {
        serde_json::from_str(&lab.ok(session, cwd, &["thread", "peers", "--json"])).unwrap()
    };
    let of_a = peers("a", &ws_a);
    let sessions: Vec<&str> = of_a
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["session"].as_str().unwrap())
        .collect();
    assert_eq!(sessions, ["b"], "{of_a}");
    let b = &of_a[0];
    let fields = [
        "dossier",
        "topic",
        "state",
        "last_tool",
        "last_path",
        "pending",
        "shares",
        "channels",
    ]
    .map(|f| b[f].clone());
    let expected = [
        json!("port"),
        json!("b"),
        json!("running"),
        json!("Edit"),
        json!(ws_b.join("src/lexer.rs")),
        json!(1),
        json!(["dossier port", "thread 1"]),
        json!(["outbox", "claude-inbox"]),
    ];
    assert_eq!(fields, expected, "{b}");
    let of_b = peers("b", &ws_b);
    let seen: Vec<Value> = of_b
        .as_array()
        .unwrap()
        .iter()
        .map(|p| json!([p["session"], p["topic"], p["state"]]))
        .collect();
    assert_eq!(
        seen,
        [json!(["a", "a", "running"])],
        "c works on another dossier: {of_b}"
    );

    // the state follows b's hooks; observe gives b's events
    lab.hook("b", &ws_b, "stop", json!({}));
    assert_eq!(peers("a", &ws_a)[0]["state"], "idle");
    let observed: Value =
        serde_json::from_str(&lab.ok("a", &ws_a, &["thread", "observe", "worker-b", "--json"]))
            .unwrap();
    assert_eq!(observed["peer"]["session"], "b");
    let kinds: Vec<&str> = observed["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["kind"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"internal.agent.tool") && kinds.contains(&"thread.send"),
        "{kinds:?}"
    );
    lab.hook("b", &ws_b, "session-end", json!({}));
    assert_eq!(peers("a", &ws_a)[0]["state"], "ended");
    let refused = lab.refused("a", &["thread", "observe", "c"]);
    assert!(
        refused.contains("is not a peer of this session"),
        "{refused}"
    );
}
