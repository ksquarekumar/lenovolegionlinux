//! End-to-end: threads (design 23, Part B, P5) through the real `legionctl hook` adapter with Claude Code's hook
//! payloads (recorded in tests/fixtures/claude-code-hooks), the CLI and the real operator: watch rules and their
//! counts, delivery of the outbox through the prompt, pre-tool and stop hooks, `SendMessage` in the thread log, pace,
//! an operator restart, and presence.

mod common;

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::Operator;
use common::scratch::Scratch;
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
        let env = [
            ("LEGIONCTL_SYSROOT", home.join("root").into()),
            ("HOME", home.into()),
            ("PATH", path),
        ];
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

/// `thread import` (design 23, C5): thread 1 runs on the home's account; its import to the account profile `work`
/// makes thread 2 with the same log rows, members (without their sessions, which run on the old account), messages
/// and watch rules, then a `thread.import` row; it copies main0's Claude transcript into the account's folders. Thread
/// 1 is read-only after it: a second import (the copy exists), a message and a layout are refused. The copy refuses
/// an import to its own account and to an account profile that does not exist.
#[test]
fn a_thread_imported_to_another_account_copies_its_log_and_members_and_imports_once() {
    let lab = thread_lab("import");
    lab.thread("main", &["send", "observer0", "note", "check the lint"]);
    lab.thread("main", &["rules", "add", "1", "observer0", "make lint"]);
    let transcript = lab.home.join(".claude/projects/-repo/main.jsonl");
    write(
        &transcript,
        "{\"type\":\"user\",\"message\":\"port the parser\"}\n",
    );
    lab.ok("main", &lab.repo, &["agents", "accounts", "add", "work"]);
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
    let copied = lab
        .home
        .join(".local/share/legionctl/accounts/work/claude/projects/-repo/main.jsonl");
    assert_eq!(
        std::fs::read_to_string(&copied).unwrap(),
        std::fs::read_to_string(&transcript).unwrap(),
        "main0's transcript in the account's folders"
    );
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
    assert!(missing.contains("no account profile nope"), "{missing}");
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
    let chart = r#"{"name": "load", "widget": "chart", "source": {"op": "status"}}"#;
    let change = r#"{"name": "power", "widget": "tree", "source": {"op": "power.apply", "input": {"source": "auto"}}}"#;
    for (view, why) in [
        (
            chart,
            "unknown variant `chart`, expected one of `table`, `tree`, `gauge`, `sparkline`, `markdown`, `diff`, `list`, `stack`",
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
        "members (table)\nname | archetype\nmain0 | main-agent\nobserver0 | thread-observer"
    );
    let missing = run(&["views", "render", "members"], "");
    assert_eq!(
        (
            missing.status.success(),
            String::from_utf8_lossy(&missing.stdout).trim()
        ),
        (
            false,
            "members (table): cannot read: view members needs the parameter thread"
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

    // the repository's layer comes first
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
    assert_eq!(text, "members (table)\nname\nmain0\nobserver0");
    let listed = lab.json("main", &["views", "list"]);
    assert_eq!(listed.as_array().unwrap().len(), 1, "{listed}");
    assert_eq!(listed[0]["layer"], "repository");
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
    lab.op = Lab::operator_with(&format!("{tag}p"), &lab.home, &pool_env(&scripts));
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
        r#"{ "checks": { "keep_runs": "many" } }"#,
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
    lab.ok(&main, &ws, &["agents", "accounts", "add", "work"]);
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
