//! End-to-end: threads (design 23, Part B, P5) through the real `legionctl hook` adapter with Claude Code's hook
//! payloads (recorded in tests/fixtures/claude-code-hooks), the CLI and the real operator: watch rules and their
//! counts, delivery of the outbox through the prompt, pre-tool and stop hooks, `SendMessage` in the thread log, pace,
//! an operator restart, and presence.

#[path = "common/clauth.rs"]
mod clauth;
mod common;
#[path = "common/hooks.rs"]
mod hooks;
#[path = "common/keyring.rs"]
mod keyring;
#[path = "common/plain.rs"]
mod plain;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::Operator;
use common::scratch::Scratch;
use hooks::claude_hook;
use serde_json::{Value, json};

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// An operator with its own HOME and a repository with one commit on main.
struct Lab {
    op: Operator,
    home: Scratch,
    repo: PathBuf,
}

impl Lab {
    fn new(tag: &str) -> Lab {
        let home = Scratch::new(format!(
            "/var/tmp/legionctl-thread-{tag}-{}",
            std::process::id()
        ));
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
        // the repository ignores the dossier home's link already, as legionctl's own
        write(&repo.join(".gitignore"), "/agentWorkspaces\n");
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
        Self::operator_with(tag, home, &[])
    }

    /// The operator with `extra` in its environment besides its HOME, PATH and sysroot.
    fn operator_with(tag: &str, home: &Path, extra: &[(&str, OsString)]) -> Operator {
        let bin = Path::new(env!("CARGO_BIN_EXE_legionctl"))
            .parent()
            .unwrap()
            .to_path_buf();
        let path: OsString = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        std::fs::create_dir_all(home.join("root")).unwrap();
        let env: Vec<(&str, OsString)> = [
            ("LEGIONCTL_SYSROOT", home.join("root").into()),
            ("HOME", home.into()),
            ("PATH", path),
        ]
        .into_iter()
        .filter(|(k, _)| !extra.iter().any(|(e, _)| e == k))
        .collect();
        Operator::start(&format!("th-{tag}"), &[&env[..], extra].concat())
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

/// Delegation and messages point to the dossier (user 2026-10-16, item 7 of v1-bugs): a subagent that a session with
/// a dossier starts gets the delegation rule with that dossier's plan path in its context (the `SubagentStart` hook's
/// `additionalContext`; payload shape from the hooks reference, no recorded fixture); a session without a dossier
/// gets nothing. A thread message over `dossier.message_chars` with no dossier link gets a one-line hint and is sent;
/// one with a link gets no hint.
#[test]
fn a_delegate_reads_the_dossier_first_and_a_long_message_is_told_to_link_it() {
    let lab = thread_lab("delegation-rule");
    let started = lab.hook("main", &lab.repo, "subagent-start", json!({}));
    let v: Value = serde_json::from_str(&started).unwrap();
    assert_eq!(v["hookSpecificOutput"]["hookEventName"], "SubagentStart");
    let rule = v["hookSpecificOutput"]["additionalContext"]
        .as_str()
        .unwrap();
    assert!(
        rule.contains("legionctl dossier read port/plan.md")
            && rule.contains("adversarial")
            && rule.contains("point to dossier artifacts"),
        "{rule}"
    );
    assert_eq!(
        lab.hook("loner", &lab.repo, "subagent-start", json!({})),
        "",
        "no dossier, no rule"
    );

    let long = "the parser drops the last token of each line. ".repeat(14);
    let hinted = lab.thread("obs", &["send", "main0", "note", &long]);
    assert!(
        hinted.contains("hint: the message has more than 600 characters and no dossier link"),
        "{hinted}"
    );
    let linked = format!("{long} details: plan#todo-1");
    let plain = lab.thread("obs", &["send", "main0", "note", &linked]);
    assert!(!plain.contains("hint:"), "{plain}");
    let inbox = lab.json("main", &["thread", "inbox"]);
    assert_eq!(inbox.as_array().unwrap().len(), 2, "both are sent: {inbox}");
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
    let home = lab.home.to_path_buf();
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

/// Session `lead` on dossier port with the topic workspaces a and b, and thread 1 where the member `worker-b` is its
/// subagent `sub-b` and `observer0` watches worker-b: a reminder after 2 edits without `make lint`.
fn subagents_lab() -> (Lab, PathBuf, PathBuf) {
    let lab = Lab::new("subagents");
    lab.ok("lead", &lab.repo, &["dossier", "use", "port"]);
    lab.ok("lead", &lab.repo, &["workspace", "open", "--topic", "a"]);
    lab.ok("lead", &lab.repo, &["workspace", "open", "--topic", "b"]);
    lab.thread("lead", &["start", "port the parser"]);
    for (name, archetype, session) in [
        ("worker-b", "main-agent", "lead/sub-b"),
        ("observer0", "thread-observer", "obs"),
    ] {
        let join = ["join", "1", name, archetype, "--member-session", session];
        lab.thread("lead", &join);
    }
    let rule = [
        "rules",
        "add",
        "1",
        "observer0",
        "make lint",
        "--watched",
        "worker-b",
        "--every",
        "2",
    ];
    lab.thread("lead", &rule);
    let ws = |t: &str| lab.home.join(format!("worktrees/repo/port--{t}"));
    let (a, b) = (ws("a"), ws("b"));
    (lab, a, b)
}

/// The `additionalContext` of a post-tool hook's answer.
fn added_context(out: &str) -> String {
    let v: Value = serde_json::from_str(out).unwrap();
    let hook = &v["hookSpecificOutput"];
    assert_eq!(hook["hookEventName"], "PostToolUse", "{v}");
    hook["additionalContext"].as_str().unwrap().to_string()
}

/// Subagents (todo-120): Claude Code's hook payload of a subagent's tool call has the lead's `session_id` and the
/// subagent's `agent_id` and `agent_type` (the documented common fields; the recorded fixtures are of a lead). Two
/// subagents of session `lead` edit in the topic workspaces a and b: each is a peer of its own with its workspace,
/// type and last tool call, and the lead's own last call stays its own. A note to `lead/sub-a` and a watch rule on the
/// member `worker-b` (session `lead/sub-b`) reach each subagent at its next tool call, not the other and not the
/// lead. `thread observe` of one subagent gives only its events.
#[test]
fn the_subagents_of_one_session_are_peers_with_their_own_workspace_tool_calls_and_messages() {
    let (lab, ws_a, ws_b) = subagents_lab();
    // the post-tool hook of an Edit by sub-a (Explore, in a) or sub-b (general-purpose, in b): what it gives back
    let sub = |agent: &str| -> String {
        let (kind, ws, file) = match agent {
            "sub-a" => ("Explore", &ws_a, "src/a.rs"),
            _ => ("general-purpose", &ws_b, "src/b.rs"),
        };
        let fields = json!({"agent_id": agent, "agent_type": kind, "tool_name": "Edit",
            "tool_input": {"file_path": ws.join(file), "old_string": "a", "new_string": "b"}});
        lab.hook("lead", ws, "post-tool", fields)
    };
    lab.hook("lead", &lab.repo, "prompt", json!({"prompt": "go on"}));
    assert_eq!(sub("sub-a"), "");
    assert_eq!(sub("sub-b"), "");
    lab.edit("lead", &lab.repo, &lab.repo.join("README.md"));
    assert_eq!(sub("sub-a"), "", "sub-a's edits do not count for worker-b");

    let peers = |session: &str| -> Value {
        serde_json::from_str(&lab.ok(
            session,
            &lab.repo,
            &["thread", "peers", "--json", "--dossier", "port"],
        ))
        .unwrap()
    };
    // session, subagent, topic, state, last path, members
    let rows = |v: &Value| -> Vec<String> {
        let row = |p: &Value| {
            let f = [
                "session",
                "subagent",
                "topic",
                "state",
                "last_path",
                "members",
            ];
            f.map(|k| p[k].to_string()).join(" ")
        };
        v.as_array().unwrap().iter().map(row).collect()
    };
    let (a, b) = (ws_a.join("src/a.rs"), ws_b.join("src/b.rs"));
    assert_eq!(
        rows(&peers("lead")),
        [
            format!(
                r#""lead/sub-a" "Explore" "a" "running" "{}" []"#,
                a.display()
            ),
            format!(
                r#""lead/sub-b" "general-purpose" "b" "running" "{}" ["1/worker-b"]"#,
                b.display()
            ),
        ]
    );
    let of_obs = peers("obs");
    let lead = of_obs
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["session"] == "lead")
        .unwrap();
    assert_eq!(
        [&lead["subagent"], &lead["last_path"]],
        [&json!(null), &json!(lab.repo.join("README.md"))],
        "the subagents' calls are not the lead's: {of_obs}"
    );

    // a note to sub-a; sub-b's second edit fires the rule on worker-b
    lab.thread("lead", &["send", "lead/sub-a", "note", "rebase on b first"]);
    assert_eq!(peers("lead")[0]["pending"], 1);
    let to_b = added_context(&sub("sub-b"));
    assert!(
        to_b.contains(
            "reminder from observer0 (thread 1): observer0: no `make lint` after 2 edits"
        ),
        "{to_b}"
    );
    assert!(!to_b.contains("rebase"), "{to_b}");
    let to_a = added_context(&sub("sub-a"));
    assert!(
        to_a.contains("note from main0 (thread 1): rebase on b first"),
        "{to_a}"
    );
    assert!(!to_a.contains("reminder"), "{to_a}");
    assert_eq!(sub("sub-a"), "", "delivered once");
    let told = lab.hook("lead", &lab.repo, "prompt", json!({"prompt": "go on"}));
    assert!(
        !told.contains("rebase") && !told.contains("reminder"),
        "not to the lead: {told}"
    );

    // observe one subagent: only its tool calls
    let observed = lab.json("lead", &["thread", "observe", "lead/sub-a"]);
    let agents: Vec<&Value> = observed["events"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| &e["payload"]["input"]["agent"])
        .collect();
    assert_eq!(agents, [&json!("sub-a"); 4], "{observed}");
    let by_name = lab.json("lead", &["thread", "observe", "worker-b"]);
    assert_eq!(by_name["peer"]["session"], "lead/sub-b");
}

/// The fields `fields` of each item of the array `key` of `v`.
fn picked(v: &Value, key: &str, fields: [&str; 3]) -> Vec<[Value; 3]> {
    v[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| fields.map(|f| item[f].clone()))
        .collect()
}

/// `thread import` (design 23, C5): thread 1 runs on the current login; its import to clauth account `work` makes
/// thread 2 with the same log rows, members (without their sessions, which run on the old account), messages and
/// watch rules, then a `thread.import` row. Thread 1 is read-only after it: a second import (the copy exists), a
/// message and a layout are refused. The copy refuses an import to its own account and to an account that clauth
/// does not have.
#[test]
fn a_thread_imported_to_another_account_copies_its_log_and_members_and_imports_once() {
    let lab = thread_lab("import");
    lab.thread("main", &["send", "observer0", "note", "check the lint"]);
    lab.thread("main", &["rules", "add", "1", "observer0", "make lint"]);
    write(
        &lab.home.join(".clauth/profiles.toml"),
        "profiles = [\"work\"]\n",
    );
    let before = lab.json("main", &["thread", "show", "1"]);

    let out = lab.thread("main", &["import", "1", "work"]);
    assert_eq!(
        out,
        "thread 1 imported to account work as thread 2; thread 1 is read-only now"
    );
    let copy = lab.json("main", &["thread", "show", "2"]);
    let (old, new) = (
        picked(&before, "log", ["member", "kind", "payload"]),
        picked(&copy, "log", ["member", "kind", "payload"]),
    );
    assert_eq!(new[..old.len()], old[..], "the log rows in order");
    assert_eq!(new.len(), old.len() + 1);
    assert_eq!(
        new[old.len()],
        [
            json!(""),
            json!("thread.import"),
            json!({"from": 1, "account": "work", "session": "main"})
        ]
    );
    assert_eq!(
        picked(&copy, "members", ["name", "archetype", "session"]),
        [
            [json!("main0"), json!("main-agent"), Value::Null],
            [json!("observer0"), json!("thread-observer"), Value::Null]
        ]
    );
    assert_eq!(
        picked(&before, "members", ["name", "archetype", "session"])[0][2],
        json!("main")
    );
    assert_eq!(copy["messages"][0]["body"], "check the lint");
    assert_eq!(copy["messages"][0]["thread"], 2);
    assert_eq!(copy["rules"][0]["expect"], "make lint");
    let list = lab.json("main", &["thread", "list"]);
    let info = |id: i64| {
        list.as_array()
            .unwrap()
            .iter()
            .find(|t| t["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(
        (
            info(2)["state"].clone(),
            info(2)["account"].clone(),
            info(2)["imported_from"].clone()
        ),
        (json!("open"), json!("work"), json!(1))
    );
    assert_eq!(
        (info(1)["state"].clone(), info(1)["account"].clone()),
        (json!("imported"), Value::Null)
    );

    let again = lab.refused("main", &["thread", "import", "1", "work"]);
    assert!(
        again.contains("thread 1 was imported to thread 2 already"),
        "{again}"
    );
    let send = lab.refused(
        "main",
        &["thread", "send", "observer0", "note", "x", "--thread", "1"],
    );
    assert!(send.contains("thread 1 is imported: read-only"), "{send}");
    let same = lab.refused("main", &["thread", "import", "2", "work"]);
    assert!(
        same.contains("thread 2 runs on account work already"),
        "{same}"
    );
    let missing = lab.refused("main", &["thread", "import", "2", "nope"]);
    assert!(
        missing.contains("no account nope; the accounts are: work"),
        "{missing}"
    );
    assert_eq!(
        lab.json("main", &["thread", "list"])
            .as_array()
            .unwrap()
            .len(),
        2,
        "a refused import makes no thread"
    );
}

/// Views (design 23, C4) from the CLI: a view of a widget outside the closed set, and a view whose source changes
/// the machine (`power.apply`), are refused by `views check` and `views save` and not written; a valid view saves
/// into the user's layer, renders the thread's members as rows for agents (`views render`), and a view placed on the
/// thread is a `views.place` row of its log. A view in the repository's layer hides the user's view of that name.
#[test]
fn a_view_cannot_bind_a_change_operation_or_a_widget_outside_the_set() {
    let lab = thread_lab("views");
    let run = |args: &[&str], stdin: &str| {
        lab.op.run(
            &lab.repo,
            &[
                ("HOME", lab.home.to_str().unwrap()),
                ("LEGIONCTL_SESSION", "main"),
            ],
            args,
            stdin,
        )
    };
    let chart = r#"{"name": "load", "widget": "piechart", "source": {"op": "status"}}"#;
    let change = r#"{"name": "power", "widget": "tree", "source": {"op": "power.apply", "input": {"source": "auto"}}}"#;
    // r324's Machine now read the fans here and showed "legionctl-system does not answer" while it answered
    let no_path = r#"{"name": "fans", "widget": "stat", "source": {"op": "monitor", "path": "$.0.root.fan_rpm"}}"#;
    for (view, why) in [
        (no_path, "monitor's answer has nothing at $.0.root.fan_rpm"),
        (
            chart,
            "unknown variant `piechart`, expected one of `table`, `tree`, `gauge`, `sparkline`, `markdown`, `diff`, `list`, `stack`, `stat`, `chart`",
        ),
        (
            change,
            "power.apply is a machine operation: a view reads only",
        ),
    ] {
        for op in ["check", "save"] {
            let out = run(&["views", op], view);
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(
                !out.status.success() && err.contains(why),
                "views {op}: {err}"
            );
        }
    }
    let views = lab.home.join(".config/legionctl/views");
    assert!(!views.exists(), "a refused view is not written");
    // every seed passes the same check: its paths are in its operation's answer
    let seeds = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/views");
    for seed in std::fs::read_dir(&seeds).unwrap() {
        let seed = seed.unwrap().path();
        let out = run(
            &["views", "check"],
            &std::fs::read_to_string(&seed).unwrap(),
        );
        assert!(
            out.status.success(),
            "{}: {}",
            seed.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }

    let members = r#"{
  "name": "members",
  "params": { "thread": "integer" },
  "widget": "table",
  // the members of the thread
  "source": { "op": "thread.show", "input": { "thread": "{thread}" }, "path": "$.members" },
  "columns": ["name", "archetype"]
}"#;
    let out = run(&["views", "save"], members);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(views.join("members.jsonc")).unwrap(),
        members,
        "the file as written, with its comment"
    );
    let text = lab.ok(
        "main",
        &lab.repo,
        &["views", "render", "members", "thread=1"],
    );
    assert_eq!(
        text,
        "members (table, version 1)\n2 rows: name | archetype\nmain0 | main-agent\nobserver0 | thread-observer"
    );
    let missing = run(&["views", "render", "members"], "");
    assert_eq!(
        (
            missing.status.success(),
            String::from_utf8_lossy(&missing.stdout).trim()
        ),
        (
            false,
            "members (table, version 1): cannot read: view members needs the parameter thread"
        )
    );

    lab.ok(
        "main",
        &lab.repo,
        &["views", "place", "1", "members", "thread=1"],
    );
    let log = lab.json("main", &["thread", "show", "1"])["log"].clone();
    let placed = log.as_array().unwrap().last().unwrap().clone();
    assert_eq!(placed["kind"], "views.place");
    assert_eq!(placed["payload"]["view"], "members");
    assert_eq!(placed["payload"]["params"], json!({"thread": "1"}));

    repository_layer_below_the_users(&lab, members);
}

/// The timeline and form widgets (DESIGN-09 G4) in their terminal form, which agents and the CLI read: a thread's
/// log as a timeline (its events, the actor and kind of each), and the form of an operation with its required
/// fields; a form that names no operation is refused at save. A list of one object shows a row per field (the
/// shape of status and load, G5's live finding), and a jq path is refused at save with what a path is.
#[test]
fn a_timeline_and_a_form_view_render_their_terminal_form() {
    let lab = thread_lab("widgets");
    let save = |text: &str| {
        lab.op.run(
            &lab.repo,
            &[
                ("HOME", lab.home.to_str().unwrap()),
                ("LEGIONCTL_SESSION", "main"),
            ],
            &["views", "save"],
            text,
        )
    };
    let timeline = r#"{"name": "log", "params": {"thread": "integer"}, "widget": "timeline",
        "source": {"op": "thread.show", "input": {"thread": "{thread}"}, "path": "$.log"},
        "columns": ["at", "member", "kind", "payload.archetype|payload.title"]}"#;
    assert!(save(timeline).status.success());
    let text = lab.ok("main", &lab.repo, &["views", "render", "log", "thread=1"]);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[1], "3 events", "{text}");
    assert!(
        lines[2].ends_with(" main0 thread.start: port the parser"),
        "{text}"
    );
    assert!(
        lines[4].ends_with(" observer0 thread.join: thread-observer"),
        "{text}"
    );
    assert!(lines[2].starts_with("20"), "a date, not seconds: {text}");

    let form = r#"{"name": "remember", "widget": "form", "form": {"label": "Keep a fact", "op": "facts.set"}}"#;
    assert!(save(form).status.success());
    let text = lab.ok("main", &lab.repo, &["views", "render", "remember"]);
    assert!(
        text.contains("form of facts.set: entity*, attribute, value*"),
        "{text}"
    );
    let refused = save(&form.replace("facts.set", "facts.sett"));
    assert!(String::from_utf8_lossy(&refused.stderr).contains("names no operation facts.sett"));

    // a list of one object: a row per column, "field: value" (a model's view of status or load reads one object)
    let info = r#"{"name": "info", "params": {"thread": "integer"}, "widget": "list",
        "source": {"op": "thread.show", "input": {"thread": "{thread}"}, "path": "$.thread"},
        "columns": ["title", "state", "account"], "labels": {"title": "Task"}}"#;
    assert!(save(info).status.success());
    let text = lab.ok("main", &lab.repo, &["views", "render", "info", "thread=1"]);
    assert_eq!(
        text.lines().skip(1).collect::<Vec<_>>(),
        ["3 rows", "Task: port the parser", "state: open", "account:"],
        "{text}"
    );
    // an empty list says so, never a bare dash
    let empty = r#"{"name": "rules", "params": {"thread": "integer"}, "widget": "list",
        "source": {"op": "thread.show", "input": {"thread": "{thread}"}}, "columns": ["rules"]}"#;
    assert!(save(empty).status.success());
    let text = lab.ok("main", &lab.repo, &["views", "render", "rules", "thread=1"]);
    assert_eq!(text.lines().last(), Some("rules: none"), "{text}");
    // a table grouped by a column: the group's head says it, the rows do not repeat it
    let grouped = r#"{"name": "by-member", "params": {"thread": "integer"}, "widget": "table",
        "source": {"op": "thread.show", "input": {"thread": "{thread}"}, "path": "$.log"},
        "columns": ["member", "kind"], "group": {"field": "member"}}"#;
    assert!(save(grouped).status.success());
    let data = lab.json("main", &["views", "render", "by-member", "thread=1"]);
    assert_eq!(data["columns"], json!(["kind"]), "{data}");
    assert_eq!(
        data["groups"],
        json!(["main0", "main0", "observer0"]),
        "{data}"
    );
    assert_eq!(data["rows"][2], json!(["thread.join"]), "{data}");
    // with rules: dropped only while each cell says its group (the live branch-changes view), kept when a rule
    // puts other values under one head
    for (rules, columns) in [
        (
            r#"[{"group": "main0", "prefix": "main"}, {"group": "observer0", "prefix": "obs"}]"#,
            json!(["kind"]),
        ),
        (
            r#"[{"group": "main0", "prefix": "main"}], "rest": "others""#,
            json!(["member", "kind"]),
        ),
    ] {
        let ruled = grouped.replace(
            r#"{"field": "member"}"#,
            &format!(r#"{{"field": "member", "rules": {rules}}}"#),
        );
        assert!(save(&ruled).status.success());
        let data = lab.json("main", &["views", "render", "by-member", "thread=1"]);
        assert_eq!(data["columns"], columns, "{rules}: {data}");
    }
    // a model's jq habits are refused at save with what a path is, not read as no data at render
    let refused = save(&info.replace("$.thread", "$.log[] | .member"));
    assert!(
        String::from_utf8_lossy(&refused.stderr)
            .contains("a path is $, then .field, .0 or .[field=value] steps: $.log[] | .member"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
}

/// `legionctl ask` (DESIGN-09 Rule 1): the same lookup as the app's chat bar. The intent of a stored view (any case and
/// end mark) prints that view in its terminal form, counts first, with no model; with --stored an intent that no view
/// has fails and says so.
#[test]
fn the_cli_asks_for_an_intent_and_prints_the_stored_view() {
    let lab = thread_lab("ask");
    let text = lab.ok(
        "main",
        &lab.repo,
        &["ask", "Everything legionctl can do?", "--stored"],
    );
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(lines[0], "operations (stored)", "{text}");
    assert!(
        lines[1].starts_with("Operations (table, original)"),
        "{text}"
    );
    assert!(
        lines[2].ends_with("rows: Operation | What it changes | What it does"),
        "{text}"
    );
    let out = lab.op.run(
        &lab.repo,
        &[
            ("HOME", lab.home.to_str().unwrap()),
            ("LEGIONCTL_SESSION", "main"),
        ],
        &["ask", "a view nobody made", "--stored"],
        "",
    );
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("no stored view answers \"a view nobody made\""),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// The user's Codex login in an ask lab.
const CODEX_LOGIN: &str = r#"{"tokens": {"refresh_token": "r-codex-of-the-user"}}"#;

/// A lab for `ask` (todo-163): the stand-in decision server and keyring, the stand-in ACP agent as the composer
/// (`fake`; `other` the same agent under another name), whose answer to an intent is the view file `<first word>.jsonc`
/// of `views`, the user's Codex login [`CODEX_LOGIN`], and `config` in the user's layer.
fn ask_lab(tag: &str, config: &Value) -> (Lab, u16, PathBuf, common::process::Kill, PathBuf) {
    let mut lab = Lab::new(tag);
    let (server, port, log, mut env) = decisions(&lab.home);
    let views = lab.home.join("composed");
    std::fs::create_dir_all(&views).unwrap();
    env.push(("FAKE_ACP_VIEWS", views.clone().into()));
    lab.op.child.kill().unwrap();
    lab.op.child.wait().unwrap();
    lab.op = Lab::operator_with(&format!("{tag}p"), &lab.home, &env);
    let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-acp-agent.py");
    let mut all = json!({
        "rlcd": {"backend": "stand"},
        "session": {"acp": {"fake": ["python3", fake], "other": ["python3", fake, "other"]},
                    "scope": false, "writable": []},
        "app": {"agent": "fake"}
    });
    for (k, v) in config.as_object().unwrap() {
        all[k] = v.clone();
    }
    write(&lab.home.join(".codex/auth.json"), CODEX_LOGIN);
    write(
        &lab.home.join(".config/legionctl/ask.jsonc"),
        &all.to_string(),
    );
    (lab, port, log, server, views)
}

impl Lab {
    /// `legionctl ask <args> --json` of the user in the repository (the composer's runs on dossier `views`): the
    /// output, and the answer when it is JSON.
    fn ask(&self, args: &[&str]) -> (std::process::Output, Value) {
        let out = self.op.run(
            &self.repo,
            &[("HOME", self.home.to_str().unwrap())],
            &[&["ask"], args, &["--dossier", "views", "--json"]].concat(),
            "",
        );
        let answer = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        (out, answer)
    }

    /// The composer's runs so far (the operator's session.run jobs).
    fn composer_runs(&self) -> usize {
        self.json("main", &["jobs", "list"])["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|j| j["kind"] == "session.run")
            .count()
    }
}

/// The rlcd step of `ask` (config views.rlcd; todo-163): an intent that no stored view has word for word goes to rlcd
/// as a choice among the stored intents that share a word with it (a lexical prefilter; the state is the intent only,
/// each option an intent and its title); its pick at 0.8 opens that view and no composer runs. A pick of none goes on
/// to the composer; a failed rlcd call fails the ask with rlcd's error and the composer does not run in its place. An
/// intent that shares no word with a stored one asks no rlcd. Each step's outcome, tokens and time are in the answer,
/// in the event views.ask and in views cost, the composer's with the account it ran on (todo-179 (5): the current
/// login, here clauth's profile alt, linked as clauth 0.17.0 does).
#[test]
fn a_similar_intent_opens_the_stored_view_through_rlcd_and_a_failed_rlcd_runs_no_composer() {
    let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-acp-agent.py");
    let (lab, port, log, server, _) = ask_lab(
        "ask-rlcd",
        &json!({"views": {"rlcd": true}, "app": {"agent": "claude"},
            "session": {"acp": {"claude": ["python3", fake]}, "scope": false, "writable": []}}),
    );
    let alt = lab.home.join(".clauth/profiles/alt/credentials.json");
    write(
        &alt,
        r#"{"claudeAiOauth": {"accessToken": "tok-a", "refreshToken": "r-alt"}}"#,
    );
    write(
        &lab.home.join(".clauth/profiles.toml"),
        "profiles = [\"alt\"]\n",
    );
    std::fs::create_dir_all(lab.home.join(".claude")).unwrap();
    std::os::unix::fs::symlink(&alt, lab.home.join(".claude/.credentials.json")).unwrap();
    clauth::install(&lab.home);
    write(&lab.home.join("clauth-which"), "alt\n");
    let ask = |pick: &str, intent: &str| {
        rlcd_backends(&lab.home, port, pick);
        lab.ask(&[intent])
    };
    let (out, answer) = ask("operations", "list what I can run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        (answer["route"].as_str(), answer["view"].as_str()),
        (Some("rlcd"), Some("operations")),
        "{answer}"
    );
    let steps: Vec<(&str, &str)> = answer["steps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["step"].as_str().unwrap(), s["outcome"].as_str().unwrap()))
        .collect();
    assert_eq!(steps, [("stored", "none"), ("rlcd", "answered")]);
    assert_eq!(answer["steps"][1]["tokens_in"], 169);
    assert_eq!(lab.composer_runs(), 0, "rlcd's pick: no composer run");
    let call = calls(&log).pop().unwrap();
    assert_eq!(
        call["body"]["state"], "The user asks: list what i can run",
        "the state is the intent, nothing else: {call}"
    );
    let criteria = call["body"]["questions"]["view"]["criteria"]
        .as_object()
        .unwrap();
    // only the stored intents that share a word ("can") with it, and none
    assert_eq!(
        criteria.keys().map(String::as_str).collect::<Vec<_>>(),
        ["none", "operations"],
        "{call}"
    );
    assert_eq!(
        criteria["operations"],
        "everything legionctl can do - Operations"
    );

    // no stored intent shares a word: rlcd is not asked, and with --stored nothing answers
    let before = calls(&log).len();
    let (none, _) = lab.ask(&["xyzzy plugh", "--stored"]);
    assert!(String::from_utf8_lossy(&none.stderr).contains("no stored view answers"));
    assert_eq!(
        calls(&log).len(),
        before,
        "no rlcd call without a candidate"
    );

    // a failed call fails the ask: the composer does not run in rlcd's place
    let (failed, _) = ask("fail", "list what I can run");
    assert!(!failed.status.success());
    assert!(
        String::from_utf8_lossy(&failed.stderr).contains("rlcd stand"),
        "{}",
        String::from_utf8_lossy(&failed.stderr)
    );
    assert_eq!(
        lab.composer_runs(),
        0,
        "a failed rlcd step runs no composer"
    );

    // a pick of none goes on to the composer (which answers in words here)
    let (went_on, answer) = ask("none", "list what I can run");
    assert!(
        went_on.status.success(),
        "{}",
        String::from_utf8_lossy(&went_on.stderr)
    );
    assert_eq!(answer["route"], "composer", "{answer}");
    assert_eq!(lab.composer_runs(), 1);

    rlcd_costs_are_recorded(&lab);
    drop(server);
}

/// todo-181: "go back to the table" on a tab is rlcd's pick among the other versions of the tab's view (each by its
/// widget, title and intent), and the view shows that version with no composer run. A new wish ("chart over time")
/// is rlcd's none: the composer makes the next version. A message that asks for no change ("Hi") gets words and no
/// version.
#[test]
fn going_back_to_an_earlier_version_of_a_tabs_view_is_rlcds_pick_with_no_composer_run() {
    let (lab, port, log, server, views) = ask_lab("ask-back", &json!({"views": {"rlcd": true}}));
    write(
        &views.join("memory.jsonc"),
        r#"{"name": "mem", "title": "Memory", "widget": "table", "source": {"op": "sensors"}, "columns": ["name"]}"#,
    );
    write(
        &views.join("chart.jsonc"),
        r#"{"name": "mem", "title": "Memory, more", "widget": "list", "source": {"op": "sensors"}, "columns": ["name"]}"#,
    );
    let ask = |pick: &str, args: &[&str]| {
        rlcd_backends(&lab.home, port, pick);
        let (out, answer) = lab.ask(args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        answer
    };
    let first = ask("none", &["memory use of the machine"]);
    let name = first["view"].as_str().unwrap().to_string();
    // the composer's run works in a scratch folder, never in the caller's (the app's is the home: r324 made a
    // copy-on-write layer of the whole home for it)
    let sessions = lab.json("main", &["session", "list"]);
    let folders: Vec<&str> = sessions
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["workdir"].as_str())
        .collect();
    assert!(
        !folders.is_empty() && folders.iter().all(|f| f.ends_with("/composer")),
        "{sessions}"
    );
    let second = ask("none", &["chart over time", "--view", &name]);
    assert_eq!(second["route"], "composer", "a new wish: {second}");
    assert_eq!(second["data"]["version"], 2);
    assert_eq!(lab.composer_runs(), 2);

    let back = ask("v1", &["go back to the table", "--view", &name]);
    assert_eq!(
        (
            back["route"].as_str(),
            back["view"].as_str(),
            back["data"]["version"].as_u64()
        ),
        (Some("rlcd"), Some(name.as_str()), Some(1)),
        "{back}"
    );
    assert_eq!(lab.composer_runs(), 2, "no composer run to go back");
    let call = calls(&log).pop().unwrap();
    let criteria = call["body"]["questions"]["version"]["criteria"]
        .as_object()
        .unwrap();
    assert_eq!(
        criteria.keys().map(String::as_str).collect::<Vec<_>>(),
        ["none", "v1"],
        "the other versions only: {call}"
    );
    assert!(
        criteria["v1"]
            .as_str()
            .unwrap()
            .starts_with("Table - Memory"),
        "{call}"
    );

    // "Hi" in the tab (the lead's r324 walk made version 3 of it): the composer gives the view back as it is, so
    // the answer is its words and the tab's view stays at its version
    write(
        &views.join("hi.jsonc"),
        r#"{"name": "mem", "title": "Memory", "intent": "memory use of the machine", "widget": "table",
            "source": {"op": "sensors"}, "columns": ["name"]}"#,
    );
    let hi = ask("none", &["Hi", "--view", &name]);
    assert_eq!(
        (hi["route"].as_str(), hi["view"].as_str()),
        (Some("composer"), None),
        "{hi}"
    );
    assert!(
        hi["text"].as_str().unwrap().contains("Here is the view."),
        "{hi}"
    );
    let shown = lab.json("main", &["views", "render", &name]);
    assert_eq!(shown["version"], 1, "no new version: {shown}");
    drop(server);
}

/// views cost after the asks of the rlcd test, and the event of its failed ask.
fn rlcd_costs_are_recorded(lab: &Lab) {
    let cost = lab.json("main", &["views", "cost"]);
    let row = |step: &str| {
        cost.as_array()
            .unwrap()
            .iter()
            .find(|c| c["step"] == step)
            .cloned()
            .unwrap_or_else(|| panic!("no {step} in {cost}"))
    };
    let rlcd = row("rlcd");
    assert_eq!(
        (
            &rlcd["runs"],
            &rlcd["answered"],
            &rlcd["failed"],
            &rlcd["tokens_in"]
        ),
        // the ask with no candidate ran the step with no call: none, no tokens
        (&json!(4), &json!(1), &json!(1), &json!(338)),
        "{cost}"
    );
    let composer = row("composer");
    assert_eq!(
        (&composer["runs"], &composer["failed"], &composer["account"]),
        (&json!(1), &json!(0), &json!("alt")),
        "the composer ran on the current login, clauth's alt: {cost}"
    );
    assert_eq!(rlcd["account"], Value::Null, "rlcd runs no agent");
    assert!(composer["tokens_in"].as_u64().unwrap() > 500, "{cost}");
    let events = lab.json("main", &["events", "--limit", "1000"]);
    let failed_event = events["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "views.ask")
        .find(|e| {
            e["payload"]["error"]
                .as_str()
                .is_some_and(|e| e.contains("rlcd stand"))
        })
        .unwrap_or_else(|| panic!("no failed ask event: {events}"));
    assert_eq!(failed_event["payload"]["route"], "rlcd");
}

/// The binding of the composer's ACP session, from the stand-in's line `bound: <json>` in its answer `text`.
fn composer_is_bound(text: &str) {
    // what the composer's session got
    let bound: Value = serde_json::from_str(
        text.lines()
            .find_map(|l| l.strip_prefix("bound: "))
            .unwrap_or_else(|| panic!("no bound line: {text}")),
    )
    .unwrap();
    let tools = [
        "views.list",
        "views.check",
        "views.parts",
        "views.render",
        "views.save",
        "views.place",
    ];
    let args: Vec<String> = std::iter::once("mcp".to_string())
        .chain(
            tools
                .iter()
                .flat_map(|t| ["--only".to_string(), (*t).to_string()]),
        )
        .collect();
    assert_eq!(
        bound["mcp"],
        json!([{"name": "legionctl", "command": "legionctl", "args": args}]),
        "only legionctl's MCP server, with the view tools only: {bound}"
    );
    assert_eq!(
        bound["meta"],
        json!({"disableBuiltInTools": true,
               "claudeCode": {"options": {"settingSources": [], "strictMcpConfig": true}}}),
        "no file, shell or web tools, no settings, plugins or other servers: {bound}"
    );
    // Codex's binding: its own home, with no default environment (no shell, patch or image tool), no web search,
    // plugins or hooks, and the user's login as the real file
    let codex = &bound["codex_home"];
    assert_eq!(
        codex["environments.toml"], "default = \"none\"\n",
        "{bound}"
    );
    let config = codex["config.toml"].as_str().unwrap_or_default();
    for line in [
        "web_search = \"disabled\"",
        "plugins = false",
        "hooks = false",
        "apps = false",
    ] {
        assert!(config.lines().any(|l| l == line), "{line}: {bound}");
    }
    assert_eq!(codex["auth.json"], CODEX_LOGIN, "{bound}");
}

/// The parts library and the bound composer (todo-163). A part is a stored one-widget view; a stack places it by
/// name and version. The composer's request lists the parts that fit the intent; its stack that places a part at
/// version 1 keeps showing version 1 after the part's version 2; a part it writes in place that the library has
/// already becomes a reference to that part, and a new one goes into the library. The composer's ACP session gets
/// legionctl's MCP server with the view tools only, and the adapter is asked for no tools and settings of its own;
/// every agent of `session.acp` can be the composer (`other` too, a second run).
#[test]
fn a_new_view_reuses_a_library_part_by_reference_and_the_composer_is_bound_to_view_tools() {
    let (lab, _, _, server, composed) = ask_lab("ask-parts", &json!({}));
    let save = |text: &str| {
        let out = lab.op.run(
            &lab.repo,
            &[("HOME", lab.home.to_str().unwrap())],
            &["views", "save"],
            text,
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let part = |title: &str| {
        json!({"name": "ops-table", "title": title, "widget": "table", "source": {"op": "ops.list"},
               "columns": ["name", "class"]})
    };
    save(&part("Operations, short").to_string());
    save(&part("All operations").to_string());
    let listed = lab.json(
        "main",
        &["views", "parts", "--intent", "table board of operations"],
    );
    assert_eq!(listed[0]["name"], "ops-table", "{listed}");
    assert_eq!(listed[0]["version"], 2);

    let mut dup = part("Every operation");
    dup["name"] = json!("again");
    let board = json!({"name": "ops-board", "title": "Operations board", "widget": "stack", "views": [
        {"name": "ops-table", "version": 1},
        dup,
        {"name": "reads", "title": "Read operations", "widget": "table", "source": {"op": "ops.list"},
         "columns": ["name"], "filter": {"class": "read"}}
    ]});
    write(&composed.join("table.jsonc"), &board.to_string());
    let (out, answer) = lab.ask(&["table board of operations"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = answer["text"].as_str().unwrap();
    assert!(
        text.contains("- ops-table v2: All operations (table of ops.list $)"),
        "the request lists the fitting part: {text}"
    );
    composer_is_bound(text);

    // the stack as saved: three references, pinned
    let saved =
        std::fs::read_to_string(lab.home.join(".config/legionctl/views/ops-board.jsonc")).unwrap();
    let saved: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(
        saved["views"],
        json!([{"name": "ops-table", "version": 1}, {"name": "ops-table", "version": 2},
               {"name": "ops-board-reads", "version": 1}]),
        "{saved}"
    );
    let titles: Vec<&str> = answer["data"]["parts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["title"].as_str().unwrap())
        .collect();
    assert_eq!(
        titles,
        ["Operations, short", "All operations", "Read operations"]
    );
    // a new part version leaves the views that pin the old one as they are
    save(&part("Operations, third").to_string());
    let shown = lab.json("main", &["views", "render", "ops-board"]);
    assert_eq!(shown["parts"][0]["title"], "Operations, short");
    assert_eq!(shown["parts"][1]["title"], "All operations");
    let new_part = lab.json("main", &["views", "parts", "--intent", "read operations"]);
    assert!(
        new_part
            .as_array()
            .unwrap()
            .iter()
            .any(|p| p["name"] == "ops-board-reads"),
        "the new part is in the library: {new_part}"
    );

    // another agent is bound the same way
    let (again, answer) = lab.ask(&["table board again", "--agent", "other"]);
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    composer_is_bound(answer["text"].as_str().unwrap());
    assert_eq!(lab.composer_runs(), 2);
    drop(server);
}

/// A repository's view does not hide the user's view of that name (A-4): an agent's save into the repository's layer
/// leaves the user's view the one that shows and lists, and the repository's file waits for the user's answer.
fn repository_layer_below_the_users(lab: &Lab, members: &str) {
    let run = |args: &[&str], stdin: &str| {
        lab.op.run(
            &lab.repo,
            &[
                ("HOME", lab.home.to_str().unwrap()),
                ("LEGIONCTL_SESSION", "main"),
            ],
            args,
            stdin,
        )
    };
    let shared = members.replace(r#"["name", "archetype"]"#, r#"["name"]"#);
    let out = run(&["views", "save", "--repo", "."], &shared);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = lab.ok(
        "main",
        &lab.repo,
        &["views", "render", "members", "thread=1"],
    );
    assert!(
        text.contains("2 rows: name | archetype"),
        "the user's view shows: {text}"
    );
    let listed = lab.json("main", &["views", "list"]);
    let layers: Vec<&Value> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|v| &v["layer"])
        .filter(|l| *l != "seed")
        .collect();
    assert_eq!(layers, [&json!("user")], "{listed}");
}

/// What observer0 does at a wake (the stand-in ACP agent runs it with the wake prompt): it records its environment,
/// runs a missed lint in its overlay of main0's folder and tells main0 with the rule's counts (with a file `hold`, it
/// waits for a file `release` first), backfills the record of a checkpoint (else it nudges), sends its verification
/// at `declare ready`, and floods its answer once at a test change. With a file `crash`, it sends one hard-block and
/// kills its agent at each wake.
const OBSERVER: &str = r#"P="$FAKE_ACP_PROMPT"
env | grep -E '^(LEGIONCTL_MEMBER|CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS)=' > member-env.txt
ws=$(printf '%s\n' "$P" | sed -n 's/^main0 workdir: //p')
ev=$(printf '%s\n' "$P" | sed -n '/^New events since/,$p')
has() { printf '%s\n' "$ev" | grep -q "$1"; }
if has 'missed `make lint`'; then
  rule=$(printf '%s\n' "$ev" | sed -n 's/.*stop: rule \([0-9]*\) missed.*/\1/p' | tail -1)
  i=0
  while [ -e "$FAKE_ACP_SCRIPTS/hold" ] && [ ! -e "$FAKE_ACP_SCRIPTS/release" ] && [ $i -lt 600 ]; do sleep 0.1; i=$((i+1)); done
  out=$(make -s -C "$ws" lint) && legionctl thread send main0 notice "ran make lint for you ($out)" --rule "$rule"
fi
if has thread.checkpoint; then
  msg=$(printf '%s\n' "$ev" | grep thread.checkpoint | tail -1 | sed 's/.*"message":"\([^"]*\)".*/\1/')
  printf -- '- checkpoint: %s\n' "$msg" | legionctl dossier append --member observer0 port/plan.md \
    || legionctl thread send main0 reminder "record the checkpoint $msg in plan.md"
fi
if has 'declare ready'; then legionctl thread send main0 result "verification ok: suite passed"; fi
if has 'test change' && [ ! -e .flooded ]; then
  touch .flooded
  awk 'BEGIN { s = sprintf("%1000s", ""); gsub(/ /, "w", s); for (i = 0; i < 200; i++) print s }'
fi
if [ -e "$FAKE_ACP_SCRIPTS/crash" ]; then
  [ -e .blocked ] || { touch .blocked; legionctl thread send main0 hard-block "the suite fails" --tool workspace_checkpoint; }
  kill -9 "$PPID"
fi
echo done
"#;

/// A team lab: the stand-in ACP agent as the adapter of `fake`, `claude` and `codex` (each interactive command is
/// `true`), the members' scripts, a repository whose `make lint` writes a file, and main0: a session of `agent` on
/// dossier port (its id and its workspace).
struct Pool {
    lab: Lab,
    main: String,
    ws: PathBuf,
}

fn pool(tag: &str, agent: &str) -> Pool {
    pool_with(tag, agent, &[])
}

/// A team lab whose operator has `extra` in its environment too.
fn pool_with(tag: &str, agent: &str, extra: &[(&str, OsString)]) -> Pool {
    let mut lab = Lab::new(tag);
    let scripts = lab.home.join("scripts");
    write(&scripts.join("observer0.sh"), OBSERVER);
    write(&scripts.join("ui0.sh"), "echo ui0 has nothing to place\n");
    let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-acp-agent.py");
    let acp = json!(["python3", fake]);
    write(
        &lab.home.join(".config/legionctl/pool.jsonc"),
        &json!({"session": {"acp": {"fake": acp, "claude": acp, "codex": acp},
                "agents": {"fake": ["true"], "claude": ["true"], "codex": ["true"]},
                "scope": false, "writable": []}})
        .to_string(),
    );
    write(
        &lab.repo.join("Makefile"),
        "lint:\n\t@touch lint-ran && echo clean\n",
    );
    for args in [
        &["add", "Makefile"][..],
        &["commit", "--quiet", "-m", "lint"],
    ] {
        let out = Command::new("git")
            .arg("-C")
            .arg(&lab.repo)
            .args(args)
            .env("HOME", &lab.home)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    // the members' scripts reach the stand-in through the operator's environment
    lab.op.child.kill().unwrap();
    lab.op.child.wait().unwrap();
    lab.op = Lab::operator_with(
        &format!("{tag}p"),
        &lab.home,
        &[&pool_env(&scripts)[..], extra].concat(),
    );
    let launch = lab.launch(agent, "port");
    Pool {
        main: launch["id"].as_str().unwrap().to_string(),
        ws: PathBuf::from(launch["workdir"].as_str().unwrap()),
        lab,
    }
}

/// The operator's environment of a team lab: this checkout's agent plugin (archetypes, teams) and the scripts.
fn pool_env(scripts: &Path) -> [(&'static str, OsString); 2] {
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/agent-plugin");
    [
        ("LEGIONCTL_PLUGIN", plugin.into()),
        ("FAKE_ACP_SCRIPTS", scripts.into()),
    ]
}

impl Lab {
    /// `session start <agent> --dossier <dossier>`: the session's launch.
    fn launch(&self, agent: &str, dossier: &str) -> Value {
        let out = self.op.run(
            &self.repo,
            &[("HOME", self.home.to_str().unwrap())],
            &["session", "start", agent, "--dossier", dossier, "--json"],
            "",
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn show(&self, session: &str, thread: u32) -> Value {
        self.json(session, &["thread", "show", &thread.to_string()])
    }

    /// A team mode hook of the lead `session` for `teammate`: its exit code and stderr.
    fn team_hook(&self, session: &str, event: &str, teammate: &str) -> (i32, String) {
        let payload = claude_hook(
            event,
            &json!({"session_id": session, "cwd": self.repo, "teammate_name": teammate}),
        );
        let out = self.op.run(&self.repo, &[], &["hook", event], &payload);
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        )
    }
}

/// The summed cost of a member (`runs`, `tokens`, `over`), from `thread show`.
fn cost(view: &Value, member: &str) -> Value {
    view["costs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["member"] == member)
        .cloned()
        .unwrap_or_else(|| json!({"runs": 0, "tokens": 0, "over": 0}))
}

/// Wait (at most 60 s) until `done` holds.
fn until(what: &str, mut done: impl FnMut() -> bool) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    while !done() {
        assert!(std::time::Instant::now() < deadline, "timed out: {what}");
        std::thread::sleep(std::time::Duration::from_millis(200));
    }
}

/// `thread start --team vanilla` with a main agent that is neither Claude Code nor Codex: observer0 and ui0 join as
/// operator sessions, asleep, with no session yet. A stop with nothing missed wakes nobody; a code edit with no lint
/// before the stop wakes observer0, which runs the lint in its overlay (main0's folder stays as it was) and sends a
/// notice with the rule's counts, given once. main0's own lint wakes nobody. A test change wakes it again; its answer
/// runs past the wake's token budget, so the wake stops, its cost says so, and its events stay after the cursor. The
/// cost of main0 (the context its hooks got) is next to observer0's.
#[test]
fn a_team_thread_wakes_its_observer_only_on_a_trigger_and_within_its_budget() {
    let Pool { lab, main, ws } = pool("wake", "fake");
    let started = lab.ok(
        &main,
        &ws,
        &["thread", "start", "port the parser", "--team", "vanilla"],
    );
    for line in [
        "thread 1 started on dossier port with team vanilla: this session is main0",
        "observer0: thread-observer as an operator session, asleep until a trigger (pace on-trigger; per wake at most 40000 tokens, 300 s)",
        "ui0: ui-composer as an operator session, asleep until a trigger (pace on-trigger; per wake at most 15000 tokens, 60 s)",
        "rule 1: observer0 watches main0 for `make lint` before each reply ends",
    ] {
        assert!(
            started.lines().any(|l| l == line),
            "{line} missing:\n{started}"
        );
    }
    let view = lab.show(&main, 1);
    for name in ["observer0", "ui0"] {
        let m = Lab::member(&view, name);
        assert_eq!(
            [&m["run"], &m["state"], &m["wakes"], &m["session"]],
            [
                &json!("operator"),
                &json!("active"),
                &json!(0),
                &Value::Null
            ],
            "{name} sleeps: {m}"
        );
    }

    lab.hook(&main, &ws, "prompt", json!({"prompt": "port the lexer"}));
    assert_eq!(lab.stop(&main, false), None);
    assert_eq!(
        lab.wakes("observer0"),
        0,
        "a stop with nothing missed is no trigger"
    );

    lab.hook(&main, &ws, "prompt", json!({"prompt": "go on"}));
    lab.edit(&main, &ws, &ws.join("src/lexer.rs"));
    assert_eq!(lab.stop(&main, false), None);
    until("observer0's first wake", || {
        cost(&lab.show(&main, 1), "observer0")["runs"] == 1
    });
    let view = lab.show(&main, 1);
    assert!(
        !ws.join("lint-ran").exists(),
        "the lint ran in main0's folder"
    );
    let rule = &view["rules"][0];
    assert_eq!(
        [&rule["misses"], &rule["done"]],
        [&json!(1), &json!(1)],
        "{rule}"
    );
    assert_eq!(Lab::member(&view, "ui0")["wakes"], 0, "ui0 has no trigger");
    let env = std::fs::read_to_string(
        lab.home
            .join("worktrees/repo/port--observer0/member-env.txt"),
    )
    .unwrap();
    assert_eq!(
        env.trim(),
        "LEGIONCTL_MEMBER=1/observer0",
        "a member is no team lead"
    );
    let told = lab.hook(&main, &ws, "prompt", json!({"prompt": "next"}));
    let notice = told
        .lines()
        .find(|l| l.contains("notice from observer0"))
        .unwrap_or_default();
    assert!(
        notice.contains("ran make lint for you (clean) — 1st miss since")
            && notice.ends_with("1 done for you"),
        "{told}"
    );
    assert!(
        !lab.hook(&main, &ws, "prompt", json!({"prompt": "again"}))
            .contains("observer0")
    );

    lab.edit(&main, &ws, &ws.join("src/lexer.rs"));
    lab.bash(&main, &ws, "make lint");
    assert_eq!(lab.stop(&main, false), None);
    assert_eq!(lab.wakes("observer0"), 1, "main0 ran the lint itself");

    a_test_change_wakes_it_and_its_wake_stops_at_the_token_budget(&lab, &main, &ws);
}

/// A test change wakes observer0; its answer runs past the wake's token budget, so the wake stops, its cost says so,
/// and its events stay after the cursor. main0's cost (the context its hooks got) is next to observer0's.
fn a_test_change_wakes_it_and_its_wake_stops_at_the_token_budget(lab: &Lab, main: &str, ws: &Path) {
    lab.edit(main, ws, &ws.join("tests/lexer.rs"));
    assert_eq!(
        lab.wakes("observer0"),
        2,
        "a test change wakes the test review"
    );
    until("the wake over its budget", || {
        cost(&lab.show(main, 1), "observer0")["over"] == 1
    });
    let view = lab.show(main, 1);
    let change = view["log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "thread.test-change")
        .unwrap();
    assert!(
        Lab::member(&view, "observer0")["cursor"].as_i64() < change["id"].as_i64(),
        "the events of a wake over its budget stay for the next wake"
    );
    let (observer, main0) = (cost(&view, "observer0"), cost(&view, "main0"));
    assert_eq!(observer["runs"], 2);
    assert!(observer["tokens"].as_i64().unwrap() > 40_000, "{observer}");
    assert!(
        main0["runs"] == 1 && main0["tokens"].as_i64().unwrap() > 0,
        "the context given to main0 is its cost: {main0}"
    );
}

/// main0's subagent does main0's work (R3-26): its edit with no lint before main0's stop is main0's miss and wakes
/// observer0. Before the fix a subagent's calls counted for no member.
#[test]
fn an_edit_of_mains_subagent_counts_for_main() {
    let Pool { lab, main, ws } = pool("delegate", "fake");
    lab.ok(
        &main,
        &ws,
        &["thread", "start", "port the parser", "--team", "vanilla"],
    );
    lab.hook(&main, &ws, "prompt", json!({"prompt": "port the lexer"}));
    let fields = json!({"tool_name": "Edit", "agent_id": "sub-1", "agent_type": "general-purpose",
        "tool_input": {"file_path": ws.join("src/lexer.rs"), "old_string": "a", "new_string": "b"}});
    assert_eq!(lab.hook(&main, &ws, "post-tool", fields), "");
    assert_eq!(lab.stop(&main, false), None);
    assert_eq!(lab.wakes("observer0"), 1, "the subagent's edit is main0's");
}

/// A wake that fails before its prompt (R3-20: main0's folder has a config that does not resolve) ends like any
/// failed wake: within the restart limit, then the member fails. Before the fix the error skipped the end of the wake,
/// so the request stayed due and the operator started the wake again at once, without end.
#[test]
fn a_wake_that_fails_before_its_prompt_counts_against_the_restart_limit() {
    let Pool { lab, main, ws } = pool("unresolved", "fake");
    lab.ok(
        &main,
        &ws,
        &["thread", "start", "port the parser", "--team", "vanilla"],
    );
    write(
        &ws.join(".legionctl/broken.jsonc"),
        // a key that needs no answer of the user: its wrong value breaks the config at once
        r#"{ "load": { "disk_busy_percent": "many" } }"#,
    );
    lab.edit(&main, &ws, &ws.join("tests/lexer.rs"));
    until("observer0 failed", || {
        Lab::member(&lab.show(&main, 1), "observer0")["state"] == "failed"
    });
    let fails: Vec<Value> = lab.show(&main, 1)["log"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "thread.fail")
        .map(|r| r["payload"]["error"].clone())
        .collect();
    assert_eq!(fails.len(), 4, "one wake and 3 restarts: {fails:?}");
    assert!(
        fails[0]
            .as_str()
            .unwrap()
            .contains("the merged config is not valid"),
        "{fails:?}"
    );
}

/// A team thread imported to another account (R3-18) keeps what makes it a team: each member's run and config, and
/// the reply rule of the flow. Before the fix the copy had `run = self` members and a plain watch rule.
#[test]
fn an_imported_team_thread_keeps_the_runs_and_rules_of_its_members() {
    let Pool { lab, main, ws } = pool("import-team", "fake");
    lab.ok(
        &main,
        &ws,
        &["thread", "start", "port the parser", "--team", "vanilla"],
    );
    write(
        &lab.home.join(".clauth/profiles.toml"),
        "profiles = [\"work\"]\n",
    );
    lab.ok(&main, &ws, &["thread", "import", "1", "work"]);
    let copy = lab.show(&main, 2);
    let observer = Lab::member(&copy, "observer0");
    assert_eq!(
        [&observer["run"], &observer["config"], &observer["session"]],
        [&json!("operator"), &json!("thread-observer"), &Value::Null],
        "{observer}"
    );
    assert_eq!(copy["rules"][0]["reply"], true, "{copy:#}");
}

/// A backlog larger than the wake's token budget (R3-5): main0's messages fill the log past observer0's
/// `wake_tokens` before a test change wakes it. Each wake gets only the events that fit (half the budget), ends within
/// its budget, moves the cursor to the last event it got and wakes again for the rest, until the cursor passes the
/// trigger. Before the fix each wake held the whole backlog, stopped at once over its budget and kept its cursor.
#[test]
fn a_backlog_larger_than_the_wake_budget_goes_through_in_wakes_that_fit() {
    let Pool { lab, main, ws } = pool("backlog", "fake");
    write(&lab.home.join("scripts/observer0.sh"), "echo done\n");
    write(
        &lab.home.join(".config/legionctl/teams/tight.jsonc"),
        &json!({"name": "tight", "members": {
            "main0": {"agent": "main-agent"},
            "observer0": {"agent": "thread-observer", "pace": "on-trigger",
                          "budget": {"wake_tokens": 4000, "wake_seconds": 60}}}})
        .to_string(),
    );
    lab.ok(
        &main,
        &ws,
        &["thread", "start", "drain the backlog", "--team", "tight"],
    );
    let text = "x".repeat(4000);
    for _ in 0..6 {
        let fields =
            json!({"tool_name": "SendMessage", "tool_input": {"to": "reviewer", "message": text}});
        assert_eq!(lab.hook(&main, &ws, "post-tool", fields), "");
    }
    lab.edit(&main, &ws, &ws.join("tests/lexer.rs"));
    let change = |view: &Value| {
        view["log"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["kind"] == "thread.test-change")
            .map(|r| r["id"].as_i64().unwrap())
    };
    until("observer0's cursor past the test change", || {
        let view = lab.show(&main, 1);
        change(&view).is_some_and(|c| Lab::member(&view, "observer0")["cursor"].as_i64() >= Some(c))
    });
    let observer = cost(&lab.show(&main, 1), "observer0");
    assert_eq!(observer["over"], 0, "{observer}");
    assert!(observer["runs"].as_i64() >= Some(3), "{observer}");
}

/// The process job with main0's dossier: a checkpoint while main0 runs wakes observer0, whose backfill is refused,
/// so it nudges; after main0's stop the next checkpoint's record is backfilled, marked `(by observer0)`, told once,
/// and `thread compensate` removes exactly that line. The flow: `declare done` waits for observer0's verification;
/// `declare ready` wakes it, its result passes the gate, and `declare done` ends the pool.
#[test]
fn the_observer_backfills_only_while_main_is_idle_and_the_flow_gates_done() {
    let Pool { lab, main, ws } = pool("backfill", "fake");
    lab.ok(
        &main,
        &ws,
        &["thread", "start", "port the parser", "--team", "vanilla"],
    );
    let plan = lab.home.join("agentWorkspaces/port/plan.md");
    let before = std::fs::read_to_string(&plan).unwrap();

    lab.hook(&main, &ws, "prompt", json!({"prompt": "port the lexer"}));
    write(&ws.join("src/a.rs"), "pub fn a() {}\n");
    lab.ok(&main, &ws, &["workspace", "checkpoint", "first step"]);
    until("the wake at the first checkpoint", || {
        cost(&lab.show(&main, 1), "observer0")["runs"] == 1
    });
    assert_eq!(
        std::fs::read_to_string(&plan).unwrap(),
        before,
        "a backfill while main0 runs"
    );
    let answers = lab.json(&main, &["jobs", "list"]).to_string();
    assert!(
        answers.contains("main0 is running: a backfill waits for its idle window"),
        "{answers}"
    );
    let told = lab.hook(&main, &ws, "prompt", json!({"prompt": "go on"}));
    assert!(
        told.contains(
            "reminder from observer0 (thread 1): record the checkpoint first step in plan.md"
        ),
        "{told}"
    );

    assert_eq!(lab.stop(&main, false), None);
    write(&ws.join("src/b.rs"), "pub fn b() {}\n");
    lab.ok(&main, &ws, &["workspace", "checkpoint", "second step"]);
    until("the wake at the second checkpoint", || {
        cost(&lab.show(&main, 1), "observer0")["runs"] == 2
    });
    let line = "- checkpoint: second step (by observer0)";
    assert_eq!(
        std::fs::read_to_string(&plan).unwrap(),
        format!("{before}{line}\n")
    );
    let view = lab.show(&main, 1);
    let backfill = view["effects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["kind"] == "backfill")
        .unwrap();
    let id = backfill["id"].to_string();
    let told = lab.hook(&main, &ws, "prompt", json!({"prompt": "next"}));
    assert!(
        told.contains(&format!(
            "observer0 wrote for you: port/plan.md: {line} (undo: legionctl thread compensate {id})"
        )),
        "{told}"
    );
    assert!(
        !lab.hook(&main, &ws, "prompt", json!({"prompt": "again"}))
            .contains("wrote for you")
    );
    // a session outside the thread cannot undo it (R3-28)
    assert!(
        lab.refused("stranger", &["thread", "compensate", &id])
            .contains("only its main0 or observer0 undoes it")
    );
    lab.ok(&main, &ws, &["thread", "compensate", &id]);
    assert_eq!(std::fs::read_to_string(&plan).unwrap(), before);
    assert!(
        lab.refused(&main, &["thread", "compensate", &id])
            .contains("is undone already")
    );

    the_flow_gates_done_on_a_verification_of_mains_newest_work(&lab, &main, &ws);
    a_branch_keeps_the_log_up_to_the_first_checkpoint(&lab, &main);
}

/// The flow's `declare:done` (R3-21, R3-22): observer0's own `declare done` neither passes the gate nor ends the pool;
/// main0's waits for observer0's verification, a checkpoint after it makes the result old, and the verification of the
/// newest work passes the gate and ends the pool.
fn the_flow_gates_done_on_a_verification_of_mains_newest_work(lab: &Lab, main: &str, ws: &Path) {
    // a member's declare done is its own: no gate, no verification, no stop of the pool (R3-21)
    let observer = Lab::member(&lab.show(main, 1), "observer0")["session"]
        .as_str()
        .unwrap()
        .to_string();
    lab.ok(&observer, ws, &["thread", "declare", "done"]);
    assert_eq!(lab.show(main, 1)["thread"]["state"], "open");
    let open = lab.refused(main, &["thread", "declare", "done"]);
    assert!(
        open.contains("observer0: result verification ok: no result from observer0 yet"),
        "{open}"
    );
    lab.ok(main, ws, &["thread", "declare", "ready"]);
    until("the verification wake", || {
        cost(&lab.show(main, 1), "observer0")["runs"] == 3
    });
    // a checkpoint after the verification makes its result old (R3-22)
    write(&ws.join("src/late.rs"), "pub fn late() {}\n");
    lab.ok(main, ws, &["workspace", "checkpoint", "late step"]);
    let stale = lab.refused(main, &["thread", "declare", "done"]);
    assert!(
        stale.contains("its newest result is older than main0's last checkpoint or declare ready"),
        "{stale}"
    );
    lab.ok(main, ws, &["thread", "declare", "ready"]);
    until("the second verification", || {
        let view = lab.show(main, 1);
        let results = view["messages"].as_array().unwrap().iter();
        results
            .filter(|m| m["from"] == "observer0" && m["kind"] == "result")
            .count()
            == 2
    });
    lab.ok(main, ws, &["thread", "declare", "done"]);
    let view = lab.show(main, 1);
    assert_eq!(view["thread"]["state"], "closed");
    for name in ["main0", "observer0", "ui0"] {
        assert_eq!(Lab::member(&view, name)["state"], "ended", "{name}");
    }
}

/// A branch of the ended pool at the first checkpoint's row: the log up to it, the members (active again) and rules,
/// main0 the caller, a git branch at that checkpoint's commit; the parent stays as it is.
fn a_branch_keeps_the_log_up_to_the_first_checkpoint(lab: &Lab, main: &str) {
    let parent = lab.show(main, 1);
    let log = parent["log"].as_array().unwrap();
    let first = log
        .iter()
        .find(|r| r["kind"] == "thread.checkpoint")
        .unwrap();
    let at = first["id"].to_string();
    let said = lab.thread(main, &["branch", "1", &at]);
    assert!(said.contains("thread 2 is a branch of thread 1"), "{said}");
    let branch = lab.show(main, 2);
    let kept: Vec<&Value> = branch["log"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| &r["payload"]["from"])
        .collect();
    let upto: Vec<Value> = log
        .iter()
        .filter(|r| r["id"].as_i64() <= first["id"].as_i64())
        .map(|r| r["id"].clone())
        .collect();
    assert_eq!(
        kept[..upto.len()],
        upto.iter().collect::<Vec<_>>()[..],
        "the parent's rows up to the point"
    );
    assert_eq!(
        branch["log"].as_array().unwrap().len(),
        upto.len() + 1,
        "and the branch row"
    );
    assert_eq!(Lab::member(&branch, "main0")["session"], main);
    assert_eq!(
        Lab::member(&branch, "observer0")["session"],
        Value::Null,
        "a new session at its first wake"
    );
    assert_eq!(branch["rules"][0]["expect"], "make lint");
    let tip = Command::new("git")
        .arg("-C")
        .arg(&lab.repo)
        .args(["rev-parse", "legionctl/thread-2"])
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&tip.stdout).trim(),
        first["payload"]["commit"].as_str().unwrap()
    );
    assert_eq!(
        lab.show(main, 1)["log"],
        parent["log"],
        "the parent is unchanged"
    );
}

/// An operator restart in the middle of a wake: the wake goes on in the member's loaded ACP session, from the same
/// cursor, and main0 gets its notice once.
#[test]
fn an_operator_restart_restores_the_members_and_a_cut_off_wake_goes_on() {
    let Pool { mut lab, main, ws } = pool("restart", "fake");
    let scripts = lab.home.join("scripts");
    write(&scripts.join("hold"), "");
    lab.ok(
        &main,
        &ws,
        &["thread", "start", "port the parser", "--team", "vanilla"],
    );
    lab.hook(&main, &ws, "prompt", json!({"prompt": "go on"}));
    lab.edit(&main, &ws, &ws.join("src/lexer.rs"));
    assert_eq!(lab.stop(&main, false), None);
    let jobs = |lab: &Lab| -> Vec<Value> {
        lab.json(&main, &["jobs", "list"])["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|j| j["kind"] == "thread.wake")
            .cloned()
            .collect()
    };
    until("the wake's ACP session", || {
        jobs(&lab)
            .first()
            .is_some_and(|j| j["input"]["acp"].is_string())
    });
    let acp = jobs(&lab)[0]["input"]["acp"].as_str().unwrap().to_string();
    lab.op.child.kill().unwrap();
    lab.op.child.wait().unwrap();
    // the cut-off wake's overlay layer (its work folder has mode 000) is in the old runtime folder
    Command::new("chmod")
        .args(["-R", "u+rwx"])
        .arg(&*lab.op.runtime)
        .status()
        .unwrap();
    lab.op = Lab::operator_with("restart2", &lab.home, &pool_env(&scripts));
    write(&scripts.join("release"), "");
    until("the wake after the restart", || {
        cost(&lab.show(&main, 1), "observer0")["runs"] == 1
    });
    let view = lab.show(&main, 1);
    assert!(
        view["log"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "thread.cut-off" && r["payload"]["acp"] == acp),
        "the cut-off wake kept its ACP session"
    );
    let events: Value = lab.json(&main, &["events", "--limit", "1000"]);
    assert!(
        events["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["payload"]["text"].as_str() == Some(&format!("acp: loaded {acp}\n"))),
        "the wake goes on in the loaded session"
    );
    let told = lab.hook(&main, &ws, "prompt", json!({"prompt": "next"}));
    assert_eq!(told.matches("ran make lint for you").count(), 1, "{told}");
    assert!(
        !lab.hook(&main, &ws, "prompt", json!({"prompt": "again"}))
            .contains("ran make lint")
    );
}

/// Team mode: a Claude Code session that legionctl starts may lead a team (the env var), and vanilla's members are
/// its teammates. `TeammateIdle` gives a teammate its new messages once (exit 2); a teammate's hard-block holds a task
/// (`TaskCompleted`, exit 2) until main0 acks it; the `SessionEnd` of /clear or /resume keeps the pool open, and after
/// /resume the teammates are lost and run as operator sessions, woken at once.
#[test]
fn a_claude_lead_runs_the_team_as_teammates_and_a_lost_one_restarts_as_an_operator_member() {
    let Pool { lab, main, ws } = pool("team", "claude");
    let sandbox = lab.launch("claude", "lead")["sandbox"].to_string();
    assert!(
        sandbox.contains(r#""--setenv","CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS","1""#),
        "{sandbox}"
    );
    let started = lab.ok(
        &main,
        &ws,
        &["thread", "start", "port the parser", "--team", "vanilla"],
    );
    assert!(
        started.contains("observer0: team mode: spawn it as the teammate observer0 (agent type legionctl:thread-observer)"),
        "{started}"
    );
    assert_eq!(
        Lab::member(&lab.show(&main, 1), "observer0")["run"],
        "teammate"
    );

    assert_eq!(
        lab.team_hook(&main, "teammate-idle", "observer0"),
        (0, String::new())
    );
    lab.ok(
        &main,
        &ws,
        &[
            "thread",
            "send",
            "observer0",
            "note",
            "look at the lexer tests",
        ],
    );
    let (code, said) = lab.team_hook(&main, "teammate-idle", "observer0");
    assert!(
        code == 2 && said.contains("note from main0: look at the lexer tests"),
        "{code}: {said}"
    );
    assert_eq!(
        lab.team_hook(&main, "teammate-idle", "observer0"),
        (0, String::new())
    );

    let sent = lab.ok(
        &main,
        &ws,
        &[
            "thread",
            "send",
            "main0",
            "hard-block",
            "weak test in tests/lexer.rs",
            "--teammate",
            "observer0",
        ],
    );
    let id = sent
        .split_whitespace()
        .nth(1)
        .unwrap()
        .trim_start_matches('#')
        .to_string();
    let (code, said) = lab.team_hook(&main, "task-completed", "observer0");
    assert!(
        code == 2
            && said.contains(&format!(
                "observer0: no open hard-block: hard-block #{id} is open: weak test"
            )),
        "{code}: {said}"
    );
    let held = lab.refused(&main, &["workspace", "checkpoint", "while blocked"]);
    assert!(
        held.contains("the checkpoint gate holds: thread 1: observer0: no open hard-block"),
        "{held}"
    );
    lab.ok(&main, &ws, &["thread", "ack", &id]);
    assert_eq!(
        lab.team_hook(&main, "task-completed", "observer0"),
        (0, String::new())
    );

    // Claude Code ends the session with reason resume (R3-4), and /clear the same way: the pool stays open
    lab.hook(&main, &ws, "session-end", json!({"reason": "clear"}));
    lab.hook(&main, &ws, "session-end", json!({"reason": "resume"}));
    assert_eq!(lab.show(&main, 1)["thread"]["state"], "open");
    let resumed = lab.hook(&main, &ws, "session-start", json!({}));
    assert!(
        resumed.contains(
            "the teammate observer0 was lost at /resume; it now runs as an operator session"
        ),
        "{resumed}"
    );
    until("observer0 and ui0 as operator sessions", || {
        let view = lab.show(&main, 1);
        cost(&view, "observer0")["runs"] == 1 && cost(&view, "ui0")["runs"] == 1
    });
    let m = lab.show(&main, 1);
    assert_eq!(Lab::member(&m, "observer0")["run"], "operator");
    assert!(Lab::member(&m, "observer0")["session"].is_string());
}

/// A Codex main agent gets vanilla's members as Codex subagents, which inherit its sandbox: a config that sets its
/// own sandbox refuses the team and names the field and the run path.
#[test]
fn a_codex_lead_refuses_a_member_config_that_sets_its_own_sandbox() {
    let Pool { lab, .. } = pool("codex", "fake");
    write(
        &lab.home.join(".config/legionctl/agents.jsonc"),
        r#"{ "agents": { "configs": { "thread-observer": { "sandbox": "workspace-write" } } } }"#,
    );
    let codex = lab.launch("codex", "other");
    let out = lab.op.run(
        &PathBuf::from(codex["workdir"].as_str().unwrap()),
        &[
            ("HOME", lab.home.to_str().unwrap()),
            ("LEGIONCTL_SESSION", codex["id"].as_str().unwrap()),
        ],
        &["thread", "start", "port the parser", "--team", "vanilla"],
        "",
    );
    let refused = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && refused.contains("observer0: sandbox (set by agents.configs.thread-observer) cannot apply on run path codex-subagent"),
        "{refused}"
    );
}

/// A member whose wake fails (its agent dies) starts again within `restart.max` (3) of vanilla's flow; the 4th failure
/// within the hour fails it: its hard-block expires, so it cannot hold main0, and main0 gets a note.
#[test]
fn a_failed_member_restarts_within_its_limit_then_its_blocks_expire() {
    let Pool { lab, main, ws } = pool("crash", "fake");
    write(&lab.home.join("scripts/crash"), "");
    lab.ok(
        &main,
        &ws,
        &["thread", "start", "port the parser", "--team", "vanilla"],
    );
    write(&ws.join("src/a.rs"), "pub fn a() {}\n");
    lab.ok(&main, &ws, &["workspace", "checkpoint", "first step"]);
    until("observer0 failed", || {
        Lab::member(&lab.show(&main, 1), "observer0")["state"] == "failed"
    });
    let view = lab.show(&main, 1);
    let fails = view["log"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["kind"] == "thread.fail")
        .count();
    assert_eq!(fails, 4, "one wake and 3 restarts");
    let block = view["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["kind"] == "hard-block")
        .unwrap();
    assert_eq!(block["state"], "expired");
    assert_eq!(lab.pre_tool(&main, "workspace_checkpoint"), None);
    let told = lab.hook(&main, &ws, "prompt", json!({"prompt": "go on"}));
    assert!(
        told.contains("note from observer0 (thread 1): observer0 failed 4 times within 1h:")
            && told.contains("It stopped; 1 of its blocks expired."),
        "{told}"
    );
}

/// The stand-in decision server (`tests/fixtures/fake-decisions.py`) and a stand-in keyring on the operator's `PATH`
/// (`secret-tool lookup legionctl-rlcd <entry>` prints `key-<entry>`), never the user's keyring. Returns the server,
/// its port, its log and the operator's environment.
fn decisions(
    home: &Path,
) -> (
    common::process::Kill,
    u16,
    PathBuf,
    Vec<(&'static str, OsString)>,
) {
    let (log, port_file) = (home.join("decisions.log"), home.join("decisions.port"));
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-decisions.py");
    let server = common::process::Kill::spawn(
        Command::new("python3")
            .arg(fixture)
            .arg(&port_file)
            .arg(&log),
    );
    let bin = home.join("bin");
    write(
        &bin.join("secret-tool"),
        "#!/bin/sh\nprintf 'key-%s' \"$3\"\n",
    );
    std::fs::set_permissions(
        bin.join("secret-tool"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let legionctl = Path::new(env!("CARGO_BIN_EXE_legionctl"))
        .parent()
        .unwrap()
        .to_path_buf();
    let path = std::env::join_paths(
        [bin, legionctl]
            .into_iter()
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let port = loop {
        if let Some(port) = std::fs::read_to_string(&port_file)
            .ok()
            .and_then(|p| p.trim().parse().ok())
        {
            break port;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the decision server did not start"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    };
    // a secret in the operator's environment: it never leaves the machine in a state
    let env = vec![
        ("PATH", path),
        ("DEPLOY_TOKEN", "hunter2-deploy-value".into()),
    ];
    (server, port, log, env)
}

/// The calls the decision server got: `{path, auth, body}`.
fn calls(log: &Path) -> Vec<Value> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

/// The user's rlcd backends `stand` (System One) and `oa` (`OpenAI`), which answer `pick`.
fn rlcd_backends(home: &Path, port: u16, pick: &str) {
    let url = |path: &str| format!("http://127.0.0.1:{port}/{pick}{path}");
    write(
        &home.join(".config/legionctl/rlcd.jsonc"),
        &json!({"rlcd": {"backends": {
            "stand": {"shape": "system-one", "url": url("/api/alpha/decisions"), "model": "stand-model",
                      "keyring": "stand", "timeout_ms": 1000},
            "oa": {"shape": "openai", "url": url("/v1/decisions"), "model": "oa-model", "keyring": "oa",
                   "timeout_ms": 1000}
        }}})
        .to_string(),
    );
}

/// A lab for `rlcd ask`: its operator reaches the stand-in decision server and the stand-in keyring.
struct AskLab {
    lab: Lab,
    port: u16,
    log: PathBuf,
    _server: common::process::Kill,
}

const QUESTIONS: [&str; 2] = [
    "action:choice:ignore,note=note it,escalate",
    "stuck:yesno:Is main0 stuck?",
];

impl AskLab {
    /// `rlcd ask <args> <QUESTIONS> --json` with `extra` in the client's environment.
    fn ask(&self, extra: &[(&str, &str)], args: &[&str]) -> std::process::Output {
        let mut env = vec![("HOME", self.lab.home.to_str().unwrap())];
        env.extend_from_slice(extra);
        self.lab.op.run(
            &self.lab.repo,
            &env,
            &[&["rlcd", "ask"], args, &QUESTIONS, &["--json"]].concat(),
            "",
        )
    }

    fn answer(&self, extra: &[(&str, &str)], args: &[&str]) -> Value {
        let out = self.ask(extra, args);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }

    fn refusal(&self, args: &[&str]) -> String {
        let out = self.ask(&[], args);
        assert!(!out.status.success(), "{args:?} passed");
        String::from_utf8_lossy(&out.stderr).into_owned()
    }

    fn last_call(&self) -> Value {
        calls(&self.log).pop().unwrap()
    }
}

/// `rlcd ask` against the stand-in server: the exact request body of each wire shape (System One: questions keyed by
/// id, a choice's options as criteria, yes-no as noul; `OpenAI`: questions in id order, a choice's options as
/// value/description objects, yes-no as predicate), the bearer key from the keyring entry, the state's secrets
/// removed (a well-known key form, an operator variable's value), the answers parsed per shape and stored. A key for a
/// manual check comes only from the client's environment (`--key-env`) and stays out of the audit. A 500 and a timeout
/// fail the call at once with the error and a stored row; no other backend gets the call. A repository layer selects a
/// backend (the dev layer picks the free model) but cannot point one at another URL.
#[test]
fn rlcd_ask_sends_each_wire_shape_and_fails_a_failed_call_at_once() {
    let mut lab = Lab::new("rlcd-ask");
    let (server, port, log, env) = decisions(&lab.home);
    lab.op.child.kill().unwrap();
    lab.op.child.wait().unwrap();
    lab.op = Lab::operator_with("rlcd-askp", &lab.home, &env);
    rlcd_backends(&lab.home, port, "escalate");
    let lab = AskLab {
        lab,
        port,
        log,
        _server: server,
    };
    each_wire_shape_goes_out_without_secrets(&lab);
    a_failed_call_fails_at_once_and_is_stored(&lab);

    // the repository layer: it selects a backend, and cannot point one elsewhere
    rlcd_backends(&lab.lab.home, port, "ignore");
    write(
        &lab.lab.repo.join(".legionctl/rlcd.jsonc"),
        r#"{"rlcd": {"backend": "oa"}}"#,
    );
    lab.answer(&[], &["--state", "x"]);
    assert_eq!(lab.last_call()["path"], "/ignore/v1/decisions");
    write(
        &lab.lab.repo.join(".legionctl/rlcd.jsonc"),
        r#"{"rlcd": {"backends": {"oa": {"url": "http://127.0.0.1:9/elsewhere"}}}}"#,
    );
    let moved = lab.refusal(&["--backend", "oa", "--state", "x"]);
    assert!(
        moved.contains("rlcd.backends.oa.url is set by the repository layer"),
        "{moved}"
    );
}

fn each_wire_shape_goes_out_without_secrets(lab: &AskLab) {
    let state = "5 edits, no lint; export KEY=sk-or-v1-0123456789abcdef; pass hunter2-deploy-value";
    let answer = lab.answer(&[], &["--backend", "stand", "--state", state]);
    let call = lab.last_call();
    assert_eq!(call["path"], "/escalate/api/alpha/decisions");
    assert_eq!(call["auth"], "Bearer key-stand");
    assert_eq!(
        call["body"],
        json!({"model": "stand-model", "state": "5 edits, no lint; export KEY=<secret>; pass <secret>",
               "questions": {
                   "action": {"type": "choice", "instructions": "action",
                              "criteria": {"escalate": "escalate", "ignore": "ignore", "note": "note it"}},
                   "stuck": {"type": "noul", "instructions": "Is main0 stuck?"}}})
    );
    assert_eq!(
        [
            &answer["path"],
            &answer["backend"],
            &answer["action"],
            &answer["input_tokens"]
        ],
        [
            &json!("ask"),
            &json!("stand"),
            &json!("escalate"),
            &json!(169)
        ],
        "{answer}"
    );
    assert_eq!(
        answer["answers"],
        json!({"action": {"type": "choice", "choice": "escalate", "confidence": 0.6,
                          "probabilities": {"escalate": 0.8, "ignore": 0.1, "note": 0.1}},
               "stuck": {"type": "yesno", "yes": 0.02}})
    );

    let answer = lab.answer(
        &[("RLCD_MANUAL", "manual-key-0000")],
        &[
            "--backend",
            "oa",
            "--key-env",
            "RLCD_MANUAL",
            "--state",
            "idle",
        ],
    );
    let call = lab.last_call();
    assert_eq!(
        [&call["path"], &call["auth"]],
        [
            &json!("/escalate/v1/decisions"),
            &json!("Bearer manual-key-0000")
        ]
    );
    assert_eq!(
        call["body"],
        json!({"model": "oa-model", "input": "idle", "questions": [
            {"type": "choice", "instructions": "action", "choices": [
                {"value": "escalate", "description": "escalate"}, {"value": "ignore", "description": "ignore"},
                {"value": "note", "description": "note it"}]},
            {"type": "predicate", "instructions": "Is main0 stuck?"}]})
    );
    assert_eq!(
        answer["answers"]["stuck"],
        json!({"type": "yesno", "yes": 0.3})
    );
    let audit = lab.lab.ok("", &lab.lab.repo, &["events", "--json"]);
    assert!(
        !audit.contains("manual-key-0000"),
        "the key is in the audit"
    );
    let unset = lab.refusal(&[
        "--backend",
        "oa",
        "--key-env",
        "NO_SUCH_VAR",
        "--state",
        "x",
    ]);
    assert!(
        unset.contains("the environment variable NO_SUCH_VAR is not set or empty"),
        "{unset}"
    );
}

fn a_failed_call_fails_at_once_and_is_stored(lab: &AskLab) {
    let before = calls(&lab.log).len();
    rlcd_backends(&lab.lab.home, lab.port, "fail");
    let failed = lab.refusal(&["--backend", "stand", "--state", "x"]);
    assert!(
        failed.contains("rlcd stand: http://127.0.0.1:")
            && failed.contains(
                "answered 500 Internal Server Error: {\"error\": {\"message\": \"Decision admission timed out.\""
            ),
        "{failed}"
    );
    rlcd_backends(&lab.lab.home, lab.port, "slow");
    let started = std::time::Instant::now();
    let slow = lab.refusal(&["--backend", "stand", "--state", "x"]);
    assert!(slow.contains("(timeout 1000 ms)"), "{slow}");
    // the stand-in answers after 60 s: an end within half of that is the 1000 ms limit, also on a loaded machine
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "the timeout did not end the call"
    );
    assert_eq!(
        calls(&lab.log).len(),
        before + 2,
        "one call each, no other backend"
    );
    let stored: Value = lab.lab.json("", &["rlcd", "decisions"]);
    let errors: Vec<bool> = stored
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["error"].is_string() && d["answers"].is_null())
        .collect();
    assert_eq!(errors, [true, true, false, false], "{stored}");
}

/// A team lab whose observer0 has the route [rlcd, acp] with the backend `stand`; its ACP script saves its prompt,
/// raises one hard-block and answers `checked the lint`.
struct RlcdPool {
    pool: Pool,
    port: u16,
    log: PathBuf,
    _server: common::process::Kill,
    _home: Scratch,
}

impl RlcdPool {
    fn new() -> RlcdPool {
        let home = Scratch::new(format!(
            "/var/tmp/legionctl-rlcd-srv-{}",
            std::process::id()
        ));
        let (server, port, log, env) = decisions(&home);
        let pool = pool_with("rlcd", "fake", &env);
        write(
            &pool.lab.home.join("scripts/observer0.sh"),
            "printf '%s\\n' \"$FAKE_ACP_PROMPT\" > prompt.txt\n\
             [ -e .blocked ] || { touch .blocked; legionctl thread send main0 hard-block \"the suite fails\" --tool workspace_checkpoint; }\n\
             echo checked the lint\n",
        );
        write(
            &pool.lab.home.join(".config/legionctl/observer.jsonc"),
            &json!({"agents": {"configs": {"thread-observer": {
                "route": ["rlcd", "acp"],
                "rlcd": {"backend": "stand", "questions": {
                    "action": {"type": "choice", "instructions": "What must observer0 do now?",
                               "options": {"ignore": "nothing to check", "note": "keep a note",
                                           "escalate": "wake observer0"}},
                    "stuck": {"type": "yesno", "instructions": "Is main0 stuck?"}}}}}}})
            .to_string(),
        );
        rlcd_backends(&pool.lab.home, port, "ignore");
        RlcdPool {
            pool,
            port,
            log,
            _server: server,
            _home: home,
        }
    }

    fn answers(&self, pick: &str) {
        rlcd_backends(&self.pool.lab.home, self.port, pick);
    }

    fn show(&self) -> Value {
        self.pool.lab.show(&self.pool.main, 1)
    }

    /// observer0's costs: kind -> [runs, failed].
    fn kinds(&self) -> Value {
        let rows: serde_json::Map<String, Value> = self.show()["costs"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|c| c["member"] == "observer0")
            .map(|c| {
                (
                    c["kind"].as_str().unwrap().to_string(),
                    json!([c["runs"], c["failed"]]),
                )
            })
            .collect();
        Value::Object(rows)
    }

    /// A code edit with no lint before the stop: a miss that wakes observer0.
    fn miss(&self, prompt: &str) {
        let Pool { lab, main, ws } = &self.pool;
        lab.hook(main, ws, "prompt", json!({"prompt": prompt}));
        lab.edit(main, ws, &ws.join("src/lexer.rs"));
        // after the escalated wake, observer0's hard-block holds each stop
        lab.stop(main, false);
    }

    fn last_rlcd_event(&self) -> Value {
        self.show()["log"]
            .as_array()
            .unwrap()
            .iter()
            .rfind(|r| r["kind"] == "thread.rlcd")
            .unwrap()["payload"]
            .clone()
    }

    fn checkpoint_held(&self) -> bool {
        self.pool
            .lab
            .pre_tool(&self.pool.main, "workspace_checkpoint")
            .is_some()
    }
}

/// observer0 of vanilla with the route [rlcd, acp]: each wake first asks the stand-in decision server its questions
/// on the wake's events (the secrets of a message to it removed). ignore records a cost row and a thread.rlcd event
/// and wakes no ACP session; escalate wakes the ACP session with the rlcd answer as its brief, and that answer is the
/// decision's outcome; a 500 is a failed step (event and cost row with the error) that does not wake the ACP
/// session, and the member goes on. The hard-block that the escalated wake raised stays open through the rlcd steps
/// after it (rlcd never clears a gate), and the next gate result is the outcome of each decision.
#[test]
fn an_observer_routed_through_rlcd_wakes_acp_only_on_escalate() {
    let r = RlcdPool::new();
    let Pool { lab, main, ws } = &r.pool;
    lab.ok(
        main,
        ws,
        &["thread", "start", "port the parser", "--team", "vanilla"],
    );
    lab.ok(
        main,
        ws,
        &[
            "thread",
            "send",
            "observer0",
            "note",
            "deploy key sk-or-v1-0123456789abcdef, pass hunter2-deploy-value",
        ],
    );
    r.miss("port the lexer");
    until("the rlcd step of the first wake", || {
        r.kinds() == json!({"rlcd": [1, 0]})
    });
    let call = calls(&r.log).pop().unwrap();
    assert_eq!(
        [&call["path"], &call["auth"]],
        [
            &json!("/ignore/api/alpha/decisions"),
            &json!("Bearer key-stand")
        ]
    );
    assert_eq!(
        call["body"]["questions"],
        json!({"action": {"type": "choice", "instructions": "What must observer0 do now?",
                          "criteria": {"escalate": "wake observer0", "ignore": "nothing to check",
                                       "note": "keep a note"}},
               "stuck": {"type": "noul", "instructions": "Is main0 stuck?"}})
    );
    let state = call["body"]["state"].as_str().unwrap();
    assert!(
        state.contains("stop: rule 1 missed `make lint`")
            && state.contains("deploy key <secret>, pass <secret>")
            && !state.contains("hunter2")
            && !state.contains("sk-or-v1"),
        "{state}"
    );
    assert_eq!(
        Lab::member(&r.show(), "observer0")["session"],
        Value::Null,
        "ignore woke the ACP session"
    );
    let ignored = r.last_rlcd_event();
    assert_eq!(
        [&ignored["action"], &ignored["escalated"], &ignored["error"]],
        [&json!("ignore"), &json!(false), &Value::Null],
        "{ignored}"
    );
    escalate_wakes_acp_with_the_brief(&r);
    a_failed_step_wakes_no_acp_and_clears_no_gate(&r);
}

fn escalate_wakes_acp_with_the_brief(r: &RlcdPool) {
    r.answers("escalate");
    r.miss("go on");
    until("the escalated wake", || {
        r.kinds() == json!({"rlcd": [2, 0], "wake": [1, 0]})
    });
    let prompt = std::fs::read_to_string(
        r.pool
            .lab
            .home
            .join("worktrees/repo/port--observer0/prompt.txt"),
    )
    .unwrap();
    assert!(
        prompt.contains(
            "rlcd (stand, stand-model) escalated this wake: action escalate p 0.80 (ignore 0.10, note 0.10), \
             confidence 0.60; stuck yes 0.02"
        ) && prompt.contains("New events since your last wake"),
        "{prompt}"
    );
    assert!(
        r.checkpoint_held(),
        "the escalated wake raised no hard-block"
    );
}

fn a_failed_step_wakes_no_acp_and_clears_no_gate(r: &RlcdPool) {
    r.answers("fail");
    r.miss("again");
    until("the failed rlcd step", || {
        r.kinds() == json!({"rlcd": [3, 1], "wake": [1, 0]})
    });
    let failed = r.last_rlcd_event();
    assert!(
        failed["error"]
            .as_str()
            .unwrap()
            .contains("answered 500 Internal Server Error")
            && failed["action"].is_null(),
        "{failed}"
    );
    r.answers("ignore");
    r.miss("once more");
    until("an rlcd step after the failure", || {
        r.kinds() == json!({"rlcd": [4, 1], "wake": [1, 0]})
    });
    assert_eq!(
        Lab::member(&r.show(), "observer0")["state"],
        "active",
        "a failed step failed the member"
    );
    assert!(r.checkpoint_held(), "an rlcd step cleared the hard-block");

    let Pool { lab, main, .. } = &r.pool;
    let refused = lab.refused(main, &["thread", "declare", "done"]);
    assert!(
        refused.contains("no result from observer0 yet"),
        "{refused}"
    );
    let stored = lab.json(main, &["rlcd", "decisions"]);
    let rows: Vec<Value> = stored
        .as_array()
        .unwrap()
        .iter()
        .rev()
        .map(|d| {
            let answered = d["acp"]["text"]
                .as_str()
                .map(|t| t.ends_with("checked the lint\n"));
            json!([
                d["action"],
                d["error"].is_string(),
                d["acp"]["ok"],
                answered,
                d["gate"]["gate"],
                d["gate"]["passed"]
            ])
        })
        .collect();
    assert_eq!(
        rows,
        [
            json!(["ignore", false, null, null, "declare:done", false]),
            json!(["escalate", false, true, true, "declare:done", false]),
            json!([null, true, null, null, "declare:done", false]),
            json!(["ignore", false, null, null, "declare:done", false]),
        ],
        "{stored}"
    );
}

/// A team lab for the account tests: team acct (main0, and observer0 of config observer-acct: Claude, account work),
/// three clauth profiles with a Claude login each (work, spare, third), the stand-in usage endpoint and keyring, and
/// the user's fallback list [work, spare] at 90%. observer0 writes the access token of the login it runs on into its
/// working folder.
struct AcctLab {
    pool: Pool,
    /// Account -> its 5h use in clauth's status feed.
    used: std::cell::RefCell<std::collections::BTreeMap<String, f64>>,
    _home: Scratch,
}

impl AcctLab {
    fn new(tag: &str) -> AcctLab {
        let home = Scratch::new(format!(
            "/var/tmp/legionctl-thread-{tag}x-{}",
            std::process::id()
        ));
        let bin = Path::new(env!("CARGO_BIN_EXE_legionctl"))
            .parent()
            .unwrap()
            .to_path_buf();
        let path = std::env::join_paths(
            std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
        )
        .unwrap();
        let pool = pool_with(
            tag,
            "fake",
            &[("PATH", keyring::keyring_path(&home, &path))],
        );
        let a = AcctLab {
            pool,
            used: std::cell::RefCell::default(),
            _home: home,
        };
        let Pool { lab, main, ws } = &a.pool;
        clauth::install(&lab.home);
        write(
            &lab.home.join(".clauth/profiles.toml"),
            "profiles = [\"work\", \"spare\", \"third\"]\n",
        );
        // Claude Code's folder, layered as by default: a session on an account gets the account's login there
        std::fs::create_dir_all(lab.home.join(".claude")).unwrap();
        let pool = lab.home.join(".config/legionctl/pool.jsonc");
        let mut config: Value =
            serde_json::from_str(&std::fs::read_to_string(&pool).unwrap()).unwrap();
        config["session"]["writable"] = json!(["~/.claude"]);
        write(&pool, &config.to_string());
        for (name, token) in [("work", "tok-w"), ("spare", "tok-s"), ("third", "tok-t")] {
            write(
                &lab.home
                    .join(format!(".clauth/profiles/{name}/credentials.json")),
                &format!(
                    r#"{{"claudeAiOauth": {{"accessToken": "{token}", "expiresAt": 4102444800000}}}}"#
                ),
            );
            a.used(name, 10.0);
        }
        write(
            &lab.home.join(".config/legionctl/accounts.jsonc"),
            &json!({"agents": {
                "configs": {"observer-acct": {"archetype": "thread-observer", "harness": "claude", "account": "work",
                                              "pace": "on-trigger", "capabilities": ["web-read"]}}}})
            .to_string(),
        );
        write(
            &lab.home.join(".config/legionctl/teams/acct.jsonc"),
            &json!({"name": "acct", "members": {"main0": {"agent": "main-agent"}, "observer0": {"agent": "observer-acct"}},
                    "flow": {"restart": {"max": 0, "within": "1h"}}})
            .to_string(),
        );
        write(
            &lab.home.join("scripts/observer0.sh"),
            "(grep -o 'tok-[a-z]' ~/.claude/.credentials.json || echo no-login) >> account-dirs.txt; echo checked\n",
        );
        lab.ok(
            main,
            ws,
            &["thread", "start", "port the parser", "--team", "acct"],
        );
        a
    }

    /// Account `name` has its 5h window at `five` percent in clauth's status feed, where work, spare and third are
    /// clauth's fallback chain in that order, moving on at 90%.
    fn used(&self, name: &str, five: f64) {
        self.used.borrow_mut().insert(name.into(), five);
        let feed: Vec<Value> = ["work", "spare", "third"]
            .iter()
            .enumerate()
            .filter_map(|(i, n)| {
                let five = *self.used.borrow().get(*n)?;
                Some(clauth::profile(
                    n,
                    "claude",
                    false,
                    &[("5h", five), ("7d", 1.0)],
                    Some((i + 1, 90.0)),
                ))
            })
            .collect();
        clauth::status(&self.pool.lab.home, &feed);
    }

    /// The access token of account `name`'s login.
    fn profile(name: &str) -> String {
        format!("tok-{}", &name[..1])
    }

    /// The access tokens of the logins that observer0's wakes ran on, in order.
    fn dirs(&self) -> Vec<String> {
        std::fs::read_to_string(
            self.pool
                .lab
                .home
                .join("worktrees/repo/port--observer0/account-dirs.txt"),
        )
        .unwrap_or_default()
        .lines()
        .map(String::from)
        .collect()
    }

    /// A test change by main0 wakes observer0: its `n`th wake.
    fn wake(&self, n: usize) {
        let Pool { lab, main, ws } = &self.pool;
        lab.edit(main, ws, &ws.join(format!("tests/lexer{n}.rs")));
        until(&format!("wake {n}"), || self.dirs().len() == n);
    }
}

/// An operator member runs under its account and, at its limit, continues along clauth's fallback chain (todo-188;
/// S2 of planning/features/accounts.jsonc): observer0 wakes on work's login; with work's 5h at 95% (clauth's chain
/// moves on at 90%) its next wake resumes on spare's login (the next chain member under its limit) with a
/// thread.account row, a note to main0 and an agents.accounts.switch event. The user's pick (`thread account`)
/// takes effect at the next wake. When every other chain member is at its limit, the wake fails with the reason and
/// the member stops (V1).
#[test]
fn a_member_runs_under_its_account_and_resumes_under_the_next_at_its_limit() {
    let a = AcctLab::new("acct");
    let Pool { lab, main, ws } = &a.pool;
    a.used("work", 50.0);
    assert_eq!(
        Lab::member(&lab.show(main, 1), "observer0")["account"],
        "work"
    );
    a.wake(1);
    assert_eq!(a.dirs().last(), Some(&AcctLab::profile("work")));
    // its first wake's session has its config's capability set beside its harness set (DESIGN-13)
    let grant: Value =
        serde_json::from_str(&lab.ok(main, ws, &["runnables", "show", "1/observer0", "--json"]))
            .unwrap();
    let sets: Vec<&str> = grant["sets"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(sets, ["claude-code", "web-read"], "{grant:#}");

    a.used("work", 95.0);
    a.wake(2);
    assert_eq!(a.dirs().last(), Some(&AcctLab::profile("spare")));
    let view = lab.show(main, 1);
    assert_eq!(Lab::member(&view, "observer0")["account"], "spare");
    let switch = view["log"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["kind"] == "thread.account")
        .unwrap();
    assert_eq!(
        switch["payload"],
        json!({"account": "spare", "from": "work", "why": "account work has 5h at 95% (clauth's fallback chain moves on at 90%)"})
    );
    assert!(
        view["messages"].as_array().unwrap().iter().any(|m| m["kind"] == "note"
            && m["to"] == "main0"
            && m["body"] == "observer0 continues on account spare: account work has 5h at 95% (clauth's fallback chain moves on at 90%)"),
        "{:#}",
        view["messages"]
    );
    for m in view["messages"].as_array().unwrap() {
        if m["kind"] == "note" {
            plain::assert_plain("the accounts", m["body"].as_str().unwrap());
        }
    }
    let events: Value =
        serde_json::from_str(&lab.ok(main, ws, &["events", "--limit", "1000", "--json"])).unwrap();
    assert!(
        events["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "agents.accounts.switch"
                && e["payload"]["from"] == "work"
                && e["payload"]["to"] == "spare"),
        "{events:#}"
    );

    // the user's pick, at the next wake
    lab.ok(
        main,
        ws,
        &[
            "thread",
            "account",
            "1",
            "--account",
            "third",
            "--member",
            "observer0",
        ],
    );
    a.wake(3);
    assert_eq!(a.dirs().last(), Some(&AcctLab::profile("third")));
    no_account_under_the_limit_stops_the_member(&a);
}

/// V1: no other chain member is under its limit: the wake fails with the reason, main0 gets it, and no wake runs.
fn no_account_under_the_limit_stops_the_member(a: &AcctLab) {
    let Pool { lab, main, ws } = &a.pool;
    for name in ["work", "spare", "third"] {
        a.used(name, 92.0);
    }
    lab.edit(main, ws, &ws.join("tests/lexer4.rs"));
    until("observer0 stopped", || {
        Lab::member(&lab.show(main, 1), "observer0")["state"] == "failed"
    });
    let view = lab.show(main, 1);
    let why = "account third has 5h at 92% (clauth's fallback chain moves on at 90%) and no other account of the \
               chain is under its limit: work: 5h at 92%; spare: 5h at 92%";
    assert!(
        view["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["to"] == "main0" && m["body"].as_str().unwrap().contains(why)),
        "{:#}",
        view["messages"]
    );
    assert_eq!(a.dirs().len(), 3, "no wake ran on an account at its limit");
}

/// A wake whose member's account clauth does not show fails with the reason (S2 V3, user decision 2026-10-17): with
/// work out of clauth's status feed, observer0's wake does not run on work, it fails, and main0 gets the cause.
#[test]
fn a_wake_fails_when_clauth_does_not_show_its_account() {
    let a = AcctLab::new("acctread");
    let Pool { lab, main, ws } = &a.pool;
    a.used.borrow_mut().remove("work");
    a.used("spare", 10.0);
    lab.edit(main, ws, &ws.join("tests/lexer1.rs"));
    until("observer0 failed", || {
        Lab::member(&lab.show(main, 1), "observer0")["state"] == "failed"
    });
    let view = lab.show(main, 1);
    let why = "clauth does not show account work of Claude Code";
    assert!(
        view["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["to"] == "main0" && m["body"].as_str().unwrap().contains(why)),
        "{:#}",
        view["messages"]
    );
    assert!(a.dirs().is_empty(), "no wake ran: {:?}", a.dirs());
}
