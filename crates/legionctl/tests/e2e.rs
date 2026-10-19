//! End-to-end: the binary as the agent plugin uses it (hook dispatch, MCP over stdio), in an isolated HOME.

#[path = "common/adapter.rs"]
mod adapter;
mod common;
#[path = "common/flood.rs"]
mod flood;
#[path = "common/git.rs"]
mod git;
#[path = "common/hooks.rs"]
mod hooks;
#[path = "common/keyring.rs"]
mod keyring;
#[path = "common/plain.rs"]
mod plain;
#[path = "common/provider.rs"]
mod provider;
#[path = "common/tokens.rs"]
mod tokens;
#[path = "common/trust.rs"]
mod trust;

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use common::Operator;
use common::process::{Kill, own};
use common::scratch::Scratch;
use git::git;
use hooks::claude_hook;
use serde_json::{Value, json};
use tokens::add_session_token;

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

/// An operator with an empty HOME and the repo's agent plugin.
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

/// A new home: outside /tmp (private in a session sandbox) and outside any git repository, like a real home.
fn home(tag: &str) -> Scratch {
    Scratch::new(format!(
        "/var/tmp/legionctl-e2e-{tag}-{}",
        std::process::id()
    ))
}

/// The operator of a new home, which goes with the operator.
fn operator(tag: &str) -> (Operator, PathBuf) {
    let home = home(tag);
    let mut op = operator_in(tag, &home);
    let path = home.to_path_buf();
    op.held.push(home);
    (op, path)
}

/// The operator of `home`, which keeps its files (an operator that starts again reads the config anew).
fn operator_in(tag: &str, home: &Path) -> Operator {
    operator_with(tag, home, &[])
}

/// An operator that reaches this machine's Docker, for the tests that build and run their own images and containers.
fn docker_operator(tag: &str) -> (Operator, PathBuf) {
    let home = home(tag);
    let mut op = operator_with(tag, &home, &docker());
    let path = home.to_path_buf();
    op.held.push(home);
    (op, path)
}

/// This machine's Docker (the harness points every other operator nowhere).
fn docker() -> [(&'static str, OsString); 1] {
    [(
        "DOCKER_HOST",
        std::env::var_os("DOCKER_HOST").unwrap_or_else(|| "unix:///var/run/docker.sock".into()),
    )]
}

fn operator_with(tag: &str, home: &Path, extra: &[(&str, OsString)]) -> Operator {
    operator_under(tag, home, extra, &[])
}

/// As [`operator_with`], with `wrapper` (a tracer) in front of the operator's socket activation.
fn operator_under(
    tag: &str,
    home: &Path,
    extra: &[(&str, OsString)],
    wrapper: &[OsString],
) -> Operator {
    // an empty machine: no /etc/legionctl unless a test writes one; the load and memory that every Linux has
    let root = home.join("root");
    std::fs::create_dir_all(root.join("proc")).unwrap();
    std::fs::write(root.join("proc/loadavg"), "0.10 0.10 0.10 1/100 100\n").unwrap();
    std::fs::create_dir_all(root.join("sys/devices/system/cpu")).unwrap();
    std::fs::write(root.join("sys/devices/system/cpu/online"), "0-3\n").unwrap();
    std::fs::write(
        root.join("proc/meminfo"),
        "MemTotal: 16000000 kB\nMemAvailable: 12000000 kB\n",
    )
    .unwrap();
    let mut env = vec![
        ("LEGIONCTL_SYSROOT", root.into()),
        ("HOME", home.into()),
        ("PATH", path_with_keyring(home)),
    ];
    env.extend(extra.iter().cloned());
    Operator::start_under(tag, &env, wrapper)
}

/// This build's `PATH` behind the stand-in keyring: no test reaches the user's real keyring.
fn path_with_keyring(home: &Path) -> OsString {
    keyring::keyring_path(home, &path_with_build())
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
    child: Kill,
    out: BufReader<std::process::ChildStdout>,
    next: u64,
}

impl Mcp {
    fn start(operator: &Operator, env: &[(&str, &str)]) -> Mcp {
        Self::start_with(operator, env, &[])
    }

    /// `legionctl mcp <args>`.
    fn start_with(operator: &Operator, env: &[(&str, &str)], args: &[&str]) -> Mcp {
        Self::spawn(operator, env, args, &std::env::current_dir().unwrap())
    }

    /// `legionctl mcp` in `folder`, as a harness starts it in the folder its agent starts in.
    fn start_in(operator: &Operator, env: &[(&str, &str)], folder: &Path) -> Mcp {
        Self::spawn(operator, env, &[], folder)
    }

    fn spawn(operator: &Operator, env: &[(&str, &str)], args: &[&str], folder: &Path) -> Mcp {
        let mut child = Kill::spawn(
            Command::new(env!("CARGO_BIN_EXE_legionctl"))
                .arg("mcp")
                .args(args)
                .current_dir(folder)
                .env_clear()
                .env("XDG_RUNTIME_DIR", &operator.runtime)
                .envs(env.iter().copied())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped()),
        );
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
        self.call_raw(method, params)["result"].clone()
    }

    /// A call of the tool `name` with `arguments`: its text, and whether it is an error.
    fn tool(&mut self, name: &str, arguments: &Value) -> (String, bool) {
        let result = self.call("tools/call", &json!({"name": name, "arguments": arguments}));
        (
            result["content"][0]["text"].as_str().unwrap().to_string(),
            result["isError"] == true,
        )
    }

    /// `legionctl_run` of `op` with `input`: its JSON output or error envelope, and whether it is an error.
    fn run(&mut self, op: &str, input: &Value) -> (Value, bool) {
        let (text, error) = self.tool("legionctl_run", &json!({"op": op, "input": input}));
        (serde_json::from_str(&text).unwrap(), error)
    }

    /// The whole reply: its result or its error.
    fn call_raw(&mut self, method: &str, params: &Value) -> Value {
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": self.next, "method": method, "params": params}));
        let mut line = String::new();
        self.out.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["id"], self.next, "{reply}");
        reply
    }
}

/// The git gate of a session sandbox fails closed (todo-142 F-3): when git cannot tell whether the subcommand is an
/// alias (here its config cannot be read: exit 128), the gate refuses with the reason and never runs the real git.
/// Before, a failed check read as "no alias" and the command ran.
#[test]
fn the_git_gate_refuses_when_git_cannot_check_for_an_alias() {
    let (op, home) = operator("gitgate");
    let repo = repository(&home);
    stand_in(
        &home,
        "git",
        "#!/bin/sh\ncase \"$*\" in *'config --get alias.'*) echo 'fatal: bad config line 1 in file .git/config' >&2; exit 128;; esac\nexec /usr/bin/git \"$@\"\n",
    );
    let gate = home.join("gate");
    std::fs::create_dir_all(&gate).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_legionctl"), gate.join("git")).unwrap();
    let ran = home.join("real-git-ran");
    let real = home.join("real-git");
    write(&real, &format!("#!/bin/sh\ntouch {}\n", ran.display()));
    std::fs::set_permissions(&real, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let out = Command::new(gate.join("git"))
        .args(["status"])
        .current_dir(&repo)
        .env_clear()
        .env("XDG_RUNTIME_DIR", &*op.runtime)
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("LEGIONCTL_REAL_GIT", &real)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && text.contains("legionctl gate: cannot check git aliases")
            && text.contains("bad config line 1"),
        "{text}"
    );
    assert!(!ran.exists(), "the real git ran");
}

/// A file the operator cannot read fails the call with its name, never reads as empty (todo-142 F-38, F-39, F-45):
/// a user config layer with a syntax error fails every call that resolves the config (before, the layer was dropped,
/// with its gates); a dossier whose `dossier.jsonc` cannot be read fails `dossier list` and `dossier search` (before,
/// it was listed without its id, and its files were not searched).
#[test]
fn a_config_layer_or_dossier_file_that_cannot_be_read_fails_with_its_name() {
    use std::os::unix::fs::PermissionsExt;
    let (op, home) = operator("unread");
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "s1"),
    ];
    let layer = home.join(".config/legionctl/mine.jsonc");
    write(&layer, r#"{ "session": { "writable": [] "#);
    let out = op.run(&home, &env, &["config", "show"], "");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success() && err.contains(&layer.display().to_string()),
        "{err}"
    );
    std::fs::remove_file(&layer).unwrap();

    let o = op.run(&home, &env, &["dossier", "use", "seen"], "");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let jsonc = home.join("agentWorkspaces/seen/dossier.jsonc");
    std::fs::set_permissions(&jsonc, std::fs::Permissions::from_mode(0o000)).unwrap();
    for args in [&["dossier", "list"][..], &["dossier", "search", "anything"]] {
        let out = op.run(&home, &env, args, "");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success()
                && err.contains(&format!("reading {}", jsonc.display()))
                && err.contains("Permission denied"),
            "{args:?}: {err}"
        );
    }
    std::fs::set_permissions(&jsonc, std::fs::Permissions::from_mode(0o644)).unwrap();
    let list = op.run(&home, &env, &["dossier", "list"], "");
    assert!(
        list.status.success(),
        "{}",
        String::from_utf8_lossy(&list.stderr)
    );
}

/// A user file that cannot be read is not written over (todo-142 F-9): an `info/exclude` in Latin-1 (not UTF-8)
/// where the dossier link needs its ignore line; `dossier use` fails with the reason and the file keeps every byte.
/// Before, the failed read gave an empty file, and the write left only legionctl's line.
#[test]
fn a_user_file_that_cannot_be_read_is_not_written_over() {
    let (op, home) = operator("latin1");
    let repo = repository(&home);
    git(&home, &repo, &["rm", "--quiet", ".gitignore"]);
    git(&home, &repo, &["commit", "--quiet", "-m", "no ignore line"]);
    let ignore = repo.join(".git/info/exclude");
    let before = b"target/\n# caf\xe9\n".to_vec();
    std::fs::write(&ignore, &before).unwrap();
    let out = op.run(
        &repo,
        &[
            ("HOME", home.to_str().unwrap()),
            ("LEGIONCTL_SESSION", "s1"),
        ],
        &["dossier", "use", "latin"],
        "",
    );
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && text.contains(&format!("reading {}", ignore.display()))
            && text.contains("valid UTF-8"),
        "{text}"
    );
    assert_eq!(
        std::fs::read(&ignore).unwrap(),
        before,
        "the user's file changed"
    );
}

/// A dossier write follows no symlink out of the dossier, also to a file that does not exist yet (todo-142 F-4):
/// `<dossier>/EVIDENCE` linked to a folder outside, and a link to a folder that is not there, are refused with the
/// reason, and nothing appears outside. Before, only a path that existed was checked.
#[test]
fn a_dossier_write_of_a_new_file_through_a_symlink_out_of_the_dossier_is_refused() {
    let (op, home) = operator("escape");
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "s1"),
    ];
    let o = op.run(&home, &env, &["dossier", "use", "esc"], "");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let dossier = home.join("agentWorkspaces/esc");
    let outside = home.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    for (link, to) in [("EVIDENCE", outside.clone()), ("SPIKE", home.join("gone"))] {
        let at = dossier.join(link);
        let _ = std::fs::remove_dir_all(&at);
        std::os::unix::fs::symlink(&to, &at).unwrap();
    }
    for (rel, why) in [
        (
            "esc/EVIDENCE/new.md",
            "esc/EVIDENCE/new.md leads out of the dossier",
        ),
        ("esc/SPIKE/new.md", "leads to no file"),
    ] {
        let o = op.run(&home, &env, &["dossier", "write", rel], "# escaped\n");
        let text = String::from_utf8_lossy(&o.stderr);
        assert!(!o.status.success() && text.contains(why), "{rel}: {text}");
    }
    assert!(
        !outside.join("new.md").exists(),
        "written outside the dossier"
    );
    assert!(!home.join("gone").exists(), "a folder outside was made");
}

/// A dossier write never follows a link planted at its temporary file's name (S-7 of the security review): the old
/// name was `.<file>.<operator pid>.tmp` in a folder that the agent writes, and the operator wrote the agent's text
/// through the link into any file of the user. The write goes on, and the file the link points to keeps its bytes.
#[test]
fn a_dossier_write_never_follows_a_link_planted_at_its_temporary_name() {
    let (op, home) = operator("plant");
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "s1"),
    ];
    let o = op.run(&home, &env, &["dossier", "use", "plant"], "");
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let runtime = op.runtime.to_string_lossy().into_owned();
    let operator_pid = std::fs::read_dir("/proc")
        .unwrap()
        .flatten()
        .find(|p| {
            std::fs::read(p.path().join("cmdline"))
                .is_ok_and(|c| String::from_utf8_lossy(&c).contains("legionctl-operator"))
                && std::fs::read(p.path().join("environ"))
                    .is_ok_and(|e| String::from_utf8_lossy(&e).contains(&runtime))
        })
        .expect("the operator's process")
        .file_name();
    let victim = home.join(".bashrc");
    std::fs::write(&victim, "# the user's own\n").unwrap();
    let dossier = home.join("agentWorkspaces/plant");
    std::os::unix::fs::symlink(
        &victim,
        dossier.join(format!(".plan.md.{}.tmp", operator_pid.to_string_lossy())),
    )
    .unwrap();
    let o = op.run(
        &home,
        &env,
        &["dossier", "append", "plant/plan.md"],
        "an agent's line\n",
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        std::fs::read_to_string(&victim).unwrap(),
        "# the user's own\n",
        "written through the planted link"
    );
    assert!(
        std::fs::read_to_string(dossier.join("plan.md"))
            .unwrap()
            .ends_with("an agent's line\n")
    );
}

/// A dossier write checks its links and plan rows against the dossier graph (todo-121): agents wrote
/// `supports: [todo-NN]` for `plan#todo-NN` 7 times, and two agents gave new plan rows the same ids. A link to no node
/// is refused, a bare plan row id with its right form, in `add`, `write` and `append`; a plan change that makes a row
/// id twice is refused, in `write`, `append` and `edit`; the right forms go through. Nothing changes on a refusal.
#[test]
fn a_dossier_write_refuses_a_link_to_no_node_and_a_plan_row_id_twice() {
    let (op, home) = operator("links");
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "s1"),
    ];
    let run = |args: &[&str], stdin: &str| op.run(&home, &env, args, stdin);
    let refused = |args: &[&str], stdin: &str, why: &str| {
        let o = run(args, stdin);
        let text = String::from_utf8_lossy(&o.stderr);
        assert!(
            !o.status.success() && text.contains(why),
            "{args:?}: {text}"
        );
    };
    assert!(run(&["dossier", "use", "links"], "").status.success());
    let plan = "# links\n\n## ToDo\n| id | item | status |\n| --- | --- | --- |\n| todo-1 | one | open |\n";
    let o = run(&["dossier", "write", "links/plan.md", "--overwrite"], plan);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let artifact =
        |to: &str| format!("---\nid: EV-1\nclass: EVIDENCE\nsupports: [{to}]\n---\n# One\n");
    let bare = "links to todo-1, which is no node: a plan row is plan#todo-1";
    let add = ["dossier", "add", "EVIDENCE", "one"];
    refused(&add, &artifact("todo-1"), bare);
    refused(
        &add,
        &artifact("plan#todo-9"),
        "plan#todo-9, which does not exist",
    );
    let one = ["dossier", "write", "links/EVIDENCE/one.md"];
    refused(&one, &artifact("todo-1"), bare);
    let evidence = home.join("agentWorkspaces/links/EVIDENCE");
    assert_eq!(
        std::fs::read_dir(&evidence).unwrap().count(),
        0,
        "nothing written"
    );
    let o = run(&one, &artifact("plan#todo-1"));
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    // an append gives an empty file its front matter
    assert!(
        run(&["dossier", "write", "links/MEMORY/m.md"], "")
            .status
            .success()
    );
    let append = ["dossier", "append", "links/MEMORY/m.md"];
    refused(&append, "---\nid: M-1\nsupports: [todo-1]\n---\n", bare);

    let twice = "plan.md would have the row plan#todo-1 twice";
    let row = "| todo-1 | again | open |";
    refused(&["dossier", "append", "links/plan.md"], row, twice);
    let overwrite = ["dossier", "write", "links/plan.md", "--overwrite"];
    refused(&overwrite, &format!("{plan}{row}\n"), twice);
    let read = run(&["dossier", "read", "links/plan.md"], "");
    let anchor = String::from_utf8_lossy(&read.stdout)
        .lines()
        .find(|l| l.contains("| todo-1 |"))
        .and_then(|l| l.split('|').next())
        .unwrap()
        .to_string();
    let edit = ["dossier", "edit", "links/plan.md", "insert-after", &anchor];
    refused(&[&edit[..], &["--content", row]].concat(), "", twice);
    let o = run(
        &["dossier", "append", "links/plan.md"],
        "| todo-2 | two | open |\n",
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    assert_eq!(
        std::fs::read_to_string(home.join("agentWorkspaces/links/plan.md")).unwrap(),
        format!("{plan}| todo-2 | two | open |\n")
    );
}

/// One bad plan table line (EV-review-10 S2): the real plan had a row without its leading `|` (todo-155), which ended
/// the table and made the next row a header, so 79 rows lost their status and health blamed four correct EVIDENCE
/// links; and rows with a `|` inside a cell. A write (write, edit, append) that adds such a line is refused with the
/// line and row; append of a row under a prose section (no table) is refused too. A bad line made outside legionctl
/// is a check finding with its line; the rows keep their nodes and statuses, so the link to the row is no finding.
#[test]
fn a_plan_table_row_that_does_not_parse_is_refused_and_check_names_its_line() {
    let (op, home) = operator("rows");
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "s1"),
    ];
    let run = |args: &[&str], stdin: &str| op.run(&home, &env, args, stdin);
    let refused = |args: &[&str], stdin: &str, why: &str| {
        let o = run(args, stdin);
        let text = String::from_utf8_lossy(&o.stderr);
        assert!(
            !o.status.success() && text.contains(why),
            "{args:?}: {text}"
        );
    };
    let ok = |args: &[&str], stdin: &str| {
        let o = run(args, stdin);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    ok(&["dossier", "use", "rows"], "");
    let plan = "# rows\n\n## ToDo\n| id | item | status |\n| --- | --- | --- |\n| todo-1 | one | open |\n\
                | todo-2 | two | open |\n| todo-3 | three | built |\n\n## Validation\nTests: none yet.\n";
    let overwrite = ["dossier", "write", "rows/plan.md", "--overwrite"];
    ok(&overwrite, plan);
    ok(
        &["dossier", "add", "EVIDENCE", "two"],
        "---\nid: EV-2\nclass: EVIDENCE\nsupports: [plan#todo-2]\n---\n# Two\n",
    );
    let file = home.join("agentWorkspaces/rows/plan.md");

    refused(
        &overwrite,
        &plan.replace("| todo-2 |", " todo-2 |"),
        "plan.md:7: a table line that does not start and end with |: todo-2",
    );
    let read = ok(&["dossier", "read", "rows/plan.md"], "");
    let anchor = read
        .lines()
        .find(|l| l.contains("| todo-1 |"))
        .and_then(|l| l.split('|').next())
        .unwrap();
    refused(
        &[
            "dossier",
            "edit",
            "rows/plan.md",
            "replace",
            anchor,
            "--content",
            "| todo-1 | curl|sh | open |",
        ],
        "",
        "plan.md:6: 4 cells, the header has 3 (a | inside a cell?): todo-1",
    );
    refused(
        &["dossier", "append", "rows/plan.md"],
        "| todo-4 | four | open |\n",
        "plan.md:12: a table row outside a table",
    );
    assert_eq!(
        std::fs::read_to_string(&file).unwrap(),
        plan,
        "nothing written"
    );

    // the same bad line written outside legionctl (an agent's own file tool)
    write(&file, &plan.replace("| todo-2 |", " todo-2 |"));
    let check = String::from_utf8_lossy(&run(&["dossier", "check"], "").stdout).to_string();
    assert!(
        check.contains("plan.md:7: a table line that does not start and end with |: todo-2")
            && !check.contains("does not exist"),
        "{check}"
    );
    let graph: Value = serde_json::from_str(&ok(&["dossier", "graph", "--json"], "")).unwrap();
    let status = |id: &str| {
        graph["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|n| n["id"] == id)
            .map(|n| n["status"].clone())
    };
    assert_eq!(
        (status("plan#todo-2"), status("plan#todo-3")),
        (Some(json!("open")), Some(json!("built"))),
        "{graph}"
    );
}

/// A misspelled field is refused, not dropped: an agent's `dossier.edit` with `lines` for `content` (as happened on
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
            "# Plan\n| id | item |\n| --- | --- |\n| todo-1 | keep me |\n",
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
    let (error, failed) = mcp.run(
        "dossier.edit",
        &json!({"path": "plan/plan.md", "mode": "replace", "start": anchor, "lines": ["| todo-1 | changed |"]}),
    );
    assert!(failed, "{error}");
    assert_eq!(
        error["error"]["message"],
        "dossier.edit has no field lines. Its fields are: content, end, mode, path, session, start.",
        "the refusal names the field and the real ones"
    );
    let after = op.run(&repo, &env, &["dossier", "read", "plan/plan.md"], "");
    assert!(
        String::from_utf8_lossy(&after.stdout).contains("| todo-1 | keep me |"),
        "the row stays"
    );
}

/// The lead's usual work through its CLI: dossier read, write and edit, workspace open, checkpoint and merge, a mutant
/// record, a thread and a message in it; each step must pass.
fn the_leads_cli_flow(op: &Operator, repo: &Path, env: &[(&str, &str)]) {
    let ok = |cwd: &Path, args: &[&str], stdin: &str| {
        let o = op.run_bare(cwd, env, args, stdin);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).trim().to_string()
    };
    ok(repo, &["dossier", "use", "port"], "");
    ok(
        repo,
        &["dossier", "write", "port/plan.md", "--overwrite"],
        "# Plan\n| id | item |\n| --- | --- |\n| todo-1 | lexer |\n",
    );
    let read = ok(repo, &["dossier", "read", "port/plan.md"], "");
    let anchor = read
        .lines()
        .find(|l| l.contains("todo-1"))
        .unwrap()
        .split('|')
        .next()
        .unwrap();
    ok(
        repo,
        &[
            "dossier",
            "edit",
            "port/plan.md",
            "replace",
            anchor,
            "--content",
            "| todo-1 | lexer done |",
        ],
        "",
    );
    let opened: Value =
        serde_json::from_str(&ok(repo, &["workspace", "open", "--json"], "")).unwrap();
    let ws = PathBuf::from(opened["path"].as_str().unwrap());
    write(&ws.join("lexer.txt"), "tokens\n");
    ok(&ws, &["workspace", "checkpoint", "the lexer"], "");
    ok(
        &ws,
        &[
            "code",
            "tests",
            "mutant",
            "record",
            "lexer.txt",
            "1",
            "no tokens",
        ],
        "",
    );
    ok(repo, &["thread", "start", "port the parser"], "");
    ok(
        repo,
        &[
            "thread",
            "join",
            "1",
            "observer0",
            "thread-observer",
            "--member-session",
            "obs",
        ],
        "",
    );
    ok(
        repo,
        &["thread", "send", "observer0", "note", "the lexer is in"],
        "",
    );
    ok(repo, &["workspace", "merge"], "");
}

/// DESIGN/22 rule 2 (lead 2026-10-18): a harness agent with the plugin holds the toolsets of the plugin's skills from
/// its session start, so the lead's usual work through its CLI (Claude Code's Bash tool: `CLAUDE_CODE_SESSION_ID`) and
/// a Codex agent's MCP server (it knows no session) ask nothing. This operator has no desktop: a question would refuse
/// its call, so each step that passes asked nothing, and the log has no question.
#[test]
fn a_plugin_agent_works_through_its_cli_and_mcp_without_a_question() {
    let (op, home) = operator("plugcli");
    let repo = repository(&home);
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("CLAUDE_CODE_SESSION_ID", "cc-1"),
    ];
    let flow = [
        "dossier.edit",
        "workspace.checkpoint",
        "code.tests.mutant.record",
        "thread.send",
    ];
    let scope = |args: &[&str]| -> Value {
        let o = op.run_bare(
            &repo,
            &env,
            &[&["runnables", "scope", "--json"], args].concat(),
            "",
        );
        serde_json::from_slice(&o.stdout).unwrap()
    };
    assert_eq!(
        scope(&flow)["missing"],
        json!({"dossier.edit": "dossier-write", "workspace.checkpoint": "workspace-write",
               "code.tests.mutant.record": "code-work", "thread.send": "thread-member"}),
        "before its session start, the flow would ask four times"
    );
    let start = op.run_bare(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &["hook", "session-start"],
        &claude_hook("session-start", &json!({"session_id": "cc-1", "cwd": repo})),
    );
    assert!(
        start.status.success(),
        "{}",
        String::from_utf8_lossy(&start.stderr)
    );
    assert_eq!(scope(&flow)["missing"], json!({}), "{}", scope(&[]));
    the_leads_cli_flow(&op, &repo, &env);

    // Codex starts legionctl's MCP server with a cleared environment: it knows no session
    let mut mcp = Mcp::start(&op, &[("HOME", home.to_str().unwrap())]);
    let (fact, failed) = mcp.run(
        "facts.set",
        &json!({"entity": "app", "attribute": "width", "value": "0.3"}),
    );
    assert!(!failed, "{fact}");

    let events: Value =
        serde_json::from_str(&op.ok(&["events", "--json", "--limit", "1000"])).unwrap();
    let asked = events["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "approval.ask" || e["kind"] == "approval.answer")
        .count();
    assert_eq!(asked, 0, "no question: {events}");
}

/// DESIGN/22 rule 3: each run of an agent's operation has a call id and a row in the call record before it runs. The
/// answer that did not reach the agent comes again for its call id, also after an operator restart, and the change does
/// not run twice; the id of another call, and a call that has no end (the operator stopped during it), are one
/// sentence each; a read with a cache time answers from the record.
#[test]
fn a_retried_call_gets_its_recorded_result_and_never_runs_twice() {
    let home = home("calls");
    let op = operator_in("calls", &home);
    agent_loads_its_skills(&op, &home, "r1");
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "r1"),
    ];
    let set = |value: &str| json!({"entity": "app", "attribute": "width", "value": value});
    let run = |mcp: &mut Mcp, input: &Value, call: Option<&str>| -> (Vec<String>, bool) {
        let mut args = json!({"op": "facts.set", "input": input});
        if let Some(call) = call {
            args["call_id"] = call.into();
        }
        let result = mcp.call(
            "tools/call",
            &json!({"name": "legionctl_run", "arguments": args}),
        );
        let texts = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|c| c["text"].as_str().unwrap().to_string())
            .collect();
        (texts, result["isError"] == true)
    };
    let mut mcp = Mcp::start(&op, &env);
    let (first, failed) = run(&mut mcp, &set("0.3"), None);
    assert!(!failed, "{first:?}");
    let id = first[1]
        .strip_prefix("call ")
        .expect("the answer's last line names its call")
        .to_string();
    drop(mcp);
    drop(op);

    let op = operator_in("calls2", &home);
    let mut mcp = Mcp::start(&op, &env);
    let (again, failed) = run(&mut mcp, &set("0.3"), Some(&id));
    assert!(!failed && again == first, "the recorded answer: {again:?}");
    let history: Value =
        serde_json::from_str(&op.ok(&["facts", "history", "app", "--json"])).unwrap();
    assert_eq!(
        history.as_array().unwrap().len(),
        1,
        "the change ran once: {history}"
    );
    let (other, failed) = run(&mut mcp, &set("0.4"), Some(&id));
    assert!(failed, "{other:?}");
    assert!(
        other[0].contains(&format!(
            "call id {id} is of another call: a retry sends the same operation and input"
        )),
        "{other:?}"
    );

    // the operator stopped during a call: its row has no end
    let cut = Command::new("python3")
        .args([
            "-c",
            "import sqlite3, sys; c = sqlite3.connect(sys.argv[1]); \
             c.execute(\"INSERT INTO calls (call, principal, session, kind, server, name, input_hash, scope, started) \
                        SELECT 'cut', principal, session, kind, server, name, input_hash, scope, started FROM calls \
                        WHERE call = ?\", (sys.argv[2],)); c.commit()",
        ])
        .arg(home.join(".local/state/legionctl/operator.db"))
        .arg(&id)
        .status()
        .unwrap();
    assert!(cut.success());
    let (open, failed) = run(&mut mcp, &set("0.3"), Some("cut"));
    assert!(
        failed
            && open[0].contains("call cut of facts.set started at ")
            && open[0].contains("legionctl does not run it twice: check what it did"),
        "{open:?}"
    );
    let history: Value =
        serde_json::from_str(&op.ok(&["facts", "history", "app", "--json"])).unwrap();
    assert_eq!(history.as_array().unwrap().len(), 1, "{history}");

    // ops.list may be answered from the record for 300 s
    for _ in 0..2 {
        let (_, failed) = mcp.run("ops.list", &json!({}));
        assert!(!failed);
    }
    let calls: Value =
        serde_json::from_str(&op.ok(&["tools", "calls", "--kind", "op", "--json"])).unwrap();
    let rows: Vec<(&str, &str)> = calls["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["session"] == "r1")
        .map(|c| (c["tool"].as_str().unwrap(), c["outcome"].as_str().unwrap()))
        .collect();
    assert_eq!(
        rows,
        [
            ("dossier.active", "ok"),
            ("facts.set", "ok"),
            ("facts.set", "running"),
            ("ops.list", "ok"),
            ("ops.list", "cached")
        ],
        "{calls}"
    );
}

/// `legionctl mcp --only <op>...`: the server of a bound agent (todo-163, the ui-composer) finds, describes and runs
/// only its operations.
fn bound_server_offers_only_its_operations(op: &Operator) {
    let mut bound = Mcp::start_with(op, &[], &["--only", "views.list", "--only", "views.parts"]);
    let (groups, _) = bound.tool("legionctl_find", &json!({}));
    assert_eq!(
        groups,
        "2 operations in 1 groups (group, count). Give words, a group or both.\nviews 2"
    );
    let (found, _) = bound.tool("legionctl_find", &json!({"words": "skills views"}));
    assert!(
        found.starts_with("2 operations match.\nviews.list · read ·") && !found.contains("skills"),
        "{found}"
    );
    let (described, refused) = bound.tool("legionctl_describe", &json!({"op": "skills"}));
    assert!(refused, "{described}");
    let (ran, refused) = bound.run("skills", &json!({}));
    assert!(refused, "{ran}");
    assert_eq!(
        ran["error"]["message"], "No operation skills. Use legionctl_find to find one.",
        "an operation outside the binding does not run, and no name outside it is told"
    );
}

/// The MCP server offers 3 tools (plan#todo-208): an agent finds an operation, reads its input schema and runs it
/// through the operator; a workspace operation still waits for the user; a wrong name gets the closest names.
#[test]
fn an_agent_finds_describes_and_runs_operations_through_three_mcp_tools() {
    let (op, home) = operator("mcp");
    // an agent session that ran no session start (no plugin): the read class only
    let mut mcp = Mcp::start(&op, &[("LEGIONCTL_SESSION", "bare-1")]);

    let tools = mcp.call("tools/list", &json!({}));
    let names: Vec<&str> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["legionctl_find", "legionctl_describe", "legionctl_run"]
    );
    let public = legionctl_api::OPS.iter().filter(|o| !o.internal()).count();
    let (groups, _) = mcp.tool("legionctl_find", &json!({}));
    assert!(
        groups.starts_with(&format!("{public} operations in ")) && groups.contains("\nfan "),
        "{groups}"
    );
    let (found, _) = mcp.tool(
        "legionctl_find",
        &json!({"words": "checkpoint workspace", "limit": 1}),
    );
    let mut lines = found.lines();
    let head = lines.next().unwrap();
    assert!(
        head.ends_with("left out. Give more words or a higher limit."),
        "{found}"
    );
    assert!(
        lines
            .next()
            .is_some_and(|row| row.starts_with("workspace.checkpoint · session · ")
                && row.ends_with(" · not granted: workspace-write, ask"))
            && lines.next().is_none(),
        "the op with both words in its name comes first: {found}"
    );
    let (described, _) = mcp.tool("legionctl_describe", &json!({"op": "hooks.add"}));
    assert!(
        described.starts_with(
            "hooks.add · workspace. It changes your files, settings or other sessions"
        ) && described.contains("Input: {")
            && described.contains("\"command\"")
            && described.contains(
                " Not granted: settings-change, ask: a run asks the user for that toolset."
            )
            && described.ends_with("Output: HookTable"),
        "{described}"
    );

    bound_server_offers_only_its_operations(&op);

    // the MCP server runs for an agent: a workspace operation of hooks asks the user, and nobody can be asked here
    let (denied, failed) = mcp.run(
        "hooks.add",
        &json!({"id": "greet", "command": "echo hello"}),
    );
    assert!(failed, "{denied}");
    assert_eq!(denied["error"]["code"], "denied", "{denied}");
    let (table, _) = mcp.run("hooks.list", &json!({}));
    assert!(
        !table.to_string().contains("greet"),
        "nothing added: {table}"
    );
    let (unknown, failed) = mcp.run("workspace_checkpoint", &json!({}));
    assert!(failed);
    assert_eq!(
        unknown["error"]["message"],
        "No operation workspace_checkpoint. The closest: workspace.checkpoint, workspace.close, workspace.list."
    );

    skills_are_listed_with_their_plugin_folder(&mut mcp, &home);

    // an agent's call works in its own working directory, not the operator's
    let (listed, _) = mcp.run("agents.list", &json!({}));
    let text = listed.to_string();
    let here = std::env::current_dir()
        .unwrap()
        .join(".claude/settings.json");
    assert!(text.contains(&here.display().to_string()), "{text}");
}

/// `skills` over MCP: every plugin skill, its reminder, and its folder once Claude Code's plugin registry names it.
fn skills_are_listed_with_their_plugin_folder(mcp: &mut Mcp, home: &Path) {
    let (skills, failed) = mcp.run("skills", &json!({}));
    assert!(!failed, "{skills}");
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
    // Claude Code's plugin registry (version 2) gives the plugin's folder; a skill shows its path there
    let plugin = home.join("plugins/legionctl/0.4.0");
    write(
        &plugin.join("skills/ste100-style/SKILL.md"),
        "---\nname: ste100-style\n---\n",
    );
    write(
        &home.join(".claude/plugins/installed_plugins.json"),
        &json!({"version": 2, "plugins": {"legionctl@lenovolegionlinux":
            [{"scope": "user", "installPath": plugin}]}})
        .to_string(),
    );
    let (skills, _) = mcp.run("skills", &json!({}));
    let installed: Vec<(&str, &str)> = skills
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| Some((s["name"].as_str()?, s["claude"].as_str()?)))
        .collect();
    assert_eq!(
        installed,
        [(
            "ste100-style",
            plugin.join("skills/ste100-style").to_str().unwrap()
        )]
    );
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// Allowed by the user, the repository layer of `repo` applies anywhere inside the repository, and only there.
fn applies_once_allowed(op: &Operator, home: &Path, repo: &Path) {
    trust::allow(home, &repo.join(".legionctl/rules.jsonc"));
    let (inside, outside) = (
        hook_out(op, &repo.join("crates/a"), "prompt"),
        hook_out(op, home, "prompt"),
    );
    assert!(
        inside.contains("Use the make targets.") && !outside.contains("make targets"),
        "{inside}\n{outside}"
    );
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
    // a repository's reminders and hooks wait for the user's first answer (O-2, A-1)
    let inside = hook_out(&op, &sub, "prompt");
    assert!(!inside.contains("Use the make targets."), "{inside}");
    assert_eq!(
        json(&repo, &["config", "explain", "hooks.bindings.prompt"])["value"],
        json!(["reminders", "load-if-busy"]),
        "a repository's hooks are not used before the user allowed the file"
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
    let held = |key| format!("rules.jsonc: {key} is not used until you allow this file");
    assert!(
        problems.contains(&held("hooks")) && problems.contains(&held("reminders")),
        "{problems}"
    );
    applies_once_allowed(&op, &home, &repo);
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
fn the_dossier_gate_holds_in_a_repository_linked_to_a_dossier_and_nowhere_else() {
    let (op, home) = operator("dossier");
    let out = |o: std::process::Output| String::from_utf8_lossy(&o.stdout).trim().to_string();
    let hook = |event: &str, input: Value| {
        out(op.run(&home, &[], &["hook", event], &claude_hook(event, &input)))
    };
    let as_session = |session: &str, dir: &Path, args: &[&str], stdin: &str| {
        let o = op.run(dir, &[("LEGIONCTL_SESSION", session)], args, stdin);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        out(o)
    };
    let blocks = |text: &str| text.contains("\"decision\"");
    let prompt = |session: &str, dir: &Path, text: &str| {
        hook(
            "prompt",
            json!({"session_id": session, "cwd": dir, "prompt": text}),
        )
    };
    let repo = home.join("repo");
    let folder = home.join("other");
    std::fs::create_dir_all(&repo).unwrap();
    std::fs::create_dir_all(&folder).unwrap();
    git(&home, &repo, &["init", "--quiet"]);

    // a folder outside any repository, and a repository where no dossier was used: Claude Code works as without
    // legionctl (U-1: before, every prompt in every folder was blocked)
    for (n, dir) in [&folder, &repo].into_iter().enumerate() {
        let s = format!("free{n}");
        let start = hook("session-start", json!({"session_id": s, "cwd": dir}));
        assert!(
            start.starts_with("legionctl dossier: none (no dossier was used in this folder"),
            "{start}"
        );
        let said = prompt(&s, dir, "hello");
        assert!(!blocks(&said), "{}: {said}", dir.display());
        assert_eq!(hook("stop", json!({"session_id": s, "cwd": dir})), "");
    }

    // the user links the repository: a session there binds a dossier, and the next session there takes it by itself
    assert!(
        as_session("s1", &repo, &["dossier", "use", "port"], "").contains("active dossier: port")
    );
    hook("session-end", json!({"session_id": "s1"}));
    let next = hook("session-start", json!({"session_id": "s3", "cwd": repo}));
    assert!(
        next.starts_with("legionctl dossier: port (bound by itself"),
        "{next}"
    );
    let said = prompt("s3", &repo, "go on");
    assert!(!blocks(&said), "{said}");

    // in the linked repository the rule holds: a session that cannot take its dossier (s3 holds it) hears why at
    // its start, its prompt is blocked with the way forward, and its reply end is stopped once
    let start = hook("session-start", json!({"session_id": "s4", "cwd": repo}));
    assert!(
        start.starts_with("legionctl: the dossier of this repository cannot be bound:"),
        "{start}"
    );
    let blocked: Value = serde_json::from_str(&prompt("s4", &repo, "fix the bug")).unwrap();
    assert_eq!(blocked["decision"], "block");
    let reason = blocked["reason"].as_str().unwrap();
    assert!(
        reason.contains("works in dossier port")
            && reason.contains("/legionctl:dossier use <name>"),
        "{blocked}"
    );
    let stop: Value =
        serde_json::from_str(&hook("stop", json!({"session_id": "s4", "cwd": repo}))).unwrap();
    assert!(
        stop["reason"]
            .as_str()
            .unwrap()
            .contains("legionctl_run dossier.use"),
        "{stop}"
    );
    assert_eq!(
        hook(
            "stop",
            json!({"session_id": "s4", "cwd": repo, "stop_hook_active": true})
        ),
        ""
    );
    let passing = prompt("s4", &repo, "/legionctl:dossier use other");
    assert!(!blocks(&passing), "{passing}");

    // once s3 ends, the next prompt of s4 takes the dossier by itself: nobody binds it by hand
    hook("session-end", json!({"session_id": "s3"}));
    let taken = prompt("s4", &repo, "fix the bug");
    assert!(
        !blocks(&taken) && taken.contains("legionctl dossier: port (bound by itself"),
        "{taken}"
    );
    assert_eq!(
        as_session("s4", &repo, &["dossier", "check"], ""),
        "port/plan.md has every ledger section"
    );

    // a folder that is not a repository stays free, also where a dossier was once used (the app's runs in $HOME):
    // no gate, and no dossier bound to the next session in silence
    as_session("s5", &folder, &["dossier", "use", "notes"], "");
    hook("session-end", json!({"session_id": "s5"}));
    let start = hook("session-start", json!({"session_id": "s6", "cwd": folder}));
    assert!(start.starts_with("legionctl dossier: none"), "{start}");
    let said = prompt("s6", &folder, "hello");
    assert!(!blocks(&said), "{said}");
}

/// U-6: the user (a terminal with no agent session) chooses a dossier once, and every operation on "my dossier" uses
/// it. Before, `workspace status`, `dossier check` and `workspace open` said "choose it: dossier use" after the user
/// had done so, with the agents' variable names.
#[test]
fn the_dossier_the_user_chose_is_the_one_that_workspace_and_dossier_commands_use() {
    let (op, home) = operator("chosen");
    let repo = repository(&home);
    let run = |args: &[&str]| {
        let o = op.run(&repo, &[], args, "");
        (
            o.status.success(),
            format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
        )
    };

    let (ok, text) = run(&["workspace", "status"]);
    assert!(
        !ok && text.contains("you have no dossier: choose one with legionctl dossier use <name>"),
        "{text}"
    );
    assert!(!text.contains("LEGIONCTL_SESSION"), "{text}");

    assert!(run(&["dossier", "create", "demo"]).0);
    assert!(run(&["dossier", "use", "demo"]).0);
    let (ok, text) = run(&["workspace", "open"]);
    assert!(ok && text.contains("legionctl/demo"), "{text}");
    let (ok, text) = run(&["workspace", "status"]);
    assert!(ok && text.contains("legionctl/demo"), "{text}");
    let (ok, text) = run(&["dossier", "check", "--json"]);
    assert!(ok && text.contains("\"dossier\": \"demo\""), "{text}");

    // a write that only an agent does (it holds the dossier) says so in one line, with no variable names
    let (ok, text) = run(&["dossier", "add", "MEMORY", "x"]);
    assert!(!ok && text.contains("only an agent does this"), "{text}");
    assert!(!text.contains("CLAUDE_CODE_SESSION_ID"), "{text}");
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

    // hook input without its cwd, or with a relative one (an outdated client), is refused, never stored with a
    // made-up place (todo-142 F-91: before, a missing cwd became ".")
    let refused = op.run(
        &home,
        &[],
        &["hook", "session-start"],
        &json!({"session_id": "s5"}).to_string(),
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("the hook input has no cwd"),
        "{refused:?}"
    );
    let refused = op.run(
        &home,
        &[],
        &["hook", "session-start"],
        &json!({"session_id": "s5", "cwd": "."}).to_string(),
    );
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("is not absolute"),
        "{refused:?}"
    );

    // the write guard: no file tool writes into a dossier (the holder writes with the dossier commands, todo-176 S3);
    // every dossier can be read; other paths are not legionctl's business
    let plan = home.join("agentWorkspaces/port/plan.md");
    let pre_tool = |session: &str, tool: &str, path: &Path| {
        hook(
            "pre-tool",
            json!({"session_id": session, "tool_name": tool, "tool_input": {"file_path": path, "content": "x"}}),
        )
    };
    let denied = pre_tool("s2", "Write", &plan);
    assert!(
        denied.contains(r#""permissionDecision":"deny""#)
            && denied.contains("active in session s1")
    );
    let own = pre_tool("s1", "Edit", &plan);
    assert!(
        own.contains(r#""permissionDecision":"deny""#)
            && own.contains(
                "port/plan.md is in this session's dossier: write it with legionctl dossier write"
            ),
        "{own}"
    );
    assert_eq!(pre_tool("s2", "Read", &plan), "");
    assert_eq!(pre_tool("s2", "Write", &home.join("x.rs")), "");

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
/// scoped): after the minute of grace, the next session takes the dossier without --steal. Before that, a state that
/// cannot be read (systemd or Docker does not answer: todo-142 F-8) refuses the bind with the reason and leaves the
/// dossier to the session that may still run.
fn ended_scope_frees_the_dossier(op: &Operator, home: &Path, notes: &Path, id: &str) {
    sql(home, "UPDATE sessions SET scoped = 1");
    sql(home, "UPDATE bindings SET updated = updated - 120");
    let take = || {
        op.run(
            notes,
            &[
                ("HOME", home.to_str().unwrap()),
                ("LEGIONCTL_SESSION", "next"),
            ],
            &["dossier", "use", "notes"],
            "",
        )
    };
    stand_in(
        home,
        "systemctl",
        "#!/bin/sh\necho 'Failed to connect to user scope bus via local transport' >&2\nexit 1\n",
    );
    let held = take();
    let why = String::from_utf8_lossy(&held.stderr);
    assert!(
        !held.status.success()
            && why.contains(&format!("cannot read the state of session {id}"))
            && why.contains("Failed to connect to user scope bus"),
        "{why}"
    );
    sql(home, "UPDATE sessions SET inside = 'container'");
    stand_in(
        home,
        "docker",
        "#!/bin/sh\necho 'Cannot connect to the Docker daemon at unix:///var/run/docker.sock' >&2\nexit 1\n",
    );
    let held = take();
    let why = String::from_utf8_lossy(&held.stderr);
    assert!(
        !held.status.success()
            && why.contains("docker inspect: Cannot connect to the Docker daemon"),
        "{why}"
    );
    // the scope is gone: systemd says so
    sql(home, "UPDATE sessions SET inside = 'host'");
    stand_in(home, "systemctl", "#!/bin/sh\necho inactive\nexit 3\n");
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
    // a change of your settings: the user's call (an agent's would ask for the toolset settings-change)
    let set = op.run(
        &home,
        &[],
        &[
            "agents",
            "set",
            "claude",
            "user",
            "env.TOKEN",
            token,
            "--dry-run",
        ],
        "",
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
    // the agents' own hook calls (their skill loads) are internal rows
    let audit: Vec<(String, Value)> = audit
        .into_iter()
        .filter(|(k, _)| !k.starts_with("internal."))
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
/// made while it runs shows at once (no polling in the client). The latency is from the end of the call that wrote
/// the event (the event is in the log when the call returns) to the line, each line stamped when it arrives, so a busy
/// machine that starts the CLI slowly does not count (todo-133). The stream may connect late: the test makes changes
/// until the first line comes.
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
        std::time::Instant::now()
    };
    use_dossier("before");
    let mut trace = Kill::spawn(
        Command::new(env!("CARGO_BIN_EXE_legionctl"))
            .arg("trace")
            .env_clear()
            .env("XDG_RUNTIME_DIR", &op.runtime)
            .env("PATH", std::env::var_os("PATH").unwrap())
            .stdout(Stdio::piped()),
    );
    let (lines, seen) = std::sync::mpsc::channel();
    let out = BufReader::new(trace.stdout.take().unwrap());
    std::thread::spawn(move || {
        for line in out.lines().map_while(Result::ok) {
            if lines.send((std::time::Instant::now(), line)).is_err() {
                break;
            }
        }
    });
    let mut written = Vec::new();
    let (arrived, line) = loop {
        assert!(written.len() < 20, "trace showed nothing");
        written.push(use_dossier(&format!("during{}", written.len())));
        if let Ok(seen) = seen.recv_timeout(std::time::Duration::from_millis(500)) {
            break seen;
        }
    };
    drop(trace);
    assert!(!line.contains("before"), "the backlog stays out: {line}");
    let n = (0..written.len())
        .rev()
        // the change in words: dossier.use's sentence with its name
        .find(|n| line.contains(&format!("(name during{n}")))
        .unwrap_or_else(|| panic!("{line}"));
    let latency = arrived.saturating_duration_since(written[n]);
    assert!(
        latency < std::time::Duration::from_millis(1500),
        "{latency:?} from the event to its line"
    );
}

/// Each operation's doc comment is what you read: its help, and its first sentence is the confirm, the approval
/// notice and the tooltip. So no doc holds a design or plan ref (they go into `//` comments), and no first sentence
/// holds an operation's dotted name or a hash.
#[test]
fn every_operation_speaks_to_you_without_builder_text() {
    let words = |text: &str| -> Vec<String> {
        text.split(|c: char| c.is_whitespace() || "(),;:`'\"".contains(c))
            .map(|w| w.trim_end_matches('.').to_string())
            .filter(|w| !w.is_empty())
            .collect()
    };
    let digit_after = |w: &str, head: &str| {
        w.strip_prefix(head)
            .is_some_and(|r| r.starts_with(|c: char| c.is_ascii_digit()))
    };
    let names: Vec<&str> = legionctl_api::OPS
        .iter()
        .map(|o| o.name)
        .filter(|n| n.contains('.'))
        .collect();
    let mut wrong = Vec::new();
    for op in legionctl_api::OPS.iter().filter(|o| !o.internal()) {
        for w in words(op.text()) {
            if w.contains("DESIGN")
                || w.starts_with("EV-")
                || w.contains("plan#")
                || digit_after(&w, "todo-")
                || ["U-", "G", "R"]
                    .iter()
                    .any(|h| digit_after(&w, h) && w.len() <= 5)
            {
                wrong.push(format!("{}: ref {w}", op.name));
            }
        }
        for w in words(op.summary()) {
            if names.contains(&w.as_str()) || w.starts_with("internal.") {
                wrong.push(format!(
                    "{}: operation name {w} in its first sentence",
                    op.name
                ));
            }
            if w.len() >= 12
                && w.chars().all(|c| c.is_ascii_hexdigit())
                && w.chars().any(|c| c.is_ascii_digit())
            {
                wrong.push(format!("{}: hash {w} in its first sentence", op.name));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "builder text in the help:\n{}",
        wrong.join("\n")
    );
}

/// One context (DESIGN-20 rule 7, user 2026-10-17: "if I set the repo once in the app, all things that use the repo
/// must know"): a call works in the folder of its caller (the CLI's folder, the app's project, the hook's `cwd`, a
/// session's own workspace), so no operation takes its own repository, project or working folder. A path field is a
/// file or a folder that is not the project, and each one is listed here with its reason; a new one fails until it
/// is listed (or until it uses the call's folder).
#[test]
fn no_operation_takes_its_own_project_folder() {
    let allowed: &[(&str, &str, &str)] = &[
        ("profile", "*", "the machine profile file, not a project"),
        ("dir", "record", "the new folder of the recording"),
        ("dir", "replay", "the folder of a recording"),
        (
            "config_fragment",
            "kernel.build",
            "a file of kernel options",
        ),
        (
            "index",
            "code.tests.review",
            "git's index file of the pre-commit hook",
        ),
    ];
    let place = [
        "repo",
        "project",
        "folder",
        "cwd",
        "workdir",
        "worktree",
        "root",
        "workspace",
    ];
    let mut wrong = Vec::new();
    for op in legionctl_api::OPS {
        let schema = (op.input)();
        for (field, prop) in schema["properties"].as_object().into_iter().flatten() {
            let path = prop["format"] == "path"
                || prop["anyOf"]
                    .as_array()
                    .is_some_and(|a| a.iter().any(|s| s["format"] == "path"));
            let named = place.contains(&field.as_str()) && prop["type"] != "boolean";
            let listed = allowed
                .iter()
                .any(|(f, o, _)| f == field && (*o == "*" || *o == op.name));
            if (path || named) && !listed {
                wrong.push(format!("{} {field}", op.name));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "fields that name a folder the call's own folder gives (use Ctx::folder, or list it with its reason):\n{}",
        wrong.join("\n")
    );
}

/// The folder of a call's agent (DESIGN-20 rule 7, lead decision 2026-10-19: the MCP server follows the agent's cwd
/// through the hooks; no folder field on `legionctl_run`). A harness starts its MCP server in the folder its agent
/// started in; the agent then works in another one. Claude Code's MCP server works in the folder its session's
/// newest hook names, and in its own again after the session ends. Codex's server has no session: it works in the
/// folder of the one Codex session whose hooks name a folder inside its own; with two such sessions, in its own. The
/// call record says which folder each call worked in.
#[test]
fn an_agents_mcp_server_works_in_the_folder_its_agent_works_in_now() {
    let (op, home) = operator("mcp-folder");
    let project = home.join("project");
    let (moved, other) = (project.join("worktree"), project.join("other"));
    for dir in [&moved, &other] {
        std::fs::create_dir_all(dir).unwrap();
    }
    let h = home.to_str().unwrap();
    let hook = |event: &str, payload: String| {
        let out = op.run(&home, &[("HOME", h)], &["hook", event], &payload);
        assert!(out.status.success(), "{out:?}");
    };
    let codex_prompt = |session: &str, cwd: &Path| {
        let mut payload: Value = serde_json::from_str(
            &std::fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("../../tests/fixtures/codex-hooks/prompt.json"),
            )
            .unwrap(),
        )
        .unwrap();
        payload["session_id"] = json!(session);
        payload["cwd"] = json!(cwd);
        hook("prompt", payload.to_string());
    };
    // the folder of agents.list's answer: its project settings file
    let folder_of = |mcp: &mut Mcp| -> PathBuf {
        let (listed, failed) = mcp.run("agents.list", &json!({}));
        assert!(!failed, "{listed}");
        let file = listed
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["scope"] == "project")
            .filter_map(|f| f["path"].as_str())
            .find(|p| p.ends_with("/.claude/settings.json"))
            .unwrap_or_else(|| panic!("{listed}"))
            .to_string();
        Path::new(&file)
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    };

    let mut claude = Mcp::start_in(
        &op,
        &[("HOME", h), ("CLAUDE_CODE_SESSION_ID", "c1")],
        &project,
    );
    assert_eq!(
        folder_of(&mut claude),
        project,
        "before a hook: its own folder"
    );
    hook(
        "prompt",
        claude_hook(
            "prompt",
            &json!({"session_id": "c1", "cwd": moved, "prompt": "go on"}),
        ),
    );
    assert_eq!(
        folder_of(&mut claude),
        moved,
        "the folder of its agent's newest hook"
    );
    let calls: Value =
        serde_json::from_str(&op.ok(&["tools", "calls", "--kind", "op", "--json"])).unwrap();
    let folders: Vec<&str> = calls["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["tool"] == "agents.list" && c["session"] == "c1")
        .map(|c| c["folder"].as_str().unwrap())
        .collect();
    assert_eq!(
        folders,
        [project.to_str().unwrap(), moved.to_str().unwrap()]
    );
    hook(
        "session-end",
        claude_hook("session-end", &json!({"session_id": "c1", "cwd": moved})),
    );
    assert_eq!(folder_of(&mut claude), project, "after its session ended");

    // Codex starts its MCP server with a cleared environment: no session
    let mut codex = Mcp::start_in(&op, &[("HOME", h)], &project);
    codex_prompt("x1", &moved);
    assert_eq!(
        folder_of(&mut codex),
        moved,
        "the one Codex session in its folder"
    );
    codex_prompt("x2", &other);
    assert_eq!(
        folder_of(&mut codex),
        project,
        "two Codex sessions: its own folder"
    );
}

/// A user's thread (a chat) keeps its scratch folder from its start (plan#todo-211 item 4): a call from a project
/// whose config moves `workspace.root` lists and uses the same folder as a call from the home: the one its start gave.
#[test]
fn a_users_thread_keeps_its_scratch_folder_from_its_start_whoever_asks() {
    let (op, home) = operator("scratch");
    let h = home.to_str().unwrap();
    let user = |cwd: &Path, args: &[&str]| -> String {
        let out = op.run(cwd, &[("HOME", h)], args, "");
        assert!(out.status.success(), "{args:?}: {out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    user(&home, &["dossier", "create", "chat"]);
    let project = home.join("project");
    std::fs::create_dir_all(project.join(".git")).unwrap();
    let layer = project.join(".legionctl/root.jsonc");
    write(
        &layer,
        &json!({"workspace": {"root": project.join("elsewhere")}}).to_string(),
    );
    trust::allow(&home, &layer);
    user(&home, &["thread", "start", "notes", "--dossier", "chat"]);
    let folder = |cwd: &Path| -> Value {
        let listed: Value =
            serde_json::from_str(&user(cwd, &["thread", "list", "--json"])).unwrap();
        listed[0]["folder"].clone()
    };
    let from_home = folder(&home);
    assert!(
        from_home.as_str().is_some_and(
            |f| f.ends_with("/threads/thread-1") && !f.starts_with(project.to_str().unwrap())
        ),
        "{from_home}"
    );
    assert_eq!(
        folder(&project),
        from_home,
        "the same folder from the project"
    );
    // a call of the thread from the project uses that folder too (thread agent makes it)
    user(&project, &["thread", "agent", "1", "claude"]);
    assert!(Path::new(from_home.as_str().unwrap()).is_dir());
    assert!(!project.join("elsewhere").exists());
}

/// A sandboxed session works in its own workspace (DESIGN-20 rule 7): a call from a folder outside it works in the
/// workspace, and one from a folder inside it works there.
#[test]
fn a_sandboxed_sessions_call_from_outside_its_workspace_works_in_its_workspace() {
    let (op, home) = operator("own-folder");
    let workspace = home.join("worktrees/repo/box");
    let inner = workspace.join("src");
    std::fs::create_dir_all(&inner).unwrap();
    // the operator makes its database at its first call
    op.ok(&["events"]);
    add_session_token(&home, "token-box", "s-box");
    let set = Command::new("python3")
        .args([
            "-c",
            "import sqlite3, sys; c = sqlite3.connect(sys.argv[1]); \
             c.execute('UPDATE sessions SET workdir = ? WHERE id = ?', (sys.argv[2], sys.argv[3])); c.commit()",
        ])
        .arg(home.join(".local/state/legionctl/operator.db"))
        .args([workspace.to_str().unwrap(), "s-box"])
        .status()
        .unwrap();
    assert!(set.success());
    let h = home.to_str().unwrap();
    let settings = |cwd: &Path| -> String {
        let out = op.run(
            cwd,
            &[("HOME", h), ("LEGIONCTL_TOKEN", "token-box")],
            &["agents", "list", "--json"],
            "",
        );
        assert!(out.status.success(), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    let project_file = |dir: &Path| format!("\"{}\"", dir.join(".claude/settings.json").display());
    let outside = settings(&home);
    assert!(outside.contains(&project_file(&workspace)), "{outside}");
    let within = settings(&inner);
    assert!(within.contains(&project_file(&inner)), "{within}");
}

/// The wire format is reviewed: `docs/api/schema.json` holds every operation's class, visibility, input and output
/// schema, and the code must match it, so a change to a type on the wire shows up as a diff of that file
/// (`LEGIONCTL_WRITE_SCHEMA=1` writes it after a deliberate change). The schemas of the planning files
/// (`planning/*.schema.json`), of team files (`teams/team.schema.json`), of view files (`docs/api/view.schema.json`)
/// and of runnable manifests and capability sets (`docs/api/runnable.schema.json`) are reviewed the same way.
#[test]
fn the_wire_format_matches_the_committed_schema() {
    for (rel, schema) in legionctl_core::planning::schemas()
        .unwrap()
        .into_iter()
        .chain([
            legionctl_core::archetypes::team_schema().unwrap(),
            legionctl_core::views::schema().unwrap(),
            legionctl_core::runnables::schema().unwrap(),
        ])
    {
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
    let home = home("pages");
    let op = operator_in("pages", &home);
    // the agent's first call loads its skills (internal rows): the pages start after them
    op.run(
        &home,
        &[("LEGIONCTL_SESSION", "s1")],
        &["dossier", "active"],
        "",
    );
    let before: Value =
        serde_json::from_str(&op.ok(&["events", "--json", "--limit", "1000"])).unwrap();
    let start = before["items"].as_array().unwrap().last().unwrap()["id"].to_string();
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
    let first = page(&["--limit", "2", "--cursor", &start]);
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
             c.execute('UPDATE events SET ts = ts - 100 * 86400 WHERE id IN (SELECT id FROM events WHERE kind = \\'dossier.use\\' ORDER BY id LIMIT 2)'); c.commit()",
        ])
        .arg(home.join(".local/state/legionctl/operator.db"))
        .status()
        .unwrap();
    assert!(aged.success());
    drop(op);
    let again = Operator::start(
        "pages2",
        &[
            ("HOME", home.as_os_str().to_owned()),
            ("LEGIONCTL_SYSROOT", home.join("root").into()),
            ("PATH", path_with_build()),
        ],
    );
    let kept: Value = serde_json::from_str(&again.ok(&["events", "--json"])).unwrap();
    let names: Vec<&str> = kept["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["payload"]["input"]["name"].as_str())
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

/// Run one statement on the operator's database of the test's home (to age a binding; nothing else changes it).
fn sql(home: &Path, statement: &str) {
    let out = Command::new("python3")
        .args([
            "-c",
            "import sqlite3, sys; c = sqlite3.connect(sys.argv[1]); c.execute(sys.argv[2]); c.commit()",
        ])
        .arg(home.join(".local/state/legionctl/operator.db"))
        .arg(statement)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// A repository with one commit on main that ignores the dossier home's link (as legionctl's own), and workspaces
/// configured under `home/worktrees`.
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
    write(&repo.join(".gitignore"), "/agentWorkspaces\n");
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

    // open: a worktree on legionctl/port with the dossier home linked in (the repository already ignores the link)
    let ws = home.join("worktrees/repo/port");
    assert!(ok(&repo, &["workspace", "open"]).contains(&ws.display().to_string()));
    assert!(
        ws.join("agentWorkspaces/port/plan.md").is_file(),
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
        (Some(1), Some(0)),
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
        "3",
        "linear: start, main moved, feature"
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
        b.join("agentWorkspaces/port/plan.md").is_file(),
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
        "3",
        "linear: start, work a, work b"
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

/// Close acts on the workspace that holds the call's folder, never on the caller's own (todo-172, EV-review-10 S1): two
/// dossiers in one repository; s1 runs close in s2's worktree, with --force, while s1's own workspace has an
/// uncommitted file. Before, s1's workspace was closed and the file deleted. Now the close is refused with the
/// workspace and dossier it would touch, and both stay; a dry run names s1's workspace and the changes it would drop.
#[test]
fn close_of_another_dossiers_worktree_is_refused_and_keeps_the_callers_work() {
    let (op, home) = operator("close-other");
    let repo = repository(&home);
    let run = |s: &str, cwd: &Path, args: &[&str]| as_session(&op, &home, (s, cwd), args);
    session_ok(&op, &home, ("s1", &repo), &["dossier", "use", "demo"]);
    session_ok(&op, &home, ("s2", &repo), &["dossier", "use", "other"]);
    session_ok(&op, &home, ("s1", &repo), &["workspace", "open"]);
    session_ok(&op, &home, ("s2", &repo), &["workspace", "open"]);
    let (demo, other) = (
        home.join("worktrees/repo/demo"),
        home.join("worktrees/repo/other"),
    );
    write(&demo.join("p.txt"), "not committed");

    // s1 in the other dossier's workspace: the call's folder is that workspace, not s1's own
    let (closed, why) = run("s1", &other, &["workspace", "close", "--force"]);
    assert!(
        !closed
            && why.contains(&format!(
                "is in the workspace {} of dossier other, not of dossier demo",
                other.display()
            )),
        "{why}"
    );
    assert!(demo.join("p.txt").is_file() && other.join(".git").exists());

    let (_, plan) = run("s1", &demo, &["workspace", "close", "--force", "--dry-run"]);
    assert!(
        plan.contains(&format!(
            "would close {} (dossier demo) and discard its 1 uncommitted changes",
            demo.display()
        )),
        "{plan}"
    );
    let (refused, why) = run("s1", &demo, &["workspace", "close"]);
    assert!(
        !refused
            && why.contains(&format!(
                "the workspace {} (dossier demo) has 1 uncommitted",
                demo.display()
            )),
        "{why}"
    );
    assert!(demo.join("p.txt").is_file());
    let list = session_ok(&op, &home, ("s1", &repo), &["workspace", "list"]);
    assert!(
        list.contains(&demo.display().to_string()) && list.contains(&other.display().to_string()),
        "{list}"
    );
}

/// The MCP proxy between a session's agent and a server (here legionctl's own MCP server): the agent sees only the
/// allowed tools, a call to another tool is refused, and the operator logs both calls with the session id.
/// A program in the operator's `PATH` (the stand-in folder of `path_with_keyring`), written after the operator started.
fn stand_in(home: &Path, name: &str, script: &str) {
    let path = home.join("keyring-bin").join(name);
    write(&path, script);
    std::fs::set_permissions(&path, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
}

/// A `mutants.out` as cargo-mutants 27.1 writes it (shape recorded from a real run on a fixture crate; the manual
/// check is in the dossier, EVIDENCE/validation/37-manual-tool-contract-checks): `outcomes.json` with the baseline
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
const FORMS: &str = include_str!("../../../tests/fixtures/test-forms.txt");

const GATE_TESTS: &str =
    "use fx::double;\n\n#[test]\nfn doubles_three() {\n    assert_eq!(double(3), 6);\n}\n";

/// The test gate of `workspace checkpoint` (design 23, A7): a new test without an assertion and a test that kills no
/// mutant are refused with their reasons and nothing is committed; an agent's recorded mutation is evidence;
/// `workspace checkpoint double` with `flags`.
fn double_checkpoint<'a>(flags: &[&'a str]) -> Vec<&'a str> {
    [&["workspace", "checkpoint", "double"][..], flags].concat()
}

/// `--accept-test` needs `--reason`, repeats (todo-129: one reason per test, or one for all), accepts all or none, and
/// each reason is kept with its test and is an event.
#[test]
fn the_checkpoint_gate_refuses_tests_without_evidence_and_takes_recorded_kills_and_accepts() {
    let agent = GateAgent::start("testgate");
    write(
        &agent.ws.join("tests/t.rs"),
        &format!(
            "{GATE_TESTS}\n#[test]\nfn only_calls() {{\n    let _ = double(1);\n}}\n\
             #[test]\nfn also_calls() {{\n    let _ = double(2);\n}}\n"
        ),
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

    // --accept-test repeats; each test gets its reason, kept with the test and as an event
    let (reason, other) = (
        "smoke test of the public path; its value is that it links",
        "the second call path; its value is the other argument",
    );
    let accepts = |test: &str| -> Value {
        let shown: Value =
            serde_json::from_str(&agent.ok(&["code", "tests", "show", test, "--json"])).unwrap();
        shown["accepts"].clone()
    };
    let both = ["--accept-test", "only_calls", "--accept-test", "also_calls"];
    let why = agent.refused(&double_checkpoint(&both));
    assert!(why.contains("--reason"), "an accept needs a reason: {why}");
    let bad = [
        "--accept-test",
        "only_calls",
        "--accept-test",
        "no_such_test",
    ];
    let why = agent.refused(&double_checkpoint(
        &[&bad[..], &["--reason", reason]].concat(),
    ));
    assert!(why.contains("no_such_test"), "{why}");
    let none = agent.refused(&["code", "tests", "show", "only_calls"]);
    assert!(none.contains("nothing is recorded"), "all or none: {none}");
    agent.ok(&double_checkpoint(
        &[&both[..], &["--reason", reason, "--reason", other]].concat(),
    ));
    assert_eq!(agent.last_commit(), "double");
    let events: Value = serde_json::from_str(&agent.ok(&["events", "--json"])).unwrap();
    let mut accepted: Vec<(String, String)> = events["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "code.tests.accept")
        .map(|e| {
            (
                e["payload"]["test"].as_str().unwrap().to_string(),
                e["payload"]["reason"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    accepted.sort();
    assert_eq!(
        accepted,
        [
            ("also_calls".into(), other.into()),
            ("only_calls".into(), reason.into())
        ],
        "each accept is an event with its own reason"
    );
    assert_eq!(accepts("only_calls")[0]["reason"], reason);
    assert_eq!(accepts("also_calls")[0]["reason"], other);
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

/// The repository's test check (`.legionctl/checks.jsonc`, design 23 E2) for `code tests run`.
const TEST_CHECK: &str = r#"{ "checks": { "e2e": { "kind": "test", "runner": { "format": "libtest" },
    "run": "cargo test --workspace --no-fail-fast --message-format=json" } } }"#;

/// `code tests run` runs the repository's `test` check as a job and records each test with the source of its test
/// binary (relative to the repository: two crates' `tests/e2e.rs` are two files), its result and the ref; the filter
/// reaches libtest after the check's command; the run is a check run too; `code tests list` and `show` give them back.
#[test]
fn a_test_run_records_each_test_with_its_file_result_and_ref() {
    let (op, home) = operator("testruns");
    let repo = repository(&home);
    cargo_of_two_crates(&home, &repo);
    write(&repo.join(".legionctl/checks.jsonc"), TEST_CHECK);
    trust::allow(&home, &repo.join(".legionctl/checks.jsonc"));
    // the runs go into the dossier attached to the repository
    let bound = op.run(
        &repo,
        &[
            ("HOME", home.to_str().unwrap()),
            ("LEGIONCTL_SESSION", "s1"),
        ],
        &["dossier", "use", "tests"],
        "",
    );
    assert!(bound.status.success(), "{bound:?}");
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
    assert!(
        String::from_utf8_lossy(&out.stdout)
            .contains("e2e: 2 failed, 1 ignored, 1 ok (exit 101, run ")
    );
    assert_eq!(
        std::fs::read_to_string(home.join("cargo-argv"))
            .unwrap()
            .trim(),
        "test --workspace --no-fail-fast --message-format=json -- flow"
    );
    let checks = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &["code", "checks", "list"],
        "",
    );
    assert!(
        String::from_utf8_lossy(&checks.stdout).contains("exit 101 at tree"),
        "the test run is a run of the check: {checks:?}"
    );
    recorded_tests_and_the_job(&op, &home, &repo);
}

/// `code tests list` and `show` give back each recorded test, and the job shows its progress.
fn recorded_tests_and_the_job(op: &Operator, home: &Path, repo: &Path) {
    let head = git(home, repo, &["rev-parse", "HEAD"]);
    let in_repo = |args: &[&str]| -> Value {
        let out = op.cli_in(repo, args);
        assert!(out.status.success(), "{args:?}: {out:?}");
        serde_json::from_slice(&out.stdout).unwrap()
    };
    let list = in_repo(&["code", "tests", "list", "--json"]);
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
    let shown = in_repo(&["code", "tests", "show", "c_needs_root", "--json"]);
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
    // without a test check there is nothing to run, and the agent is told how to get one
    let none = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &["code", "tests", "run"],
        "",
    );
    assert!(
        String::from_utf8_lossy(&none.stderr).contains(
            "this repository has no checks of kind test: run `legionctl code checks detect`"
        ),
        "{none:?}"
    );
    write(&repo.join(".legionctl/checks.jsonc"), TEST_CHECK);
    trust::allow(&home, &repo.join(".legionctl/checks.jsonc"));
    // the runs go into the dossier attached to the repository
    let bound = op.run(
        &repo,
        &[
            ("HOME", home.to_str().unwrap()),
            ("LEGIONCTL_SESSION", "s1"),
        ],
        &["dossier", "use", "tests"],
        "",
    );
    assert!(bound.status.success(), "{bound:?}");
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

#[test]
fn the_mcp_proxy_offers_only_allowed_tools_and_logs_every_call() {
    use rmcp::ServiceExt;
    use rmcp::model::CallToolRequestParams;

    let (op, home) = operator("proxy");
    agent_loads_its_skills(&op, &home, "s-proxy");
    let exe = env!("CARGO_BIN_EXE_legionctl");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (tools, load, doctor) = runtime.block_on(async {
        let mut proxy = tokio::process::Command::new(exe);
        own(proxy.as_std_mut());
        proxy
            .args([
                "mcp-proxy",
                "--server",
                "self",
                "--allow",
                "legionctl_find",
                "--allow",
                "legionctl_describe",
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
            .call_tool(CallToolRequestParams::new("legionctl_find"))
            .await
            .unwrap();
        let doctor = agent.peer().call_tool(hooks_list()).await;
        (tools, load, doctor)
    });
    assert_eq!(tools, ["legionctl_describe", "legionctl_find"]);
    assert_ne!(load.is_error, Some(true), "{load:?}");
    assert!(
        doctor.as_ref().is_err_and(|e| e
            .to_string()
            .contains("legionctl_run is not allowed for self")),
        "{doctor:?}"
    );

    let calls = op.run(
        &home,
        &[("HOME", home.to_str().unwrap())],
        &["tools", "calls", "--kind", "tool", "--json"],
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
            ("s-proxy".into(), "legionctl_find".into(), "ok".into()),
            ("s-proxy".into(), "legionctl_run".into(), "refused".into())
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
    agent_loads_its_skills(&op, &home, "s-remote");
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-remote-mcp.py");
    // the stand-in picks its own port and says which: listening by then, and no race for the port
    let mut server = Kill::spawn(Command::new("python3").arg(fixture).stdout(Stdio::piped()));
    let mut port = String::new();
    BufReader::new(server.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let port: u16 = port.trim().parse().expect("the stand-in's port");
    let url = format!("http://127.0.0.1:{port}/mcp");
    let (tools, answer) = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut proxy = tokio::process::Command::new(env!("CARGO_BIN_EXE_legionctl"));
        own(proxy.as_std_mut());
        proxy
            .args([
                "mcp-proxy",
                "--server",
                "docs",
                "--allow",
                "docs",
                "--url",
                &url,
            ])
            // the header's value comes in the environment, never in the command line
            .env("LEGIONCTL_MCP_HEADERS", r#"{"x-api-key": "abc"}"#)
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
    drop(server);
    assert_eq!(tools, ["docs"]);
    let text = serde_json::to_string(&answer.content).unwrap();
    assert!(text.contains("key=abc"), "{text}");
    let calls = op.run(
        &home,
        &[("HOME", home.to_str().unwrap())],
        &["tools", "calls", "--kind", "tool", "--json"],
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

    let (op, home) = operator("cache");
    agent_loads_its_skills(&op, &home, "s-cache");
    let repo = repository(&home);
    let exe = env!("CARGO_BIN_EXE_legionctl");
    let results = tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut proxy = tokio::process::Command::new(exe);
        own(proxy.as_std_mut());
        proxy
            .args([
                "mcp-proxy",
                "--server",
                "self",
                "--cache",
                "legionctl_run=60:workspace",
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
            results.push(agent.peer().call_tool(hooks_list()).await.unwrap());
        }
        results
    });
    assert_ne!(results[0].is_error, Some(true), "{:?}", results[0]);
    assert_eq!(results[0], results[1], "the cached result is the same");
    let calls = op.run(
        &home,
        &[("HOME", home.to_str().unwrap())],
        &["tools", "calls", "--kind", "tool", "--json"],
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
    // one hooks.list call through the proxy, as the session of `token` (none: the user); its outcome
    let call = |token: Option<&str>| -> String {
        use rmcp::ServiceExt;
        let exe = env!("CARGO_BIN_EXE_legionctl");
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            let mut proxy = tokio::process::Command::new(exe);
            own(proxy.as_std_mut());
            proxy
                .args([
                    "mcp-proxy",
                    "--server",
                    "self",
                    "--cache",
                    "legionctl_run=60",
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
            agent.peer().call_tool(hooks_list()).await.unwrap();
        });
        let calls: Value =
            serde_json::from_str(&op.ok(&["tools", "calls", "--kind", "tool", "--json"])).unwrap();
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

/// The agent of `session` makes its first call, so it loads the skills of its work (their toolsets, among them the
/// brokered MCP tools, are its scope).
fn agent_loads_its_skills(op: &Operator, home: &Path, session: &str) {
    let first = op.run(
        home,
        &[
            ("HOME", home.to_str().unwrap()),
            ("LEGIONCTL_SESSION", session),
        ],
        &["dossier", "active"],
        "",
    );
    assert!(
        first.status.success(),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
}

/// A git repository with tag v1: a server that notes the network it sees, then serves legionctl's own MCP tools;
/// configured as package `probe` and as MCP server `probe` allowing only `legionctl_find`.
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
            "mcp": {"servers": {"probe": {"package": "probe", "allow": ["legionctl_find"]}}}
        })
        .to_string(),
    );
}

/// `legionctl_run` of `hooks.list`, as an MCP tool call.
fn hooks_list() -> rmcp::model::CallToolRequestParams {
    let Value::Object(arguments) = json!({"op": "hooks.list"}) else {
        unreachable!()
    };
    rmcp::model::CallToolRequestParams::new("legionctl_run").with_arguments(arguments)
}

/// Start `legionctl <args>` as an agent would start an MCP server, and list its tools.
fn mcp_tools(op: &Operator, home: &Path, args: &[String]) -> Vec<String> {
    use rmcp::ServiceExt;
    let exe = Path::new(env!("CARGO_BIN_EXE_legionctl"));
    let path = path_with_build();
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        let mut proxy = tokio::process::Command::new(exe);
        own(proxy.as_std_mut());
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

/// `session start claude` in `repo`: the session id and its `mcp.json`.
fn session_mcp(op: &Operator, home: &Path, repo: &Path) -> (String, Value) {
    let started = op.run(
        repo,
        &[("HOME", home.to_str().unwrap())],
        &["session", "start", "claude", "--dossier", "hdr", "--json"],
        "",
    );
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let id = serde_json::from_slice::<Value>(&started.stdout).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let config = serde_json::from_slice(
        &std::fs::read(op.runtime.join(format!("legionctl/sessions/{id}/mcp.json"))).unwrap(),
    )
    .unwrap();
    (id, config)
}

/// The call record has the two calls of the remote server of session `id`, and no third for the retry.
fn two_calls_and_no_third_for_the_retry(op: &Operator, id: &str) {
    let calls: Value =
        serde_json::from_str(&op.ok(&["tools", "calls", "--kind", "tool", "--json"])).unwrap();
    let logged: Vec<(&str, &str, &str, &str)> = calls["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| {
            (
                c["session"].as_str().unwrap(),
                c["server"].as_str().unwrap(),
                c["tool"].as_str().unwrap(),
                c["outcome"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        logged,
        [(id, "remote", "docs", "ok"), (id, "remote", "docs", "ok")],
        "two calls of the server, and no third for the retry"
    );
}

/// A brokered call's answer ends with its call id; a retry with it gets the recorded answer, and the server is not
/// called again.
fn a_brokered_retry_gets_the_recorded_answer(mcp: &mut Mcp) {
    let mut docs = |call: Option<&str>| {
        let mut args = json!({"op": "remote/docs", "input": {"q": "fan"}});
        if let Some(call) = call {
            args["call_id"] = call.into();
        }
        mcp.call(
            "tools/call",
            &json!({"name": "legionctl_run", "arguments": args}),
        )["content"]
            .clone()
    };
    let first = docs(None);
    let call = first[1]["text"]
        .as_str()
        .unwrap()
        .strip_prefix("call ")
        .unwrap()
        .to_string();
    assert_eq!(docs(Some(&call)), first, "the recorded answer");
}

/// DESIGN/22 rule 1 with B-1 (EV-review-14): a session's only MCP server is legionctl's, which brokers each
/// configured server: the agent finds, describes and runs a remote server's allowed tool through the three tools, the
/// proxy logs the call, and the keyring header reaches the server but never a command line (the exec sensor records
/// every session's argv, and `events` shows them).
#[test]
fn a_session_runs_a_brokered_servers_tool_through_legionctls_three_tools() {
    let (op, home) = operator("mcphdr");
    let repo = repository(&home);
    let fixture =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-remote-mcp.py");
    let mut server = Kill::spawn(Command::new("python3").arg(fixture).stdout(Stdio::piped()));
    let mut port = String::new();
    BufReader::new(server.stdout.take().unwrap())
        .read_line(&mut port)
        .unwrap();
    let url = format!("http://127.0.0.1:{}/mcp", port.trim());
    write(&home.join("keyring/remote-key"), "HEADER_SECRET_VALUE");
    write(
        &home.join(".config/legionctl/mcp.jsonc"),
        &json!({"mcp": {"servers": {
            "remote": {"url": url, "allow": ["docs"], "keyring_headers": {"x-api-key": "remote-key"}},
            // a command server that notes the environment it gets
            "self": {"command": ["sh", "-c", format!("env > \"$HOME/seen.env\"; exec {} mcp", env!("CARGO_BIN_EXE_legionctl"))],
                     "allow": ["legionctl_find"]},
            // a server that never answers
            "mute": {"command": ["sleep", "60"]}
        }}})
        .to_string(),
    );
    let (id, config) = session_mcp(&op, &home, &repo);
    let servers: Vec<&String> = config["mcpServers"].as_object().unwrap().keys().collect();
    assert_eq!(servers, ["legionctl"], "{config}");
    let legionctl = &config["mcpServers"]["legionctl"];
    assert!(
        !legionctl["args"]
            .to_string()
            .contains("HEADER_SECRET_VALUE"),
        "a secret in a command line: {legionctl}"
    );

    // the agent's MCP server, as the harness starts it from mcp.json; the session does not run here (no user
    // manager), so its token is not valid and the user token stands in
    let vars: Vec<(String, String)> = legionctl["env"]
        .as_object()
        .unwrap()
        .iter()
        .filter(|(k, _)| *k != "LEGIONCTL_TOKEN")
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .chain([("HOME".into(), home.to_str().unwrap().into())])
        .collect();
    let vars: Vec<(&str, &str)> = vars.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let mut mcp = Mcp::start(&op, &vars);
    let (found, _) = mcp.tool("legionctl_find", &json!({"group": "remote"}));
    assert_eq!(
        found, "1 operations match.\nremote/docs · mcp · Look up docs",
        "only the allowed tool"
    );
    let (groups, _) = mcp.tool("legionctl_find", &json!({}));
    assert!(
        groups.contains("\nremote 1")
            && groups.contains("\nself 1")
            && groups.ends_with("\nMCP server mute did not start in 10 s."),
        "the others answer, and the mute one is one sentence: {groups}"
    );
    let seen = std::fs::read_to_string(home.join("seen.env")).unwrap();
    assert!(
        !seen.contains("LEGIONCTL_MCP_BROKER") && !seen.contains("HEADER_SECRET_VALUE"),
        "a brokered server sees the other servers' secrets: {seen}"
    );
    let (described, _) = mcp.tool("legionctl_describe", &json!({"op": "remote/docs"}));
    assert!(
        described.starts_with("remote/docs · mcp. A tool of the MCP server remote")
            && described.ends_with("Input: {\"type\":\"object\"}"),
        "{described}"
    );
    let (ran, failed) = mcp.tool(
        "legionctl_run",
        &json!({"op": "remote/docs", "input": {"q": "fan"}}),
    );
    assert!(!failed, "{ran}");
    assert!(
        ran.contains("'q': 'fan'") && ran.ends_with("key=HEADER_SECRET_VALUE"),
        "the input and the header reach the server: {ran}"
    );
    a_brokered_retry_gets_the_recorded_answer(&mut mcp);
    let (refused, failed) = mcp.tool("legionctl_run", &json!({"op": "remote/admin"}));
    assert!(
        failed && refused.contains("No operation remote/admin. The closest: remote/docs"),
        "{refused}"
    );
    two_calls_and_no_third_for_the_retry(&op, &id);
    drop(server);
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
    assert!(
        String::from_utf8_lossy(&cli(&["packages", "list"]).stdout)
            .lines()
            .any(|l| l.starts_with("probe") && l.ends_with(" ok"))
    );

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
    let brokered: Value = serde_json::from_str(
        mcp["mcpServers"]["legionctl"]["env"]["LEGIONCTL_MCP_BROKER"]
            .as_str()
            .unwrap(),
    )
    .unwrap();
    let args: Vec<String> = brokered["probe"]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap().into())
        .collect();
    assert!(
        args.contains(&"--no-network".into()) && args.iter().any(|a| a.contains("store/probe/v1")),
        "{args:?}"
    );
    assert_eq!(mcp_tools(&op, &home, &args), ["legionctl_find"]);
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

/// A headless run on Codex's ACP adapter as it answers live (todo-180 U-39: the bound run on Codex): the replayed
/// transcript of @agentclientprotocol/codex-acp 2.2.2 with codex 0.160.1, recorded 2026-10-17 in a bound Codex home
/// (no shell, patch, image or web tool; Luna through `session/set_model`). Its notifications (auth status, session
/// info, usage, a guarded MCP tool call) are no failure, and the answer is the words of its chunks.
#[test]
fn a_run_on_codexs_acp_adapter_takes_its_live_answer_shape() {
    let (op, home) = operator("codex-live");
    let repo = repository(&home);
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    let recording = fixtures.join("codex-acp-live.jsonl");
    write(
        &home.join(".config/legionctl/run.jsonc"),
        &json!({"session": {"acp": {"codex": ["python3", fixtures.join("replay-acp-agent.py"), &recording]},
                            "scope": false, "writable": []}})
        .to_string(),
    );
    let out = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &[
            "session",
            "run",
            "codex",
            "list your tools",
            "--dossier",
            "port",
            "--model",
            "gpt-6-luna[low]",
            "--json",
        ],
        "",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let words: String = std::fs::read_to_string(&recording)
        .unwrap()
        .lines()
        .filter_map(|l| {
            let m: Value = serde_json::from_str(l).unwrap();
            let u = &m["params"]["update"];
            (u["sessionUpdate"] == "agent_message_chunk")
                .then(|| u["content"]["text"].as_str().unwrap().to_string())
        })
        .collect();
    let run: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(run["text"].as_str().unwrap().trim(), words.trim(), "{run}");
    plain::assert_plain("the answer", &words);
    assert!(
        words.contains("echo: hi") && words.contains("shell"),
        "{words}"
    );
}

/// A run whose agent gives no words fails with one plain sentence, never "done" with an empty answer (todo-205, live
/// job 290: Codex answered only "Falling back from `WebSockets` to HTTPS transport. workspace routing discovery failed"
/// and the run was done). Codex: the replayed transcript of @agentclientprotocol/codex-acp 2.2.2 with no path to its
/// service, recorded 2026-10-18 with legionctl's typed-failure capability (its warnings and its error come as typed
/// session failures, none as words); the run fails with the error. Without that capability the adapter sends them as
/// words, as job 290 kept them (codex-acp-no-service-untyped.jsonl, the same run in that shape). Claude's shape: a stand-in that ends its turn with
/// no words; the run fails with the same sentence. Each job is failed with that sentence.
#[test]
fn a_run_whose_agent_gives_no_words_fails_with_one_sentence() {
    let (op, home) = operator("no-words");
    let repo = repository(&home);
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures");
    adapter::executable(
        &home.join(".local/bin/fake-acp"),
        &adapter::node_script(&home),
    );
    write(
        &home.join(".config/legionctl/run.jsonc"),
        &json!({"session": {"acp": {
                    "codex": ["python3", fixtures.join("replay-acp-agent.py"), fixtures.join("codex-acp-no-service.jsonl"),
                              fixtures.join("codex-acp-no-service-untyped.jsonl")],
                    "claude": adapter::COMMAND},
                "scope": false, "writable": []}})
        .to_string(),
    );
    let sentences = [
        (
            "codex",
            "review the plan",
            "the agent stopped with an error: workspace routing discovery failed",
        ),
        (
            "claude",
            "SILENT review the plan",
            "the agent gave no answer",
        ),
    ];
    for (agent, prompt, sentence) in sentences {
        let out = op.run(
            &repo,
            &[("HOME", home.to_str().unwrap())],
            &["session", "run", agent, prompt, "--dossier", agent],
            "",
        );
        let (stdout, stderr) = (
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
        );
        assert!(!out.status.success(), "{agent}: {stdout}");
        assert!(stderr.contains(sentence), "{agent}: {stderr}");
        assert!(
            !stdout.contains("Falling back") && !stderr.contains("Falling back"),
            "{agent}: a transport warning is no answer: {stdout}"
        );
    }
    let jobs: Value = serde_json::from_str(&op.ok(&["jobs", "list", "--json"])).unwrap();
    let mut failed: Vec<&str> = jobs["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{jobs}"))
        .iter()
        .filter(|j| j["kind"] == "session.run" && j["state"] == "failed")
        .filter_map(|j| j["error"].as_str())
        .collect();
    failed.sort_unstable();
    assert_eq!(
        failed,
        [sentences[1].2, sentences[0].2],
        "each job fails with its sentence: {jobs:#}"
    );
}

/// A headless run through an ACP adapter (a stand-in agent): it gets the session's MCP servers at session/new, its
/// permission ask is answered and logged, it works inside the same sandbox and git gate as an interactive session,
/// and its answer comes back. The adapter starts as the defaults start Zed's: bun runs a `#!/usr/bin/env node` script
/// from a folder of `setup apply` (~/.local/bin) that is not on the operator's PATH, and the `node` there fails.
#[test]
fn a_headless_run_drives_an_acp_agent_in_the_session_sandbox() {
    let (op, home) = operator("run");
    let repo = repository(&home);
    let script = adapter::node_script(&home);
    adapter::executable(&home.join(".local/bin/fake-acp"), &script);
    write(
        &home.join(".config/legionctl/run.jsonc"),
        &json!({"session": {"acp": {"fake": adapter::COMMAND},
                            "scope": false, "writable": []}})
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
    // the option the run chose is logged (todo-142 F-69)
    assert_eq!(
        run["permissions"],
        json!(["git push (Execute): allow_once"])
    );
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
    let mut again = Kill::spawn(
        Command::new(env!("CARGO_BIN_EXE_legionctl"))
            .args(["session", "run", "fake", "WAIT go on", "--dossier", "port"])
            .current_dir(&repo)
            .env_clear()
            .env("XDG_RUNTIME_DIR", &op.runtime)
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &home)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    );
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
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut again.stderr.take().unwrap(), &mut stderr).unwrap();
    let rest = seen.iter().collect::<Vec<_>>().join("\n");
    assert!(
        again.wait().unwrap().success(),
        "a run that ended releases its dossier: {stderr}"
    );
    assert!(
        !rest.contains("prompt:") && rest.contains("PUSH_REFUSED") && rest.ends_with(" ms"),
        "each line once, then the result: {rest}"
    );
}

/// A chat or run whose adapter is not installed (the move from ~/.cache/.bun to ~/.bun): legionctl installs it with
/// bun from config `session.acp_packages`, then starts it, and the run's first output line says so (a stand-in bun).
/// A failed install fails the run with its cause; the next run installs again; an installed adapter stays.
#[test]
fn a_headless_run_installs_its_missing_adapter_first() {
    let (op, home) = operator("install");
    let repo = repository(&home);
    adapter::stand_in_bun(&home);
    write(
        &home.join(".config/legionctl/run.jsonc"),
        &json!({"session": {"acp": {"fake": adapter::COMMAND}, "acp_packages": {"fake-acp": "@test/fake-acp"},
                            "scope": false, "writable": []}})
        .to_string(),
    );
    let env = [("HOME", home.to_str().unwrap())];
    let run = || {
        op.run(
            &repo,
            &env,
            &["session", "run", "fake", "hi", "--dossier", "port"],
            "",
        )
    };
    write(&home.join("install-fails"), "");
    let failed = run();
    let stderr = String::from_utf8_lossy(&failed.stderr);
    assert!(
        !failed.status.success()
            && stderr.contains(
                "Installing the adapter fake-acp failed: error: GET https://registry.npmjs.org/@test/fake-acp - 503"
            ),
        "{stderr}"
    );
    std::fs::remove_file(home.join("install-fails")).unwrap();
    let out = run();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        stdout.starts_with("Installing the Fake adapter…\nprompt: hi\n")
            && stdout.matches("prompt: hi").count() == 1,
        "the note, then the answer once: {stdout}"
    );
    plain::assert_plain("the run with its install note", &stdout);
    assert!(run().status.success());
    assert_eq!(adapter::installs(&home), ["add -g @test/fake-acp"; 2]);
}

/// A chat goes on after the computer started again (user finding r340: "Internal error: { \"details\": \"Session not
/// found\" }"): the new adapter process does not know the chat's ACP session, so the run starts a new one with the
/// chat's history as its prompt, logs the lost session, and answers; no JSON-RPC error reaches the user.
#[test]
fn a_chat_goes_on_when_the_agent_lost_its_session() {
    let home = home("lost");
    let rebooted = home.join("rebooted");
    let mut op = operator_with(
        "lost",
        &home,
        &[("FAKE_ACP_FORGET", rebooted.clone().into())],
    );
    let repo = repository(&home);
    let agent =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-acp-agent.py");
    write(
        &home.join(".config/legionctl/run.jsonc"),
        &json!({"session": {"acp": {"fake": ["python3", agent]}, "scope": false, "writable": []}})
            .to_string(),
    );
    let env = [("HOME", home.to_str().unwrap())];
    let ok = |args: &[&str]| {
        let out = op.run(&repo, &env, args, "");
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).into_owned()
    };
    ok(&["dossier", "create", "chat"]);
    ok(&["thread", "start", "a chat", "--dossier", "chat"]);
    let threads: Value = serde_json::from_str(&ok(&["thread", "list", "--json"])).unwrap();
    let id = threads
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["title"] == "a chat")
        .unwrap()["id"]
        .to_string();
    let say = |text: &str| ok(&["session", "run", "fake", text, "--thread", &id]);
    assert!(say("first").contains("acp: new"));
    assert!(
        say("second").contains("acp: loaded"),
        "the same process state: the session loads"
    );
    write(&rebooted, "");
    let after = say("third");
    assert!(
        after.contains("acp: new")
            && after.contains("user: first")
            && after.contains("The user's new message:\nthird"),
        "a new ACP session with the chat's history: {after}"
    );
    let events: Value =
        serde_json::from_str(&op.ok(&["events", "--limit", "1000", "--json"])).unwrap();
    assert!(
        events["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "session.load-lost"
                && e["payload"]["error"]
                    .as_str()
                    .unwrap_or_default()
                    .contains("Session not found")),
        "{events}"
    );
    op.held.push(home);
}

/// SIGTERM ends the operator within the stop time (todo-125: on 2026-10-16 systemd killed it after
/// `TimeoutStopSec=10`): while a headless run holds its ACP agent (a running job) and `trace` follows the event
/// stream, a stop as systemd makes it (SIGTERM to the unit's processes) ends the operator and its agent within half of
/// it. A stop waited for each open request and stream before, and then for each blocking task.
#[test]
fn a_stop_ends_the_operator_with_a_running_job_and_an_open_event_stream() {
    let (mut op, home) = operator("stop");
    let repo = repository(&home);
    let agent =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-acp-agent.py");
    write(
        &home.join(".config/legionctl/run.jsonc"),
        &json!({"session": {"acp": {"fake": ["python3", agent]}, "scope": false, "writable": []}})
            .to_string(),
    );
    let client = |args: &[&str]| {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_legionctl"));
        cmd.args(args)
            .current_dir(&repo)
            .env_clear()
            .env("XDG_RUNTIME_DIR", &op.runtime)
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("HOME", &home)
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        cmd
    };
    let _trace = Kill::spawn(&mut client(&["trace"]));
    let mut run = Kill::spawn(&mut client(&[
        "session",
        "run",
        "fake",
        "WAIT hold",
        "--dossier",
        "port",
    ]));
    let mut first = String::new();
    BufReader::new(run.stdout.take().unwrap())
        .read_line(&mut first)
        .unwrap();
    assert_eq!(
        first.trim(),
        "prompt: WAIT hold",
        "the agent holds its answer"
    );
    let jobs: Value = serde_json::from_str(&op.ok(&["jobs", "list", "--json"])).unwrap();
    assert!(
        jobs.to_string().contains("\"running\""),
        "a job runs: {jobs}"
    );

    let pid = rustix::process::Pid::from_child(&op.child);
    rustix::process::kill_process_group(pid, rustix::process::Signal::TERM).unwrap();
    let stopped = std::time::Instant::now();
    let in_group = || -> Vec<String> {
        std::fs::read_dir("/proc")
            .unwrap()
            .flatten()
            .filter_map(|e| std::fs::read_to_string(e.path().join("stat")).ok())
            .filter(|stat| {
                // pid (comm) state ppid pgrp ...: the comm may hold spaces, so count from its end
                let rest = &stat[stat.rfind(')').unwrap_or(0) + 2..];
                let fields: Vec<&str> = rest.split_whitespace().collect();
                fields.get(2) == Some(&pid.as_raw_nonzero().to_string().as_str())
                    && fields[0] != "Z"
            })
            .collect()
    };
    while op.child.try_wait().unwrap().is_none() || !in_group().is_empty() {
        assert!(
            stopped.elapsed() < std::time::Duration::from_secs(5),
            "5 s after SIGTERM these still run: {:?}",
            in_group()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// A repository whose own image (its Dockerfile) has Python, with the stand-in ACP agent in it (a container sees only
/// the mounted folders), and a config for sessions in a container with the static client of `make static` and the
/// gateway on `gateway` in front of the stand-in provider on `provider`; the interactive agent `probe` tries what an
/// agent would, also a model call to the gateway at 127.0.0.1, as its base URL would say on the host.
fn container_repository(home: &Path, (gateway, provider): (u16, u16)) -> PathBuf {
    let probe = "echo x > note.txt && git add note.txt && git commit -qm probe && echo COMMIT_OK; \
                 git push origin main 2>&1 | grep -q gates.git.deny && echo PUSH_REFUSED; \
                 touch \"$HOME/outside\" && touch agentWorkspaces/talk/MEMORY/probe.md && echo WROTE; \
                 [ -f /.dockerenv ] && echo IN_CONTAINER; echo \"SESSION=$LEGIONCTL_SESSION\"; \
                 grep -q logged-in \"$HOME/.agent-state.json\" && echo STATE_READ; \
                 c=$(git rev-parse --git-common-dir); s=\"$c/refs/heads/legionctl\"; \
                 [ -e \"$s/sib-0/x\" ] && { (echo 0 > \"$s/sib-0/x\") 2>/dev/null || echo SIBLING_READ_ONLY; }; \
                 [ -d \"$s/sib-1\" ] && { (touch \"$s/sib-1/y\") 2>/dev/null || echo NEST_READ_ONLY; }; \
                 grep -q old \"$HOME/.claude/.credentials.json\" && \
                 echo '{\"refresh\": \"new\"}' > \"$HOME/.claude/.credentials.json\" && echo LOGIN_THROUGH; \
                 grep -q old \"$HOME/.codex/auth.json\" && \
                 echo '{\"refresh\": \"new\"}' > \"$HOME/.codex/auth.json\" && echo CODEX_LOGIN_THROUGH; \
                 python3 ask-gateway.py";
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
        // the images of the container tests are in one list, which CI caches (todo-148)
        &format!(
            "FROM {}\n",
            include_str!("../../../tests/fixtures/ci-images.txt")
                .lines()
                .next()
                .unwrap()
        ),
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
    // Claude Code's login as clauth leaves it: a link to the active profile's file
    let alt = home.join(".clauth/profiles/alt/credentials.json");
    write(&alt, "{\"refresh\": \"old\"}");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::os::unix::fs::symlink(&alt, home.join(".claude/.credentials.json")).unwrap();
    // Codex's login as Codex leaves it: a file of its own
    write(&home.join(".codex/auth.json"), "{\"refresh\": \"old\"}");
    write(
        &home.join(".config/legionctl/run.jsonc"),
        &json!({"session": {
            // the real adapters (bun) read their input as a pipe: a port or a file fails them (r380)
            "acp": {"fake": ["sh", "-c", "[ -p /dev/stdin ] && [ -p /dev/stdout ] || \
                { echo 'the adapter has no pipes' >&2; exit 1; }; exec python3 fake-acp-agent.py"]},
            "agents": {"probe": ["sh", "-c", probe]},
            "scope": false,
            "writable": [state, home.join(".claude"), home.join(".codex")],
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

/// The probe refreshed both logins (`LOGIN_THROUGH`, `CODEX_LOGIN_THROUGH` in its `text`), and each refresh reached the
/// real file: Claude's through clauth's link, Codex's own.
fn refresh_reached_clauth(home: &Path, text: &str) {
    assert!(text.contains("LOGIN_THROUGH"), "{text}");
    assert!(text.contains("CODEX_LOGIN_THROUGH"), "{text}");
    assert_eq!(
        std::fs::read_to_string(home.join(".codex/auth.json"))
            .unwrap()
            .trim(),
        "{\"refresh\": \"new\"}",
        "Codex's refresh reached its login file"
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".clauth/profiles/alt/credentials.json"))
            .unwrap()
            .trim(),
        "{\"refresh\": \"new\"}",
        "the refresh reached the file that clauth's link points to"
    );
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

/// The container tests share the agent image of `repo` (each test's repository has that name), also with the tests of
/// other checkouts on this machine (one Docker): one at a time, by a lock on a file that every checkout uses.
fn repo_image() -> std::fs::File {
    let lock = Path::new("/var/tmp/legionctl-test-cache");
    std::fs::create_dir_all(lock).unwrap();
    let file = std::fs::File::create(lock.join("repo-image.lock")).unwrap();
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive).unwrap();
    file
}

/// A session in a container reaches its granted connections through the gateway's tunnel as on the host (r393: Codex
/// in a container could not reach chatgpt.com: the gateway knew sessions only by their host scope, so it took the
/// container's caller for a program outside sessions). The gateway knows it by the container that legionctl named;
/// a host the session was not granted is still refused. Needs docker and `make static`.
#[test]
fn a_container_session_reaches_its_granted_connections_through_the_gateway_tunnel() {
    let _image = repo_image();
    let home = home("ctunnel");
    let op = operator_with("ctunnel", &home, &docker());
    let (_provider, ports) = gateway_ports(&home);
    let repo = container_repository(&home, ports);
    drop(op);
    let op = operator_with("ctunnel2", &home, &docker());
    let server = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = server.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in server.incoming().flatten() {
            let mut stream = stream;
            let mut request = [0; 1024];
            let _ = std::io::Read::read(&mut stream, &mut request);
            let _ = std::io::Write::write_all(&mut stream, b"HTTP/1.0 200 OK\r\n\r\nTUNNEL_OK");
        }
    });
    write(
        &home.join(".config/legionctl/capabilities/service.jsonc"),
        &json!({"name": "service", "version": 1, "connections": {
            "out": [{"host": "localhost", "port": port, "why": "the test's server"}]}})
        .to_string(),
    );
    write(
        &home.join(".config/legionctl/runnables.jsonc"),
        r#"{ "runnables": { "allow+": ["service"] } }"#,
    );
    // Python's HTTP client through the session's HTTPS_PROXY, as an agent's client does: a granted host, then another
    let probe = format!(
        "import http.client, os\n\
         proxy = os.environ['HTTPS_PROXY'].split('//')[1].split(':')\n\
         for host in ['localhost', '127.0.0.1']:\n\
         \x20   c = http.client.HTTPConnection(proxy[0], int(proxy[1]), timeout=20)\n\
         \x20   c.set_tunnel(host, {port})\n\
         \x20   try:\n\
         \x20       c.request('GET', '/')\n\
         \x20       print(host, c.getresponse().read().decode())\n\
         \x20   except OSError as e:\n\
         \x20       print(host, 'REFUSED', e)\n"
    );
    write(
        &home.join(".config/legionctl/tunnel.jsonc"),
        &json!({"session": {"agents": {"tunnel": ["python3", "-c", probe]}}}).to_string(),
    );
    let started = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &[
            "session",
            "start",
            "tunnel",
            "--dossier",
            "talk",
            "--in",
            "container",
            "--capability",
            "service",
        ],
        "",
    );
    let text = String::from_utf8_lossy(&started.stdout);
    assert!(
        text.contains("localhost TUNNEL_OK") && text.contains("127.0.0.1 REFUSED"),
        "{text}{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let events: Value =
        serde_json::from_str(&op.ok(&["events", "--limit", "1000", "--json"])).unwrap();
    let tunnels: Vec<&Value> = events["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == "gateway.tunnel")
        .collect();
    assert_eq!(tunnels.len(), 1, "{events:#}");
    assert!(
        tunnels[0]["payload"]["who"]
            .as_str()
            .unwrap()
            .starts_with("session:"),
        "the container's caller is its session: {tunnels:?}"
    );
    let _ = std::process::Command::new("docker")
        .args(["rmi", "-f", "localhost/repo/legionctl-agent:latest"])
        .output();
}

/// W5 (note 18): `workspace pack` puts the agent layer on the repository's own image (its Dockerfile, built by I1),
/// and sessions `--in container` run there, headless and interactive (through the real client): the workspace, its
/// git data and the dossier are mounted at their own paths, so the agent's commits land in the workspace on the host;
/// the image's `/usr/bin/git` is the static client, which asks the operator through the mounted socket and refuses
/// the push; the home is private; the sessions are recorded as containers'. A run with no image, or with an image of
/// another agent layer, makes it first and says so. The adapter gets pipes. Needs docker and `make static`.
#[test]
fn sessions_in_a_container_work_on_the_mounted_workspace_behind_the_git_gate() {
    let _image = repo_image();
    let home = home("container");
    let op = operator_with("container", &home, &docker());
    let (_provider, ports) = gateway_ports(&home);
    let repo = container_repository(&home, ports);
    drop(op);
    let op = operator_with("container2", &home, &docker());
    let run = |args: &[&str]| op.run(&repo, &[("HOME", home.to_str().unwrap())], args, "");
    let words = |line: &'static str| line.split(' ').collect::<Vec<_>>();
    let in_container = words("session run fake fix --dossier port --in container");
    let image = "localhost/repo/legionctl-agent:latest";
    let rmi = || {
        std::process::Command::new("docker")
            .args(["rmi", "-f", image])
            .output()
    };
    let _ = rmi();

    // no image: the run makes it first, and works there
    let made = run(&words("session run fake fix --dossier port --in container"));
    let text = String::from_utf8_lossy(&made.stdout);
    assert!(
        made.status.success() && text.starts_with("Making the agent image of this repository."),
        "{text}{}",
        String::from_utf8_lossy(&made.stderr)
    );
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
    an_image_of_another_agent_layer_is_made_again(image, &|| run(&in_container));
    let packed = run(&["workspace", "pack"]);
    assert!(
        String::from_utf8_lossy(&packed.stdout)
            .contains(&format!("packed {image} from localhost/repo/repo:")),
        "the agent layer goes on the repository's own image: {}",
        String::from_utf8_lossy(&packed.stderr)
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
    refresh_reached_clauth(&home, &text);
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
        Path::is_file(&workspace.join("agentWorkspaces/talk/MEMORY/probe.md")),
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

/// An agent image of another agent layer (an older pack, its label differs): the next run makes it again first.
fn an_image_of_another_agent_layer_is_made_again(
    image: &str,
    run: &dyn Fn() -> std::process::Output,
) {
    let old = std::process::Command::new("docker")
        .args(["build", "-q", "-t", image, "-"])
        .stdin(std::process::Stdio::piped())
        .spawn()
        .and_then(|mut b| {
            use std::io::Write;
            write!(
                b.stdin.take().unwrap(),
                "FROM {image}\nLABEL org.legionctl.layer=old\n"
            )?;
            b.wait()
        })
        .unwrap();
    assert!(old.success());
    let again = run();
    let text = String::from_utf8_lossy(&again.stdout);
    assert!(
        again.status.success() && text.starts_with("Updating the agent image."),
        "{text}{}",
        String::from_utf8_lossy(&again.stderr)
    );
}

/// 40 other branches in folders of their own in the folder of the sessions' branches, half of them packed: each is
/// read-only in the session (C7), and the VM still has room for its devices (r390: one share per folder ran out of
/// PCI slots). Returns the value of `legionctl/sib-0/x`.
fn sibling_branches(home: &Path, repo: &Path) -> String {
    for i in 0..40 {
        git(home, repo, &["branch", &format!("legionctl/sib-{i}/x")]);
        if i == 19 {
            git(home, repo, &["pack-refs", "--all"]);
        }
    }
    git(home, repo, &["rev-parse", "legionctl/sib-0/x"])
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
    let home = home(tag);
    let op = operator_in(tag, &home);
    let (provider, ports) = gateway_ports(&home);
    let repo = container_repository(&home, ports);
    drop(op);
    let mut op = operator_in(again, &home);
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
    let path = home.to_path_buf();
    op.held.push(home);
    (op, path, repo, provider)
}

/// W5 part 3 (note 18): the first session makes the repository's disk from a cloud image (checked against its
/// SHA512SUMS) in one cloud-init boot, a disk of another pack is made again, the adapter gets pipes (bun reads a port
/// as a file, r380), and sessions `--in vm` run on a throwaway layer over it, headless and through
/// the real client: the workspace, its git data and the dossier are virtiofs shares at their own paths, so commits
/// land in the workspaces on the host; the disk's `/usr/bin/git` is the static client, which reaches the operator
/// through the guest's relay and QEMU's forward, and refuses the push. Boots VMs: needs KVM, QEMU, virtiofsd,
/// xorriso, `make static` and the Debian cloud image (downloaded once into /var/tmp/legionctl-test-cache).
#[test]
#[ignore = "boots VMs (KVM, QEMU, virtiofsd, a cloud image): CI runs it in its own step"]
fn sessions_in_a_vm_work_on_the_shared_workspace_behind_the_git_gate() {
    let (op, home, repo, _provider) = vm_repository("vm", "vm2");
    let run = |args: &[&str]| op.run(&repo, &[("HOME", home.to_str().unwrap())], args, "");
    let sibling = sibling_branches(&home, &repo);

    let words = |line: &'static str| line.split(' ').collect::<Vec<_>>();
    // no disk: the first run makes it (here one that asks for the GPU, which this disk does not have)
    let gpu = run(&words("session run fake fix --dossier port --in vm --gpu"));
    assert!(
        String::from_utf8_lossy(&gpu.stdout).starts_with("Making the VM disk of this repository."),
        "{}",
        String::from_utf8_lossy(&gpu.stdout)
    );
    assert!(
        String::from_utf8_lossy(&gpu.stderr).contains(
            "has no NVIDIA driver: make it with `legionctl workspace pack --as vm --gpu`"
        ),
        "{}",
        String::from_utf8_lossy(&gpu.stderr)
    );
    // a disk of another pack (its mark differs: an older client, whose unit ran the disk's own client that could not
    // read a newer guest spec, r368) is packed again by the operator before the run, and the run works on it
    let disk = home.join(".local/share/legionctl/vm/repo.qcow2");
    let mark = disk.with_extension("boot");
    std::fs::write(&mark, "old").unwrap();
    let out = run(&words("session run fake fix --dossier port --in vm"));
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(
        text.starts_with("Updating the VM disk."),
        "the progress line comes first:\n{text}"
    );
    assert!(mark.is_file(), "the disk is packed again, with its mark");
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
        "SIBLING_READ_ONLY",
        "NEST_READ_ONLY",
    ] {
        assert!(
            text.contains(expected),
            "{expected} missing:\n{text}\n{}",
            String::from_utf8_lossy(&started.stderr)
        );
    }
    assert_eq!(
        git(&home, &repo, &["rev-parse", "legionctl/sib-0/x"]),
        sibling,
        "the other branches are as they were"
    );
    // git packing the refs (gc, pack-refs) keeps the sessions' branch folder, which the mounts of a session hold (r393:
    // a pack during a VM's boot took its mount point away)
    git(&home, &repo, &["pack-refs", "--all", "--prune"]);
    assert!(
        repo.join(".git/refs/heads/legionctl").is_dir(),
        "the branch folder outlives a pack"
    );
    assert!(!home.join("outside").exists(), "the VM's home is its own");
    refresh_reached_clauth(&home, &text);
    assert_eq!(
        sandbox_calls(&home),
        1,
        "the model call from the VM went through the host's gateway"
    );
    let talk = home.join("worktrees/repo/talk");
    assert_eq!(git(&home, &talk, &["log", "-1", "--format=%s"]), "probe");
    assert!(
        talk.join("agentWorkspaces/talk/MEMORY/probe.md").exists(),
        "the dossier is shared"
    );
    a_failing_guest_fails_the_run_with_its_own_line(&home, &run);
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

/// A guest that fails (a stand-in static client that writes the guest's line and powers off): the run fails with the
/// guest's own line in one sentence, not the closed ACP transport (r368).
fn a_failing_guest_fails_the_run_with_its_own_line(
    home: &Path,
    run: &dyn Fn(&[&str]) -> std::process::Output,
) {
    let stand_in = home.join("failing-guest");
    write(
        &stand_in,
        "#!/bin/sh\necho 'legionctl guest: mount /work: no such share (stand-in)' >&2\nsystemctl poweroff\n",
    );
    std::fs::set_permissions(
        &stand_in,
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    write(
        &home.join(".config/legionctl/vm-guest.jsonc"),
        &json!({"session": {"container": {"client": stand_in}}}).to_string(),
    );
    let failed = run(&[
        "session",
        "run",
        "fake",
        "fix",
        "--dossier",
        "port",
        "--in",
        "vm",
    ]);
    assert!(!failed.status.success());
    assert_eq!(
        String::from_utf8_lossy(&failed.stderr).trim(),
        "Error: the agent stopped before it answered: the VM did not start the agent: mount /work: no such share \
         (stand-in)"
    );
    // a VM that QEMU cannot start: the run fails with QEMU's own line (r390: the console was empty)
    write(
        &home.join(".config/legionctl/zz-vm-cpus.jsonc"),
        &json!({"session": {"vm": {"cpus": 99_999}}}).to_string(),
    );
    let failed = run(&[
        "session",
        "run",
        "fake",
        "fix",
        "--dossier",
        "port",
        "--in",
        "vm",
    ]);
    let said = String::from_utf8_lossy(&failed.stderr);
    assert!(
        said.trim().starts_with(
            "Error: the agent stopped before it answered: the VM failed: qemu-system-x86_64: "
        ),
        "{said}"
    );
    std::fs::remove_file(home.join(".config/legionctl/zz-vm-cpus.jsonc")).unwrap();
}

/// Jobs (DESIGN/02): a headless run is a job; when the operator stops in the middle of it, the next operator goes on
/// with it in the same session (its ACP session loaded, told that it was cut off) and records how it ended.
#[test]
fn a_headless_run_cut_off_by_a_restart_goes_on_in_its_session() {
    let home = home("jobs");
    let op = operator_in("jobs", &home);
    let repo = repository(&home);
    let agent =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-acp-agent.py");
    write(
        &home.join(".config/legionctl/run.jsonc"),
        &json!({"session": {"acp": {"fake": ["python3", agent]}, "scope": false, "writable": []}})
            .to_string(),
    );
    // a long run (the stand-in waits 30 s on SLOW), from a client that stays connected
    let client = Kill::spawn(
        Command::new(env!("CARGO_BIN_EXE_legionctl"))
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
            .stderr(Stdio::null()),
    );
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
    drop(client);

    let again = Operator::start(
        "jobs2",
        &[
            ("HOME", home.as_os_str().to_owned()),
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
    let mut client = Kill::spawn(
        Command::new(env!("CARGO_BIN_EXE_legionctl"))
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
            .stderr(Stdio::piped()),
    );
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
    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut client.stderr.take().unwrap(), &mut stderr).unwrap();
    client.wait().unwrap();
    assert!(
        stderr.contains("offloaded: memory is under pressure"),
        "{stderr}"
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

/// `session start probe` in `repo` for `dossier`: its id and its sandbox's token.
fn started_session(op: &Operator, repo: &Path, home: &Path, dossier: &str) -> (String, String) {
    let out = op.run(
        repo,
        &[("HOME", home.to_str().unwrap())],
        &["session", "start", "probe", "--dossier", dossier, "--json"],
        "",
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let launch: Value = serde_json::from_slice(&out.stdout).unwrap();
    let argv: Vec<&str> = launch["sandbox"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    let at = argv.iter().position(|a| *a == "LEGIONCTL_TOKEN").unwrap();
    (
        launch["id"].as_str().unwrap().to_string(),
        argv[at + 1].to_string(),
    )
}

/// Durability by kind (EV-review-16 B7): a commit of control state (here a dossier binding of `dossier use`) syncs
/// the operator database's WAL (synchronous FULL); only the recorder's event log does not (its flood tests).
#[test]
fn a_control_state_commit_syncs_the_database() {
    let home = home("durable");
    let log = home.join("syncs.log");
    let tracer: Vec<OsString> = [
        "strace",
        "-f",
        "-qq",
        "-y",
        "-e",
        "trace=fsync,fdatasync",
        "-o",
    ]
    .map(OsString::from)
    .into_iter()
    .chain([log.clone().into_os_string()])
    .collect();
    let op = operator_under("durable", &home, &[], &tracer);
    let repo = repository(&home);
    op.ok(&["events"]);
    // a connection of this test stays open, so the operator's last connection closes with no checkpoint (which syncs
    // the WAL also with NORMAL)
    let _held = legionctl_core::db::Db::open(
        &home.join(".local/state/legionctl/operator.db"),
        legionctl_core::db::OPERATOR,
    )
    .unwrap();
    let bind = |session: &str, dossier: &str| {
        op.run(
            &repo,
            &[
                ("HOME", home.to_str().unwrap()),
                ("LEGIONCTL_SESSION", session),
            ],
            &["dossier", "use", dossier],
            "",
        )
    };
    // the first write after the open makes the WAL anew, which syncs also with NORMAL
    assert!(bind("s1", "port").status.success());
    let before = std::fs::read_to_string(&log).unwrap().len();
    let used = bind("s2", "other");
    assert!(
        used.status.success(),
        "{}",
        String::from_utf8_lossy(&used.stderr)
    );
    let syncs = std::fs::read_to_string(&log).unwrap().split_off(before);
    assert!(
        syncs.contains("operator.db-wal>"),
        "no sync of the WAL: {syncs}"
    );
}

/// A session's token works only while its session runs (EV-review-16 C3): after `session stop` (its scope still
/// answers active), after its scope or container ended (the state at the call), and after a headless run, the token is
/// refused; a scope that ended takes the token away, so the next call finds no such token.
#[test]
fn a_session_token_works_only_while_its_session_runs() {
    let (op, home) = operator("tokenlife");
    let repo = repository(&home);
    let script = adapter::node_script(&home);
    adapter::executable(&home.join(".local/bin/fake-acp"), &script);
    write(
        &home.join(".config/legionctl/session.jsonc"),
        &json!({"session": {"agents": {"probe": ["sh", "-c", "true"]}, "acp": {"fake": adapter::COMMAND},
                            "scope": false, "writable": []}})
        .to_string(),
    );
    let home_env = ("HOME", home.to_str().unwrap());
    let call = |token: &str| {
        op.run(
            &repo,
            &[home_env, ("LEGIONCTL_TOKEN", token)],
            &["dossier", "list"],
            "",
        )
    };
    let refused = |token: &str, why: &str| {
        let out = call(token);
        let said = String::from_utf8_lossy(&out.stderr);
        assert!(
            !out.status.success() && said.contains(why),
            "{why} missing: {said}"
        );
    };
    let start = |dossier: &str| started_session(&op, &repo, &home, dossier);
    // the scope answers: active while the file `scope-ended` is not there
    stand_in(
        &home,
        "systemctl",
        &format!(
            "#!/bin/sh\ncase \"$*\" in *is-active*) [ -e {ended} ] && echo inactive && exit 3; echo active;; \
             *FreezerState*) echo running;; esac; exit 0\n",
            ended = home.join("scope-ended").display()
        ),
    );

    let (stopped, token) = start("one");
    assert!(
        call(&token).status.success(),
        "{}",
        String::from_utf8_lossy(&call(&token).stderr)
    );
    sql(
        &home,
        &format!("UPDATE sessions SET scoped = 1 WHERE id = '{stopped}'"),
    );
    assert!(call(&token).status.success(), "a scope that runs");
    op.ok(&["session", "stop", &stopped]);
    refused(&token, "the token is not valid");

    let (ended, token) = start("two");
    sql(
        &home,
        &format!("UPDATE sessions SET scoped = 1 WHERE id = '{ended}'"),
    );
    assert!(call(&token).status.success());
    write(&home.join("scope-ended"), "");
    refused(
        &token,
        &format!("session {ended} ended, so its token is not valid now"),
    );
    std::fs::remove_file(home.join("scope-ended")).unwrap();
    refused(&token, "the token is not valid");

    let run = op.run(
        &repo,
        &[home_env],
        &[
            "session",
            "run",
            "fake",
            "TOKEN fix the bug",
            "--dossier",
            "three",
        ],
        "",
    );
    let said = String::from_utf8_lossy(&run.stdout);
    assert!(
        run.status.success(),
        "{}",
        String::from_utf8_lossy(&run.stderr)
    );
    let token = said
        .lines()
        .find_map(|l| l.strip_prefix("token: "))
        .unwrap_or_else(|| panic!("the run's token: {said}"));
    assert!(!token.is_empty(), "{said}");
    refused(token, "the token is not valid");
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
    assert!(
        said[0].starts_with(&format!(
            "linked {}",
            repo.join("agentWorkspaces").display()
        )) && said[1].starts_with("created "),
        "{said:?}"
    );
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

/// A plain folder's merge never loses what the user changed after the session's copy and never writes outside the
/// folder: the session edits `edit` and `deep/file`, adds `new` and deletes `gone`; then the user edits `edit` and
/// moves `deep` to a symbolic link to a folder outside. The merge writes `new` and `gone`, keeps the user's `edit` and
/// the outside file, names both in one sentence, and keeps only them in the layer; the source is never half written.
#[test]
fn a_plain_folder_merge_keeps_what_the_user_changed_since_and_writes_only_inside_the_folder() {
    let (op, home) = operator("folder-conflict");
    let (notes, outside) = (home.join("notes"), home.join("outside"));
    for (file, text) in [("gone", "g\n"), ("edit", "e\n"), ("deep/file", "d\n")] {
        write(&notes.join(file), text);
    }
    write(&outside.join("file"), "outside\n");
    let work = "rm gone; echo session >> edit; echo session >> deep/file; echo n > new";
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
    // the user's work after the session's
    write(&notes.join("edit"), "the user's\n");
    std::fs::remove_dir_all(notes.join("deep")).unwrap();
    std::os::unix::fs::symlink(&outside, notes.join("deep")).unwrap();

    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", launch["id"].as_str().unwrap()),
    ];
    let merge = op.run(&notes, &env, &["workspace", "merge"], "");
    let said = String::from_utf8_lossy(&merge.stderr);
    assert!(!merge.status.success(), "{said}");
    assert!(
        said.contains("merged D gone, A new")
            && said.contains("changed in")
            && said.contains(
                "after the session changed them, so the merge did not write them: deep/file, edit."
            )
            && said.contains("workspace close --force"),
        "{said}"
    );
    let read = |path: &Path| std::fs::read_to_string(path).ok();
    assert_eq!(
        (read(&notes.join("gone")), read(&notes.join("new"))),
        (None, Some("n\n".into()))
    );
    assert_eq!(read(&notes.join("edit")).as_deref(), Some("the user's\n"));
    assert_eq!(
        read(&outside.join("file")).as_deref(),
        Some("outside\n"),
        "no write through the link"
    );
    let left: Vec<_> = std::fs::read_dir(&notes)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains("legionctl-merge"))
        .collect();
    assert!(left.is_empty(), "no half-written entry: {left:?}");
    let status = op.run(&notes, &env, &["workspace", "status", "--json"], "");
    let status: Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(
        status["files"],
        json!(["M deep/file", "M edit"]),
        "only the kept changes"
    );
    let closed = op.run(&notes, &env, &["workspace", "close", "--force"], "");
    assert!(
        closed.status.success(),
        "{}",
        String::from_utf8_lossy(&closed.stderr)
    );
    assert_eq!(read(&notes.join("edit")).as_deref(), Some("the user's\n"));
}

/// `src/lib.rs` in `repo` with `parse` and `keep`, both committed in 2001; `parse` changed again in a commit of now.
fn lib_with_dated_symbols(home: &Path, repo: &Path) {
    let lib = |one: &str| format!("fn parse() {{\n    {one}\n}}\n\nfn keep() {{\n    2\n}}\n");
    write(&repo.join("src/lib.rs"), &lib("1"));
    git(home, repo, &["add", "."]);
    let old = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["commit", "--quiet", "-m", "lib"])
        .env("HOME", home)
        .env("GIT_COMMITTER_DATE", "2001-01-01T00:00:00")
        .status()
        .unwrap();
    assert!(old.success());
    write(&repo.join("src/lib.rs"), &lib("10"));
    git(home, repo, &["commit", "--quiet", "-am", "parse"]);
}

/// The work graph from a dossier's files: plan rows and artifacts with front matter; `dossier check` finds a link
/// to a row that does not exist, an acceptance row without EVIDENCE, and EVIDENCE whose code changed after it was
/// written, anchored to the symbol it names (todo-156): `src/lib.rs#parse` changed after its EVIDENCE was written,
/// `src/lib.rs#keep` did not (its file did), a symbol that is not there is a finding, and whole-file links count once
/// as unanchored; `dossier graph` shows a row with what supports it.
#[test]
fn dossier_check_finds_dangling_links_unverified_acceptance_and_stale_evidence() {
    let (op, home) = operator("graph");
    let repo = repository(&home);
    lib_with_dated_symbols(&home, &repo);
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
    let dossier = home.join("agentWorkspaces/port");
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
    for (n, symbol) in [(3, "parse"), (4, "keep"), (5, "gone")] {
        artifact(
            &format!("EVIDENCE/validation/0{n}-{symbol}.md"),
            &format!("id: EV-0{n}\nsupports: [plan#acceptance-1]\ncode: [src/lib.rs#{symbol}]"),
        );
    }
    // a plan row that names an artifact by path reaches it by its id
    let plan = std::fs::read_to_string(dossier.join("plan.md"))
        .unwrap()
        .replace(
            "| acceptance-2 | tests pass | done |",
            "| acceptance-2 | tests pass (EVIDENCE/validation/02-tests.md) | done |",
        );
    write(&dossier.join("plan.md"), &plan);
    // EV-01 was written long before README.md's last commit, EV-03..05 in 2010
    for (file, hours) in [
        ("01-build", 262_968),
        ("03-parse", 350_640),
        ("04-keep", 350_640),
        ("05-gone", 350_640),
    ] {
        std::fs::File::options()
            .write(true)
            .open(dossier.join(format!("EVIDENCE/validation/{file}.md")))
            .unwrap()
            .set_modified(std::time::UNIX_EPOCH + std::time::Duration::from_hours(hours))
            .unwrap();
    }

    let check: Value = serde_json::from_str(&ok(&["dossier", "check", "--json"])).unwrap();
    assert_eq!(
        check["problems"],
        json!([
            "MEMORY/01-note supports plan#acceptance-9, which does not exist",
            "plan#acceptance-3 has no EVIDENCE that supports it",
            "EV-03 is stale: parse in src/lib.rs changed after it was written",
            "EV-05 links src/lib.rs#gone: git log -1 -s --format=%ct -L:gone:src/lib.rs: fatal: -L parameter 'gone' starting at line 1: no match",
            "1 code links of EVIDENCE name a whole file (unanchored): their staleness is not checked; link \
             <path>#<symbol>",
            // the repository declares no checks (design 23, E2 rule 2)
            format!(
                "{}: this repository has no checks of kind format, lint, build, test",
                home.join("worktrees/repo/port").display()
            )
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

/// A compaction defers to the dossier (todo-156): `dossier health` gives a count line and one keyed line per stale or
/// ambiguous finding (a row says not merged of a commit that main has, a row id twice, an orphan artifact); the
/// `PreCompact` hook writes an immutable HEALTH note and a JOURNAL entry with this session's status changes and the
/// note's link; the session start after the compaction points to both; JOURNAL entries and an artifact marked
/// `immutable: true` refuse each change.
#[test]
fn a_compaction_leaves_an_immutable_journal_and_health_note_that_the_next_start_points_to() {
    let (op, home) = operator("compact");
    let repo = repository(&home);
    let env = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "s1"),
    ];
    let run = |args: &[&str], stdin: &str| op.run(&repo, &env, args, stdin);
    let ok = |args: &[&str], stdin: &str| {
        let o = run(args, stdin);
        assert!(
            o.status.success(),
            "{args:?}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        String::from_utf8_lossy(&o.stdout).to_string()
    };
    ok(&["dossier", "use", "port"], "");
    ok(&["workspace", "open"], "");
    let merged = git(&home, &repo, &["rev-parse", "--short", "HEAD"]);
    let dossier = home.join("agentWorkspaces/port");
    let plan = std::fs::read_to_string(dossier.join("plan.md"))
        .unwrap()
        .replace(
            "## ToDo\n",
            &format!(
                "## ToDo\n| id | item | status |\n| --- | --- | --- |\n\
             | todo-1 | parser | built {merged}, not merged |\n| todo-2 | lexer | open |\n"
            ),
        );
    ok(&["dossier", "write", "port/plan.md", "--overwrite"], &plan);
    // a second todo-2 that came in outside legionctl, and a note nothing links
    let row = "| todo-2 | lexer | open |\n";
    write(
        &dossier.join("plan.md"),
        &plan.replace(row, &format!("{row}{row}")),
    );
    write(&dossier.join("MEMORY/01-loose.md"), "# loose\n");

    let health = ok(&["dossier", "health"], "");
    for line in [
        "port: 4 findings (check 1, duplicate 1, orphan 1, status-vs-git 1)",
        &format!("status-vs-git: plan#todo-1 says not merged, but main has {merged}"),
        "duplicate: plan#todo-2 is a plan row id twice",
        "orphan: MEMORY/01-loose (MEMORY/01-loose.md) has no link to or from it",
    ] {
        assert!(health.contains(line), "{line} missing:\n{health}");
    }

    let compact = json!({"cwd": repo, "session_id": "s1"});
    let out = op.run(
        &repo,
        &[],
        &["hook", "pre-compact"],
        &claude_hook("pre-compact", &compact),
    );
    assert!(out.status.success() && out.stdout.is_empty(), "{out:?}");
    let journal = std::fs::read_to_string(dossier.join("JOURNAL/01-compaction.md")).unwrap();
    assert!(
        journal.contains("- plan#todo-1: new -> built")
            && journal.contains("Health: HEALTH/01-before-compaction."),
        "{journal}"
    );
    let note = std::fs::read_to_string(dossier.join("HEALTH/01-before-compaction.md")).unwrap();
    assert!(note.contains("port: 4 findings"), "{note}");

    let start = json!({"cwd": repo, "session_id": "s1", "source": "compact"});
    let out = op.run(
        &repo,
        &[],
        &["hook", "session-start"],
        &claude_hook("session-start", &start),
    );
    let told = String::from_utf8_lossy(&out.stdout);
    assert!(
        told.contains(
            "Read port/plan.md, port/JOURNAL/01-compaction.md and port/HEALTH/01-before-compaction.md first"
        ),
        "{told}"
    );

    immutable_entries_refuse_each_change(&op, &repo, &env);
    assert_eq!(
        std::fs::read_to_string(dossier.join("JOURNAL/01-compaction.md")).unwrap(),
        journal
    );
}

/// JOURNAL entries and an artifact marked `immutable: true` refuse each change (todo-156).
fn immutable_entries_refuse_each_change(op: &Operator, repo: &Path, env: &[(&str, &str)]) {
    let run = |args: &[&str], stdin: &str| op.run(repo, env, args, stdin);
    let refused = |args: &[&str], stdin: &str| {
        let o = run(args, stdin);
        let text = String::from_utf8_lossy(&o.stderr).to_string();
        assert!(
            !o.status.success() && text.contains("is immutable once written"),
            "{args:?}: {text}"
        );
    };
    let journal = "port/JOURNAL/01-compaction.md";
    refused(&["dossier", "write", journal, "--overwrite"], "# new\n");
    refused(&["dossier", "append", journal], "- one more\n");
    let fixed = "port/MEMORY/02-fixed.md";
    let o = run(
        &["dossier", "write", fixed],
        "---\nimmutable: true\n---\n# fixed\n",
    );
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    refused(&["dossier", "append", fixed], "- later\n");
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
    let home = Scratch::new(format!(
        "/var/tmp/legionctl-e2e-plugins-{}",
        std::process::id()
    ));
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
    let op = Operator::start(
        "plugins",
        &[("HOME", home.as_os_str().to_owned()), ("PATH", path)],
    );
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

/// `agents set` on a settings file that it cannot read (Latin-1, not UTF-8) fails with the path, and the file keeps
/// every byte: a read error is not an empty file that the write replaces with the one key.
#[test]
fn agents_set_never_writes_over_a_settings_file_that_it_cannot_read() {
    let (op, home) = operator("agentsetunread");
    let env = [("HOME", home.to_str().unwrap())];
    let codex = home.join(".codex/config.toml");
    std::fs::create_dir_all(codex.parent().unwrap()).unwrap();
    let bytes = b"model = \"mod\xe8le\"\n".to_vec();
    std::fs::write(&codex, &bytes).unwrap();
    let o = op.run(
        &home,
        &env,
        &["agents", "set", "codex", "user", "model", "gpt", "--json"],
        "",
    );
    let err = String::from_utf8_lossy(&o.stderr);
    assert!(!o.status.success() && err.contains("config.toml"), "{err}");
    assert_eq!(
        std::fs::read(&codex).unwrap(),
        bytes,
        "the file keeps every byte"
    );
    assert_eq!(
        std::fs::read_dir(codex.parent().unwrap()).unwrap().count(),
        1,
        "no backup and no temporary file"
    );
}

/// Usage counts each Claude Code message once (its usage repeats on every content block) and each Codex turn. The
/// rate-limit windows come from the providers (`agents accounts usage`), not from the transcripts.
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

/// `images build` in a git repository whose `git rev-parse` fails (no commit yet) fails with git's error before any
/// build: the tag is the commit, never `dev` as for a folder outside git.
#[test]
fn images_build_fails_when_git_cannot_give_the_commit() {
    let (op, home) = operator("imgnocommit");
    let repo = home.join("fresh");
    std::fs::create_dir_all(&repo).unwrap();
    git(&home, &repo, &["init", "--quiet"]);
    write(&repo.join("svc/Dockerfile"), "FROM scratch\n");
    let env = [("HOME", home.to_str().unwrap())];
    let built = op.run(&repo, &env, &["images", "build"], "");
    let err = String::from_utf8_lossy(&built.stderr);
    assert!(
        !built.status.success() && err.contains("git rev-parse --short HEAD"),
        "{err}"
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
    // the user allowed the repository's compose file (A-5), as docker compose reads it
    let model = Command::new("docker")
        .current_dir(&repo)
        .args([
            "compose",
            "-p",
            "repo",
            "-f",
            "compose.yaml",
            "config",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    trust::allow_value(
        &home,
        &repo.join("compose.yaml"),
        &serde_json::from_slice(&model.stdout).unwrap(),
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
        .env("PATH", path_with_build())
        .env("XDG_RUNTIME_DIR", &op.runtime)
        .output()
        .unwrap()
}

/// What a hostile agent tries in its sandbox: $1 a host PID, $2 a port on the host's loopback, $3 the repository's
/// git folder, $4 the main checkout.
const ESCAPE_PROBE: &str = r#"
        echo x > note.txt && git add note.txt && git commit -qm probe && echo COMMIT_OK
        [ -d /proc/$1 ] && echo HOST_PID_SEEN
        echo pwn > /proc/$1/root$HOME/pwned 2>/dev/null && echo PROC_ROOT_WRITTEN
        python3 -c 'import socket, sys; socket.create_connection(("127.0.0.1", int(sys.argv[1])), 2)' $2 2>/dev/null           && echo LOOPBACK_REACHED
        python3 -c 'import json, os, urllib.request as u
body = json.dumps({"model": "in-sandbox", "max_tokens": 5, "messages": [{"role": "user", "content": "hi"}]}).encode()
h = {"x-api-key": "caller-key", "anthropic-version": "2023-06-01", "content-type": "application/json"}
r = u.urlopen(u.Request(os.environ["ANTHROPIC_BASE_URL"] + "/v1/messages", body, h), timeout=20)
print("GATEWAY_" + str(r.status))'
        echo '{"hooks": "planted"}' > ~/.claude/settings.json && echo CLAUDE_WRITE_KEPT
        echo planted > ~/.cache/sccache/kept && echo CACHE_WRITE_KEPT
        printf '[core]
	fsmonitor = touch /tmp/pwn
' >> $3/config 2>/dev/null && echo GIT_CONFIG_WRITTEN
        echo 'touch /tmp/pwn' > $3/hooks/post-checkout 2>/dev/null && echo GIT_HOOK_WRITTEN
        echo 0000000000000000000000000000000000000000 > $3/refs/heads/main 2>/dev/null && echo MAIN_MOVED
        echo 'gitdir: /tmp' > .git 2>/dev/null && echo GIT_LINK_WRITTEN
        cat ~/.ssh/id_test ~/.git-credentials $4/.env ~/.cache/legionctl/answers 2>/dev/null | grep -q SECRET && echo SECRET_READ
        rm -f $XDG_RUNTIME_DIR/legionctl/api.sock 2>/dev/null
        [ -S $XDG_RUNTIME_DIR/legionctl/api.sock ] && echo API_SOCKET_KEPT
        echo "SESSION_FOLDERS=$(ls $XDG_RUNTIME_DIR/legionctl/sessions | wc -l)"
        legionctl dossier active 2>&1 | grep -q 'active dossier escape' && echo OPERATOR_REACHED
"#;

/// The agent config and caches, keys and a token of a home, and the repository's .env, as on this machine.
fn home_with_secrets(home: &Path, repo: &Path) {
    write(&home.join(".claude/settings.json"), "{}");
    write(&home.join(".cache/sccache/kept"), "cache");
    write(&home.join(".ssh/id_test"), "SECRET ssh key");
    write(
        &home.join(".git-credentials"),
        "https://user:SECRET@github.com",
    );
    write(&repo.join(".env"), "OPENAI_API_KEY=SECRET");
    // beside cache.db: the gateway's and MCP servers' answers to other sessions
    write(&home.join(".cache/legionctl/answers"), "SECRET answer");
}

/// The probe agent of `a_sandboxed_agent_finds_no_way_out_and_still_commits_calls_the_model_and_the_operator`, and the
/// gateway to the stand-in provider.
fn escape_config(home: &Path, repo: &Path, (gateway, provider): (u16, u16), service: u16) {
    let common = home.join("repo/.git");
    write(
        &home.join(".config/legionctl/session.jsonc"),
        &json!({
            "session": {
                "agents": {"probe": ["bash", "-c", ESCAPE_PROBE, "probe", std::process::id().to_string(),
                                     service.to_string(), common, repo]},
                "scope": false
            },
            "gateway": {
                "port": gateway,
                "accounts": {"main": {"provider": "anthropic", "base_url": format!("http://127.0.0.1:{provider}/ok")}},
                "pool": {"*": ["main"]}
            }
        })
        .to_string(),
    );
}

/// A hostile agent in a host session tries each way out that the security review found (EV-review-08: S-1 host
/// processes, S-2 the operator's socket, S-3 the real agent config and caches, S-4 the repository's git config,
/// hooks, other branches and git links, S-5 a service on the host's loopback, S-6 keys and tokens of the home and the
/// repository's .env, B-2 legionctl's cache under the cache overlay) and each fails, while the work still works: a commit through the git gate, a model call
/// through the gateway (the only network), and calls to the operator. The writes to the agents' own folders stay in
/// the session's layer; the real files keep their content.
#[test]
fn a_sandboxed_agent_finds_no_way_out_and_still_commits_calls_the_model_and_the_operator() {
    let home = home("breakout");
    let (_provider, (gateway, provider)) = gateway_ports(&home);
    let repo = repository(&home);
    home_with_secrets(&home, &repo);
    // a service on the host's loopback, like the sccache server
    let service = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    escape_config(
        &home,
        &repo,
        (gateway, provider),
        service.local_addr().unwrap().port(),
    );
    let op = operator_in("breakout", &home);
    let main_before = git(&home, &repo, &["rev-parse", "main"]);
    let started = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &["session", "start", "probe", "--dossier", "escape", "--json"],
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
    let why = format!("{text}\n{}", String::from_utf8_lossy(&out.stderr));
    for works in [
        "COMMIT_OK",
        "GATEWAY_200",
        "CLAUDE_WRITE_KEPT",
        "CACHE_WRITE_KEPT",
        "API_SOCKET_KEPT",
        "SESSION_FOLDERS=1",
        "OPERATOR_REACHED",
    ] {
        assert!(text.contains(works), "{works} missing: {why}");
    }
    for escape in [
        "HOST_PID_SEEN",
        "PROC_ROOT_WRITTEN",
        "LOOPBACK_REACHED",
        "GIT_CONFIG_WRITTEN",
        "GIT_HOOK_WRITTEN",
        "MAIN_MOVED",
        "GIT_LINK_WRITTEN",
        "SECRET_READ",
    ] {
        assert!(!text.contains(escape), "{escape}: {why}");
    }
    assert_eq!(
        sandbox_calls(&home),
        1,
        "the model call went through the gateway"
    );
    assert!(!home.join("pwned").exists());
    assert_eq!(git(&home, &repo, &["rev-parse", "main"]), main_before);
    assert_eq!(
        std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(),
        "{}",
        "the real Claude Code settings keep their content"
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".cache/sccache/kept")).unwrap(),
        "cache"
    );
    let layer = home
        .join(".local/state/legionctl/session-home")
        .join(launch["id"].as_str().unwrap())
        .join("home")
        .join(home.strip_prefix("/").unwrap());
    assert!(
        std::fs::read_to_string(layer.join(".claude/upper/settings.json"))
            .unwrap()
            .contains("planted"),
        "the agent's write stays in its own layer"
    );
    assert!(
        launch["lines"].as_array().unwrap().iter().any(|l| l
            .as_str()
            .unwrap()
            .starts_with("network: the model gateway and the granted connections only")),
        "{launch}"
    );
    assert_eq!(
        git(
            &home,
            &home.join("worktrees/repo/escape"),
            &["log", "-1", "--format=%s"]
        ),
        "probe"
    );
}

/// A session writes only its own branch (EV-review-16 C7): in its sandbox it commits, stashes and pops (the stash's
/// own `git reset --hard` runs; the agent's own one stays refused), but the other
/// branches under legionctl/ (a loose one and a packed one, which a loose file of the agent would hide) and a
/// replacement ref (which would change what the host's git shows) stay as they were, also when the agent writes the
/// ref files itself with the real git or with its shell.
#[test]
fn a_session_writes_only_its_own_branch() {
    let (op, home) = operator("ownref");
    let repo = repository(&home);
    for branch in ["legionctl/packed", "legionctl/loose"] {
        git(&home, &repo, &["branch", branch]);
    }
    git(&home, &repo, &["pack-refs", "--all"]);
    git(
        &home,
        &repo,
        &["update-ref", "refs/heads/legionctl/loose", "HEAD"],
    );
    let start = git(&home, &repo, &["rev-parse", "HEAD"]);
    let probe = r#"common=$(git rev-parse --path-format=absolute --git-common-dir)
        echo x > note.txt && git add note.txt && git commit -qm own && echo COMMIT_OK
        echo y > note.txt && git stash -q && git stash pop -q && grep -qx y note.txt && echo STASH_OK
        head=$(git rev-parse HEAD)
        for b in loose packed; do
            echo "$head" > "$common/refs/heads/legionctl/$b" 2>/dev/null && echo "WROTE_$b"
            "$LEGIONCTL_REAL_GIT" update-ref "refs/heads/legionctl/$b" "$head" 2>/dev/null && echo "MOVED_$b"
            "$LEGIONCTL_REAL_GIT" update-ref -d "refs/heads/legionctl/$b" 2>/dev/null && echo "DELETED_$b"
        done
        mkdir -p "$common/refs/replace" && echo "$head" > "$common/refs/replace/$head" 2>/dev/null && echo REPLACED
        true"#;
    write(
        &home.join(".config/legionctl/session.jsonc"),
        &json!({"session": {"agents": {"probe": ["sh", "-c", probe]}, "scope": false, "writable": []}})
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
    assert_eq!(
        text.lines().collect::<Vec<_>>(),
        ["COMMIT_OK", "STASH_OK"],
        "{text}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for branch in ["legionctl/loose", "legionctl/packed"] {
        assert_eq!(git(&home, &repo, &["rev-parse", branch]), start, "{branch}");
    }
    assert_eq!(
        git(
            &home,
            &repo,
            &["log", "-1", "--format=%s", "legionctl/port"]
        ),
        "own"
    );
    assert_eq!(git(&home, &repo, &["replace", "--list"]), "");
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
                 touch agentWorkspaces/port/MEMORY/probe.md && echo DOSSIER_WRITABLE; echo \"SESSION=$LEGIONCTL_SESSION\"; \
                 touch agentWorkspaces/port/.state/dossier.db 2>/dev/null || echo EVIDENCE_READONLY; \
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
            // the test runs the sandbox with no scope
            "scope": false,
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
        "EVIDENCE_READONLY",
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

/// What the agent of `a_sessions_repository_commands_run_in_its_sandbox_never_on_the_host` copies into `workspace`:
/// its escape script, a checks file and a hooks file (both allowed by the user, O-2) and a prompt hook's input.
fn jail_files(home: &Path, workspace: &Path) {
    let h = home.display();
    let escape = format!(
        "echo ran-$1 >> agentWorkspaces/jail/ran.log\n\
         touch {h}/pwned-$1 2>/dev/null\n\
         grep -q SECRET {h}/.ssh/id_test 2>/dev/null && touch agentWorkspaces/jail/leaked-$1\n\
         [ -d /proc/{pid} ] && touch agentWorkspaces/jail/host-pid-$1\n\
         echo HOOK_DONE\n\
         exit 0\n",
        pid = std::process::id()
    );
    let checks = json!({"checks": {
        "escape": {"kind": "lint", "run": "sh ./escape.sh check"},
        "suite": {"kind": "test", "run": "sh ./escape.sh test"}
    }});
    let hooks = json!({"hooks": {
        "actions": {"probe": {"command": "sh ./escape.sh hook", "timeout": 10}},
        "bindings": {"prompt": ["probe"]}
    }});
    // the user allowed these contents of the repository's hooks and checks files (O-2, A-5), in its main checkout,
    // which holds for each worktree; the agent cannot write the answers
    let main = home.join("repo/.legionctl");
    trust::allow_value(home, &main.join("hooks.jsonc"), &hooks);
    trust::allow_value(home, &main.join("checks.jsonc"), &checks);
    write(&workspace.with_file_name("escape.sh"), &escape);
    write(
        &workspace.with_file_name("checks.jsonc"),
        &checks.to_string(),
    );
    write(&workspace.with_file_name("hooks.jsonc"), &hooks.to_string());
    write(
        &workspace.with_file_name("prompt.json"),
        &claude_hook("prompt", &json!({"cwd": workspace})),
    );
}

/// DESIGN-12 wall 1: a harness that refreshes its login in a session rotates the refresh token, so the new login
/// must reach the real file, or the user's own Claude Code or Codex keeps a spent token and is logged out. A stand-in
/// harness rewrites both login files and a settings file: the logins are new in the real home after the session, the
/// settings write stays in the session's layer. Claude Code's login is clauth's link to its active profile's file
/// (bwrap refused to mount on it, r324): the refresh reaches the profile's file and the link stays clauth's.
#[test]
fn a_login_refreshed_in_a_session_reaches_the_real_login_files() {
    let (op, home) = operator("logins");
    let repo = repository(&home);
    let alt = home.join(".clauth/profiles/alt/credentials.json");
    write(&alt, "{\"refresh\": \"old\"}");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::os::unix::fs::symlink(&alt, home.join(".claude/.credentials.json")).unwrap();
    write(&home.join(".codex/auth.json"), "{\"refresh\": \"old\"}");
    write(&home.join(".claude/settings.json"), "{}");
    let refresh = "echo '{\"refresh\": \"new\"}' > ~/.claude/.credentials.json && \
                   echo '{\"refresh\": \"new\"}' > ~/.codex/auth.json && \
                   echo '{\"hooks\": \"planted\"}' > ~/.claude/settings.json && echo REFRESHED";
    write(
        &home.join(".config/legionctl/session.jsonc"),
        &json!({"session": {"agents": {"refresh": ["sh", "-c", refresh]}, "scope": false}})
            .to_string(),
    );
    let started = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &[
            "session",
            "start",
            "refresh",
            "--dossier",
            "logins",
            "--json",
        ],
        "",
    );
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
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("REFRESHED"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    for login in [".clauth/profiles/alt/credentials.json", ".codex/auth.json"] {
        assert_eq!(
            std::fs::read_to_string(home.join(login)).unwrap().trim(),
            "{\"refresh\": \"new\"}",
            "{login}: the refreshed login is the user's"
        );
    }
    assert_eq!(
        std::fs::read_link(home.join(".claude/.credentials.json")).unwrap(),
        alt,
        "the live login stays clauth's link"
    );
    assert_eq!(
        std::fs::read_to_string(home.join(".claude/settings.json")).unwrap(),
        "{}",
        "the other files stay in the session's layer"
    );
}

/// O-1 (EV-review-08): the commands of a repository's config that legionctl runs for a sandboxed agent run in that
/// session's sandbox, never on the host: its checks (`code checks run`), its test runs (`code tests run`), the user's
/// commit hook at `workspace checkpoint` (which runs a script of the working tree, as pre-commit does), a command
/// hook of the repository layer (allowed by the user, O-2) and its mutants (`code tests mutant run`, which builds and
/// runs the agent's code: A-2, a stand-in cargo-mutants here). The agent writes each of them to escape: write the real
/// home, read a key, see a host process. Each runs (its line in the dossier), none escapes, and the commit lands.
#[test]
fn a_sessions_repository_commands_run_in_its_sandbox_never_on_the_host() {
    let (op, home) = operator("jail");
    let repo = repository(&home);
    write(&home.join(".ssh/id_test"), "SECRET ssh key");
    let hook = repo.join(".git/hooks/pre-commit");
    write(&hook, "#!/bin/sh\nexec sh ./commit-check.sh\n");
    std::fs::set_permissions(&hook, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    let workspace = home.join("worktrees/repo/jail");
    jail_files(&home, &workspace);
    write(
        &home.join("keyring-bin/cargo-mutants"),
        "#!/bin/sh\nexec sh ./escape.sh mutant\n",
    );
    std::fs::set_permissions(
        home.join("keyring-bin/cargo-mutants"),
        std::os::unix::fs::PermissionsExt::from_mode(0o755),
    )
    .unwrap();
    let agent = "mkdir -p .legionctl && cp ../escape.sh . && cp ../checks.jsonc ../hooks.jsonc .legionctl/ && \
                 echo 'sh ./escape.sh commit' > commit-check.sh && echo 'pub fn x() {}' > lib.rs && \
                 legionctl code checks run escape && legionctl code tests run && \
                 legionctl workspace checkpoint probe && legionctl hook prompt < ../prompt.json; \
                 legionctl code tests mutant run";
    write(
        &home.join(".config/legionctl/session.jsonc"),
        &json!({"session": {"agents": {"probe": ["sh", "-c", agent]}, "writable": [], "scope": false}})
            .to_string(),
    );
    let started = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &["session", "start", "probe", "--dossier", "jail", "--json"],
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
    let why = format!("{text}\n{}", String::from_utf8_lossy(&out.stderr));
    assert!(text.contains("escape: exit 0"), "{why}");
    assert!(
        text.contains("HOOK_DONE"),
        "the repository's hook ran: {why}"
    );
    assert_eq!(
        git(&home, &workspace, &["log", "-1", "--format=%s"]),
        "probe",
        "{why}"
    );
    let dossier = home.join("agentWorkspaces/jail");
    assert_eq!(
        std::fs::read_to_string(dossier.join("ran.log")).unwrap_or_default(),
        "ran-check\nran-test\nran-commit\nran-hook\nran-mutant\n",
        "{why}"
    );
    for what in ["check", "test", "commit", "hook", "mutant"] {
        assert!(
            !home.join(format!("pwned-{what}")).exists(),
            "{what} wrote the home: {why}"
        );
        assert!(
            !dossier.join(format!("leaked-{what}")).exists(),
            "{what} read the key: {why}"
        );
        assert!(
            !dossier.join(format!("host-pid-{what}")).exists(),
            "{what} saw the host: {why}"
        );
    }
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

/// A session on a clauth account (user decision 2026-10-17: clauth is the only store of the logins): in a Scratch HOME
/// laid out as clauth 0.17.0 keeps it (main the current login through clauth's link, work and spare other profiles,
/// cx with a Codex login only), `session start claude --account work` runs on work's own login file, its refresh
/// goes into work's file (no copy, the link and main's login stay), and clauth's store is hidden in the sandbox;
/// `session resume --account spare` goes on with the same transcript on spare's login and keeps the account. An
/// account that clauth does not have, and one with no Claude Code login, are refused in plain words.
#[test]
fn a_session_on_a_clauth_account_runs_on_its_login_and_resumes_on_another() {
    let (op, home) = operator("clauth");
    let repo = repository(&home);
    // the sessions' scopes are gone when a later start asks (no user manager in a test: systemd's answer stands in)
    stand_in(&home, "systemctl", "#!/bin/sh\necho inactive\nexit 3\n");
    let env = [("HOME", home.to_str().unwrap())];
    let cli = |args: &[&str]| op.run(&repo, &env, args, "");
    let profile = |name: &str| home.join(format!(".clauth/profiles/{name}/credentials.json"));
    write(
        &home.join(".clauth/profiles.toml"),
        "active_profile = \"main\"\nprofiles = [\"main\", \"work\", \"spare\"]\n",
    );
    write(
        &home.join(".clauth/codex-profiles.toml"),
        "profiles = [\"cx\"]\n",
    );
    write(&home.join(".clauth/profiles/cx/auth.json"), "codex-cx");
    for name in ["main", "work", "spare"] {
        write(&profile(name), &format!("{name}-1"));
    }
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::os::unix::fs::symlink(profile("main"), home.join(".claude/.credentials.json")).unwrap();
    // the agent says which login it got and "refreshes" it, appends a turn and tries clauth's store; the resumed one
    // reads its transcript back
    let login = "~/.claude/.credentials.json";
    let transcript = "~/.claude/projects/p/$LEGIONCTL_SESSION.jsonl";
    let start = format!(
        "echo \"LOGIN=$(cat {login})\"; printf 'r%s' \"$(cat {login})\" > {login}; mkdir -p ~/.claude/projects/p && \
         echo turn-1 >> {transcript}; cat ~/.clauth/profiles/spare/credentials.json || echo HIDDEN"
    );
    let resume = format!("cat {transcript}; echo \"LOGIN=$(cat {login})\"");
    write(
        &home.join(".config/legionctl/session.jsonc"),
        &json!({"session": {"agents": {"claude": ["sh", "-c", start]}, "resume": {"claude": ["sh", "-c", resume]}}})
            .to_string(),
    );
    let session = |account: &str| {
        cli(&[
            "session",
            "start",
            "claude",
            "--dossier",
            "clauth",
            "--account",
            account,
            "--json",
        ])
    };
    for (account, error) in [
        (
            "ghost",
            "no account ghost; the accounts are: main, work, spare, cx",
        ),
        ("cx", "account cx has no Claude Code login"),
    ] {
        let refused = session(account);
        assert!(
            !refused.status.success() && String::from_utf8_lossy(&refused.stderr).contains(error),
            "{account}: {}",
            String::from_utf8_lossy(&refused.stderr)
        );
    }
    let started = session("work");
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    let launch: Value = serde_json::from_slice(&started.stdout).unwrap();
    let out = String::from_utf8_lossy(&run_launch(&op, &home, &launch).stdout).into_owned();
    assert!(
        out.contains("LOGIN=work-1") && out.contains("HIDDEN") && !out.contains("spare-1"),
        "{out}"
    );
    let read = |name: &str| std::fs::read_to_string(profile(name)).unwrap();
    assert_eq!(
        [read("work"), read("main")],
        ["rwork-1", "main-1"],
        "the refresh is work's own"
    );
    assert_eq!(
        std::fs::read_link(home.join(".claude/.credentials.json")).unwrap(),
        profile("main"),
        "the current login stays clauth's link"
    );

    resumes_on_spare(&op, &home, &repo, launch["id"].as_str().unwrap());
}

/// `session resume --account` of the test above: an account with no Claude Code login is refused; on spare the same
/// transcript goes on with spare's login, and the session keeps the account.
fn resumes_on_spare(op: &Operator, home: &Path, repo: &Path, id: &str) {
    let cli = |args: &[&str]| op.run(repo, &[("HOME", home.to_str().unwrap())], args, "");
    let refused = cli(&["session", "resume", id, "--account", "cx"]);
    assert!(
        String::from_utf8_lossy(&refused.stderr).contains("account cx has no Claude Code login"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
    let resumed = cli(&["session", "resume", id, "--account", "spare", "--json"]);
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let out = run_launch(op, home, &serde_json::from_slice(&resumed.stdout).unwrap());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "turn-1\nLOGIN=spare-1\n",
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let sessions: Value =
        serde_json::from_slice(&cli(&["session", "list", "--json"]).stdout).unwrap();
    assert_eq!(sessions[0]["account"], "spare", "{sessions}");
}

/// Run QEMU on `disk` with the read-only `iso` until the guest powers off (it may restart before); the serial console
/// goes to `log`. `results` adds a raw results disk, `table` an ACPI table.
fn boot_until_poweroff(
    disk: &Path,
    iso: &Path,
    results: Option<&Path>,
    table: Option<&Path>,
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
    if let Some(table) = table {
        qemu.arg("-acpitable")
            .arg(format!("file={}", table.display()));
    }
    if let Some(results) = results {
        qemu.arg("-drive")
            .arg(format!(
                "file={},if=none,id=results,format=raw",
                results.display()
            ))
            .args(["-device", "virtio-blk-pci,drive=results,serial=lwmiresults"]);
    }
    let mut child = Kill::spawn(&mut qemu);
    let deadline = std::time::Instant::now() + std::time::Duration::from_mins(minutes);
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if std::time::Instant::now() > deadline {
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

/// The disk of the VM tests (legionctl-wmi, the daemon's unit), made once over the cloud image and kept in the test
/// cache: Debian's generic kernel from trixie-backports (the cloud kernel has no WMI, and the module needs the WMI API
/// of 7.x) with its headers, DKMS, python3, polkit, sfdisk, mkfs.vfat and the unit that runs the test from the disk
/// labelled LWMITEST, then powers off.
fn vm_test_base(image: &Path) -> PathBuf {
    // the VM tests of this binary run in parallel: one makes the disk, the others wait for it (two at once fight
    // over the write lock of the part file, CI run 38737131349)
    static MAKING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _making = MAKING
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    // a new name when the preparation changes
    let base = image.with_file_name("legionctl-vm-base-v2.qcow2");
    if base.exists() {
        return base;
    }
    let dir = Scratch::new(format!("/var/tmp/lc-kmod-base-{}", std::process::id()));
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
         apt-get install -y dkms python3 polkitd fdisk dosfstools; \
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
    boot_until_poweroff(&part, &seed_iso, None, None, &log, 25);
    let console = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        console.contains("lwmi-base-ok"),
        "the base disk was not prepared; console:\n{}",
        &console[console.len().saturating_sub(4000)..]
    );
    std::fs::rename(&part, &base).unwrap();
    base
}

/// The checks of tests/fixtures/legionctl-wmi/run.sh, in their order: each must report ok.
const LEGIONCTL_WMI_CHECKS: [&str; 48] = [
    "dkms-build",
    "no-autoload",
    "explicit-load",
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
/// kernel and bound to a stand-in of the 82WQ firmware (tests/fixtures/legionctl-wmi/ssdt.asl, a QEMU -acpitable) after a
/// restart with the package's blacklist in place. The guest (run.sh) uses it through sysfs only: `platform_profile`
/// and its Fn+Q notice, the firmware attributes within and outside their bounds and modes, the fan table byte for
/// byte as the firmware gets it, its refusals and its return after a mode change, Fn+Q and resume, full speed, fan
/// max mode, the mute LEDs and a clean unload.
#[test]
#[ignore = "boots VMs (KVM, QEMU, a cloud image) and builds a kernel module: CI runs it in the VM step"]
fn the_legionctl_wmi_module_gives_the_legion_wmi_through_sysfs() {
    let base = vm_test_base(&debian_cloud_image());
    let dir = Scratch::new(format!("/var/tmp/lc-kmod-{}", std::process::id()));
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
    for name in ["legionctl-wmi.conf", "no-autoload.conf"] {
        std::fs::copy(kmod.join(name), test.join(name)).unwrap();
    }
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/legionctl-wmi");
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
        Some(&results),
        Some(&dir.join("ssdt.aml")),
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
}

/// The checks of tests/fixtures/daemon-sandbox/run.sh, in their order: each must report ok.
const DAEMON_SANDBOX_CHECKS: [&str; 24] = [
    "unit-active",
    "esp-after-start",
    "usr-read-only",
    "keys-root-only",
    "boot-dry-run-user",
    "greeter-install",
    "greeter-config",
    "greeter-display-manager",
    "greeter-unit",
    "greeter-modules",
    "greeter-splash-quit",
    "greeter-other-unit-refused",
    "boot-install",
    "boot-loader",
    "boot-image",
    "boot-line",
    "boot-loader-conf",
    "boot-entry",
    "esp-automount-idle",
    "boot-rebuild-automount",
    "boot-image-automount",
    "autostart-rule",
    "autostart-rule-file",
    "primary-gpu-rule",
];

/// The daemon's sandbox (docs/design/13-daemon.md, Sandbox): `legionctl-system` under the package's own unit
/// (`ProtectSystem=strict`, its `ReadWritePaths`) in a Debian 13 VM, and root's calls over the system bus through
/// polkit. Greeter install writes `/etc/greetd` and changes the display manager; boot install writes the ESP (a vfat
/// partition mounted at `/boot/efi` after the daemon started) and `/etc/legionctl`, and boot rebuild writes it when
/// it is an automount that nothing opened yet (the daemon's open mounts it); and its signers and `efibootmgr` run as `legionctl-step-*`
/// transient units; the Windows VM autostart writes `/etc/polkit-1/rules.d`, the desktop GPU `/etc/udev/rules.d`; the
/// daemon cannot write `/usr`. A path that the unit does not allow fails its check. A user without a session runs
/// `boot install --dry-run` through the user's own operator while the MOK key folder is root's alone (todo-128): the
/// daemon's plan lists the steps, without polkit, and nothing changes.
#[test]
#[ignore = "boots a VM (KVM, QEMU, a cloud image) and runs the daemon under systemd: CI runs it in the VM step"]
fn under_its_own_unit_the_daemon_writes_the_greeter_the_esp_and_the_rules() {
    let base = vm_test_base(&debian_cloud_image());
    let dir = Scratch::new(format!("/var/tmp/lc-sandbox-{}", std::process::id()));
    let test = dir.join("test");
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let profile = "lenovo-82wq-legion-pro-7-16irx8h";
    std::fs::create_dir_all(test.join("profiles")).unwrap();
    for (from, to) in [
        (
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../tests/fixtures/daemon-sandbox/run.sh"),
            "run.sh",
        ),
        (
            Path::new(env!("CARGO_BIN_EXE_legionctl")).with_file_name("legionctl-system"),
            "legionctl-system",
        ),
        (
            Path::new(env!("CARGO_BIN_EXE_legionctl")).with_file_name("legionctl-operator"),
            "legionctl-operator",
        ),
        (PathBuf::from(env!("CARGO_BIN_EXE_legionctl")), "legionctl"),
        (
            repo.join("systemd/legionctl-system.service"),
            "legionctl-system.service",
        ),
        (
            repo.join("dbus/org.legionctl.System1.conf"),
            "org.legionctl.System1.conf",
        ),
        (
            repo.join("polkit/org.legionctl.policy"),
            "org.legionctl.policy",
        ),
        (
            repo.join("polkit/50-legionctl-greeter.rules"),
            "50-legionctl-greeter.rules",
        ),
        (
            repo.join("systemd/legionctl-splash-quit.service"),
            "legionctl-splash-quit.service",
        ),
        (
            repo.join("packaging/greeter/greetd.service"),
            "greetd.service",
        ),
        (
            repo.join("packaging/greeter/nvidia-modules.conf"),
            "nvidia-modules.conf",
        ),
        (
            repo.join(format!("profiles/{profile}.json")),
            &format!("profiles/{profile}.json"),
        ),
    ] {
        std::fs::copy(&from, test.join(to)).unwrap_or_else(|e| panic!("{}: {e}", from.display()));
    }
    let layers = test.join("configsets").join(profile);
    std::fs::create_dir_all(&layers).unwrap();
    for entry in std::fs::read_dir(repo.join("configsets").join(profile))
        .unwrap()
        .flatten()
    {
        std::fs::copy(entry.path(), layers.join(entry.file_name())).unwrap();
    }
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
    boot_until_poweroff(&disk, &test_iso, Some(&results), None, &log, 10);

    let report = String::from_utf8_lossy(&std::fs::read(&results).unwrap())
        .trim_end_matches('\0')
        .to_string();
    let passed: Vec<&str> = report
        .lines()
        .filter_map(|l| l.strip_prefix("ok "))
        .collect();
    let console = std::fs::read_to_string(&log).unwrap_or_default();
    assert!(
        passed == DAEMON_SANDBOX_CHECKS && report.contains("\ndone"),
        "results:\n{report}\nconsole:\n{}",
        &console[console.len().saturating_sub(3000)..]
    );
}

/// todo-118: the folders of a test (the operator's runtime folder and the HOME it holds) go when their owner is
/// dropped and when the test panics; a failed test keeps them only with `LEGIONCTL_TEST_KEEP=1`, and says where.
/// The test runs its own body in two child processes of this binary (without and with the variable), so the
/// outer environment does not change the result.
#[test]
fn test_folders_go_with_their_owner_and_a_failed_test_keeps_them_only_when_asked() {
    const NAME: &str =
        "test_folders_go_with_their_owner_and_a_failed_test_keeps_them_only_when_asked";
    if std::env::var_os("LEGIONCTL_TEST_CHILD").is_some() {
        let folders = |op: &Operator, home: PathBuf| [op.runtime.to_path_buf(), home];
        let (op, home) = operator("dropped");
        let made = folders(&op, home);
        assert!(made.iter().all(|f| f.is_dir()), "{made:?}");
        drop(op);
        assert!(
            made.iter().all(|f| !f.exists()),
            "left after drop: {made:?}"
        );
        let mut made = Vec::new();
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (op, home) = operator("panicked");
            made.extend(folders(&op, home));
            panic!("the test failed");
        }));
        assert!(failed.is_err());
        assert!(
            made.iter().all(|f| !f.exists()),
            "left after a panic: {made:?}"
        );
        return;
    }
    let child = |keep: bool| {
        let mut c = Command::new(std::env::current_exe().unwrap());
        c.args([NAME, "--exact", "--nocapture", "--test-threads", "1"])
            .env("LEGIONCTL_TEST_CHILD", "1")
            .env_remove("LEGIONCTL_TEST_KEEP");
        if keep {
            c.env("LEGIONCTL_TEST_KEEP", "1");
        }
        let out = c.output().unwrap();
        let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
        (out.status.success(), stderr)
    };
    let (passed, stderr) = child(false);
    assert!(passed, "{stderr}");
    let (passed, stderr) = child(true);
    let kept: Vec<PathBuf> = stderr
        .lines()
        .filter_map(|l| l.strip_prefix("LEGIONCTL_TEST_KEEP=1: kept "))
        .map(PathBuf::from)
        .collect();
    for f in &kept {
        assert!(f.is_dir(), "{f:?} is said kept but is not there");
        std::fs::remove_dir_all(f).unwrap();
    }
    assert!(
        !passed,
        "with LEGIONCTL_TEST_KEEP=1 the panic must keep the folders: {stderr}"
    );
    let names: Vec<String> = kept
        .iter()
        .map(|f| f.file_name().unwrap().to_string_lossy().into_owned())
        .map(|n| n[..n.rfind('-').unwrap()].to_string())
        .collect();
    assert_eq!(
        names,
        ["legionctl-e2e-panicked", "lc-panicked"],
        "only the folders of the failed part are kept, the HOME then the runtime: {stderr}"
    );
}

/// A process that ended: its `/proc` entry is gone, or it is a zombie that its new parent did not reap yet.
fn ended(pid: u32) -> bool {
    std::fs::read_to_string(format!("/proc/{pid}/stat")).map_or(true, |stat| {
        stat.rsplit_once(") ")
            .is_some_and(|(_, rest)| rest.starts_with('Z'))
    })
}

/// Waits up to 10 s for each process of `pids` to end.
fn wait_ended(pids: &[u32], what: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !pids.iter().all(|&pid| ended(pid)) {
        assert!(
            std::time::Instant::now() < deadline,
            "{what}: still running {:?}",
            pids.iter().filter(|&&pid| !ended(pid)).collect::<Vec<_>>()
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

/// todo-123: the processes of a test end with it (63 stayed after hung and killed runs, one with the user's real
/// HOME). When the test panics, the guards kill their process groups: the real operator, a stand-in and the process
/// that the stand-in started. When the test binary is killed with SIGKILL (no drop runs), the real operator gets the
/// parent-death signal. The test runs its body in a child process of this binary, which it kills.
#[test]
fn the_processes_of_a_test_end_when_it_panics_and_when_its_binary_is_killed() {
    const NAME: &str = "the_processes_of_a_test_end_when_it_panics_and_when_its_binary_is_killed";
    if std::env::var_os("LEGIONCTL_TEST_CHILD").is_some() {
        let mut pids = Vec::new();
        let failed = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let (op, _home) = operator("procs-panic");
            op.ok(&["events"]);
            let mut tree = Kill::spawn(
                Command::new("sh")
                    .args(["-c", "sleep 600 & echo $!; wait"])
                    .stdout(Stdio::piped()),
            );
            let mut started = String::new();
            BufReader::new(tree.stdout.take().unwrap())
                .read_line(&mut started)
                .unwrap();
            pids.extend([op.child.id(), tree.id(), started.trim().parse().unwrap()]);
            panic!("the test failed");
        }));
        assert!(failed.is_err());
        // on a line of its own: libtest wrote "test <name> ... " before it
        println!("\nafter the panic: {pids:?}");
        let (op, home) = operator("procs-kill");
        op.ok(&["events"]);
        // the folders stay when the binary is killed: the outer test removes them
        println!(
            "\nrunning: {} {} {}",
            op.child.id(),
            home.display(),
            op.runtime.display()
        );
        // the outer test kills this binary before then
        std::thread::sleep(std::time::Duration::from_secs(60));
        return;
    }
    let mut test = Kill::spawn(
        Command::new(std::env::current_exe().unwrap())
            .args([NAME, "--exact", "--nocapture", "--test-threads", "1"])
            .env("LEGIONCTL_TEST_CHILD", "1")
            .stdout(Stdio::piped()),
    );
    let mut lines = BufReader::new(test.stdout.take().unwrap()).lines();
    let mut line = |prefix: &str| -> String {
        lines
            .by_ref()
            .map(Result::unwrap)
            .find_map(|l| l.strip_prefix(prefix).map(str::to_string))
            .unwrap_or_else(|| panic!("the test binary ended before {prefix}"))
    };
    let panicked: Vec<u32> = serde_json::from_str(&line("after the panic: ")).unwrap();
    assert_eq!(panicked.len(), 3);
    wait_ended(&panicked, "after the panic");
    let running = line("running: ");
    let running: Vec<&str> = running.split(' ').collect();
    let operator: u32 = running[0].parse().unwrap();
    assert!(!ended(operator), "the operator of the second part runs");
    // SIGKILL to the test binary alone: its process group is not killed, and no drop runs in it
    test.kill().unwrap();
    test.wait().unwrap();
    wait_ended(&[operator], "after SIGKILL of the test binary");
    for folder in &running[1..] {
        std::fs::remove_dir_all(folder).unwrap();
    }
}

/// todo-122, as on 2026-10-15: the user's operator.db at v10 while an operator of another build runs (a stand-in that
/// holds `operator.lock` with that build's fingerprint). An operator of this build does not start, and the file stays
/// at v10 without a backup. After the other operator stopped, this one starts: it keeps the v10 file in
/// `operator.db.v10.bak`, then takes the database to v17 with its rows (the MCP tool call log into the call record, which
/// does not say their folder).
#[test]
fn an_operator_does_not_start_beside_one_of_another_build_and_keeps_the_old_database() {
    let home = home("holder");
    let state = home.join(".local/state/legionctl");
    std::fs::create_dir_all(&state).unwrap();
    let db = state.join("operator.db");
    let bak = state.join("operator.db.v10.bak");
    // the tables of v10 (the release before the threads)
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE events (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, actor TEXT NOT NULL,
                kind TEXT NOT NULL, payload TEXT NOT NULL);
             CREATE INDEX events_kind ON events (kind, id);
             CREATE TABLE gateway_usage (id INTEGER PRIMARY KEY, ts INTEGER NOT NULL, account TEXT NOT NULL,
                model TEXT NOT NULL, tokens_in INTEGER NOT NULL, tokens_out INTEGER NOT NULL, cached INTEGER NOT NULL,
                status INTEGER NOT NULL, ms INTEGER NOT NULL, cost_usd REAL NOT NULL DEFAULT 0);
             CREATE TABLE tokens (hash TEXT PRIMARY KEY, session TEXT NOT NULL, created INTEGER NOT NULL);
             CREATE TABLE jobs (id INTEGER PRIMARY KEY, kind TEXT NOT NULL, input TEXT NOT NULL, state TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 1, created INTEGER NOT NULL, updated INTEGER NOT NULL, result TEXT,
                error TEXT);
             INSERT INTO events (ts, actor, kind, payload) VALUES (strftime('%s'), 'cli', 'kept.v10', '{}');
             INSERT INTO events (ts, actor, kind, payload) VALUES (strftime('%s'), 'mcp-proxy', 'internal.tools.record',
                '{\"input\": {\"session\": \"s9\", \"server\": \"probe\", \"tool\": \"load\", \"args_hash\": \"ab\",
                  \"ms\": 12, \"bytes\": 34, \"outcome\": \"ok\"}}');
             PRAGMA user_version = 10;",
        )
        .unwrap();
    let version = |path: &Path| -> i64 {
        rusqlite::Connection::open(path)
            .unwrap()
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap()
    };
    let lock = state.join("operator.lock");
    let other = "operator.db v10, dossier.db v1, cache.db v1, workspace.db v14";
    std::fs::write(&lock, other).unwrap();
    let holder = Kill::spawn(
        Command::new("flock")
            .arg("--shared")
            .arg(&lock)
            .args(["sleep", "600"]),
    );
    let probe = std::fs::File::open(&lock).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while rustix::fs::flock(&probe, rustix::fs::FlockOperation::NonBlockingLockExclusive).is_ok() {
        rustix::fs::flock(&probe, rustix::fs::FlockOperation::Unlock).unwrap();
        assert!(
            std::time::Instant::now() < deadline,
            "the stand-in holds no lock"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    drop(probe);
    let refused = Command::new(common::sibling("legionctl-operator"))
        .env_clear()
        .env("HOME", &*home)
        .env("PATH", std::env::var_os("PATH").unwrap())
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&refused.stderr);
    assert!(!refused.status.success(), "{stderr}");
    assert!(
        stderr.contains(&format!(
            "operator ({other}) of another build holds {}; stop it or install this build",
            db.display()
        )),
        "{stderr}"
    );
    assert_eq!(version(&db), 10, "the refused operator changed nothing");
    assert!(!bak.exists());
    drop(holder);
    let op = operator_in("holder", &home);
    let events = op.ok(&["events", "--json"]);
    assert!(events.contains("kept.v10"), "{events}");
    assert_eq!(version(&db), 17);
    assert_eq!(version(&bak), 10, "the backup is the v10 file");
    the_old_tool_log_is_in_the_call_record(&op);
    let kept: String = rusqlite::Connection::open(&bak)
        .unwrap()
        .query_row("SELECT kind FROM events ORDER BY id LIMIT 1", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(kept, "kept.v10");
}

/// The old MCP tool call log (an `internal.tools.record` event of v10) is in the call record (DESIGN/22 rule 3).
fn the_old_tool_log_is_in_the_call_record(op: &Operator) {
    let calls: Value =
        serde_json::from_str(&op.ok(&["tools", "calls", "--kind", "tool", "--json"])).unwrap();
    let call = &calls["items"][0];
    assert_eq!(
        [
            &call["call_id"],
            &call["session"],
            &call["server"],
            &call["tool"],
            &call["outcome"],
            &call["ms"],
            &call["bytes"],
            &call["folder"]
        ],
        [
            &json!("event-2"),
            &json!("s9"),
            &json!("probe"),
            &json!("load"),
            &json!("ok"),
            &json!(12),
            &json!(34),
            &json!("")
        ],
        "{calls}"
    );
}

/// The rows of `query` on the SQLite file `db` as JSON arrays (python3's sqlite3: what is on disk).
fn rows(db: &Path, query: &str) -> Value {
    let out = Command::new("python3")
        .args([
            "-c",
            "import json, sqlite3, sys; c = sqlite3.connect(sys.argv[1]); r = c.execute(sys.argv[2]).fetchall(); \
             c.commit(); print(json.dumps(r))",
        ])
        .arg(db)
        .arg(query)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{query}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}

/// The newest job of `kind` once `until` holds for it.
fn job_when(op: &Operator, kind: &str, until: impl Fn(&Value) -> bool) -> Value {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    loop {
        let out = op.cli(&["jobs", "list", "--json", "--limit", "1000"]);
        let page: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
        let job = page["items"]
            .as_array()
            .and_then(|jobs| jobs.iter().rev().find(|j| j["kind"] == kind));
        if let Some(job) = job.filter(|j| until(j)) {
            return job.clone();
        }
        assert!(std::time::Instant::now() < deadline, "job {kind}: {page:#}");
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
}

/// The id in a dossier's `dossier.jsonc` (its comment lines left out).
fn dossier_id(dir: &Path) -> String {
    let text: String = std::fs::read_to_string(dir.join("dossier.jsonc"))
        .unwrap()
        .lines()
        .filter(|l| !l.trim_start().starts_with("//"))
        .collect();
    let value: Value = serde_json::from_str(&text).unwrap();
    value["id"].as_str().unwrap().to_string()
}

/// The id that beta has before the migration.
const BETA: &str = "dsr-01JAAAAAAAAAAAAAAAAAAAAAAA";

/// The old home of the migration test in `home`: alpha (with a commented dossier.jsonc) and beta (with an id), an
/// old backup file, the workspace.db of `tests/fixtures/workspace-v13.sql`, a main checkout that ignores the old link
/// by a committed line, and alpha's worktree with the old link. Returns the repository and the worktree.
fn old_home(home: &Path) -> (PathBuf, PathBuf) {
    let repo = repository(home);
    let old = home.join(".agentworkspace");
    write(&old.join("alpha/plan.md"), "# alpha\n");
    write(&old.join("alpha/MEMORY/01-x.md"), "# x\n");
    write(
        &old.join("alpha/dossier.jsonc"),
        "// alpha's data\n{\n  \"title\": \"alpha\",\n  \"entities\": []\n}\n",
    );
    write(&old.join("beta/plan.md"), "# beta\n");
    write(
        &old.join("beta/dossier.jsonc"),
        &format!("{{\"id\": \"{BETA}\"}}\n"),
    );
    write(&old.join("workspace.db.v6.bak"), "an old backup");
    write(&repo.join(".gitignore"), "target/\n/.agentworkspace\n");
    git(home, &repo, &["add", ".gitignore"]);
    git(
        home,
        &repo,
        &["commit", "--quiet", "-m", "ignore the old link"],
    );
    let wt = home.join("worktrees/repo/alpha");
    git(
        home,
        &repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "legionctl/alpha",
            wt.to_str().unwrap(),
            "main",
        ],
    );
    std::os::unix::fs::symlink(old.join("alpha"), wt.join(".agentworkspace")).unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let fixture = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/workspace-v13.sql"),
    )
    .unwrap()
    .replace("@HOME@", home.to_str().unwrap())
    .replace("@NOW@", &now.to_string());
    let made = Command::new("python3")
        .args([
            "-c",
            "import sqlite3, sys; sqlite3.connect(sys.argv[1]).executescript(sys.argv[2])",
        ])
        .arg(old.join("workspace.db"))
        .arg(&fixture)
        .status()
        .unwrap();
    assert!(made.success());
    (repo, wt)
}

/// The operator of `home` again.
fn start_again(home: &Path, n: u32) -> Operator {
    operator_in(&format!("migrate{n}"), home)
}

/// The refusal of each dossier operation until the migration is done.
const NOT_MIGRATED: &str = "the dossiers are not migrated yet: run legionctl dossier migrate";

/// `dossier migrate` failed at the step that `why` names, and the old root is still there.
fn migration_failed(op: &Operator, home: &Path, why: &str) {
    let out = op.cli(&["dossier", "migrate"]);
    assert!(
        !out.status.success() && String::from_utf8_lossy(&out.stderr).contains(why),
        "{out:?}"
    );
    let job = job_when(op, "dossier.migrate", |j| j["state"] != "running");
    assert_eq!(job["state"], "failed", "{job:#}");
    assert!(
        home.join(".agentworkspace").exists(),
        "the old root stays until the counts match"
    );
}

/// An operator start with the old home there: nothing moves, each dossier operation (also `dossier list`) refuses
/// with the one message; then the dry run lists the refusal, the backup, the moves (with the
/// clash), the counts and the links, and changes nothing.
fn nothing_moves_before_the_user_runs_the_migration(op: &Operator, home: &Path, repo: &Path) {
    let (old, homes) = (home.join(".agentworkspace"), home.join("agentWorkspaces"));
    let page: Value = serde_json::from_str(&op.ok(&["jobs", "list", "--json"])).unwrap();
    assert!(
        !page["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|j| j["kind"] == "dossier.migrate"),
        "a start runs no migration: {page:#}"
    );
    assert!(old.join("alpha/plan.md").is_file() && !homes.join("alpha").exists());
    for args in [&["dossier", "list"][..], &["dossier", "use", "alpha"]] {
        let (done, why) = as_session(op, home, ("s-new", repo), args);
        assert!(!done && why.contains(NOT_MIGRATED), "{args:?}: {why}");
    }

    write(&homes.join("beta"), "in the way");
    let count = |table: &str| {
        rows(
            &old.join("workspace.db"),
            &format!("SELECT count(*) FROM {table}"),
        )[0][0]
            .clone()
    };
    let before = std::fs::read(old.join("workspace.db")).unwrap();
    let plan = op.ok(&["dossier", "migrate", "--dry-run"]);
    for line in [
        "keep the live binding: session s-live holds dossier beta (it moves with its rows)"
            .to_string(),
        format!("back up {}", old.join("workspace.db").display()),
        format!(
            "move {} to {} (a new id)",
            old.join("alpha").display(),
            homes.join("alpha").display()
        ),
        format!(
            "clash: {} exists, so {} cannot move there",
            homes.join("beta").display(),
            old.join("beta").display()
        ),
        format!("compare bindings: {} rows", count("bindings")),
        format!("compare mutants: {} rows", count("mutants")),
        format!("link {}/agentWorkspaces", repo.display()),
    ] {
        assert!(plan.contains(&line), "{line}\n{plan}");
    }
    assert_eq!(std::fs::read(old.join("workspace.db")).unwrap(), before);
    assert!(!homes.join("alpha").exists() && !repo.join("agentWorkspaces").exists());
}

/// Steps 2 to 5 of the migration (the live binding of s-live stays), each cut by an obstacle in its way, then the obstacle
/// removed and `dossier migrate` run again; returns the ids of alpha and beta.
fn migrate_with_a_cut_at_each_step(op: &Operator, home: &Path, repo: &Path) -> (String, String) {
    let (old, homes) = (home.join(".agentworkspace"), home.join("agentWorkspaces"));
    let state = home.join(".local/state/legionctl");
    // 2. the backup fails on a file in its place
    write(&state.join("backup"), "a file where the backup folder goes");
    migration_failed(op, home, "backing up");
    assert!(old.join("alpha").is_dir() && !homes.join("alpha").exists());
    std::fs::remove_file(state.join("backup")).unwrap();

    // 3. the move: a file holds beta's place, so alpha moves and beta stays
    migration_failed(op, home, "agentWorkspaces/beta exists");
    assert!(homes.join("alpha/MEMORY/01-x.md").is_file() && !old.join("alpha").exists());
    assert!(old.join("beta/plan.md").is_file());
    let alpha = dossier_id(&homes.join("alpha"));
    assert!(alpha.starts_with("dsr-") && alpha.len() == 30, "{alpha}");
    assert!(
        std::fs::read_to_string(homes.join("alpha/dossier.jsonc"))
            .unwrap()
            .starts_with("// alpha's data\n{\n  \"id\": "),
        "the id goes in, the comment stays"
    );
    std::fs::remove_file(homes.join("beta")).unwrap();

    // 4. the split: alpha's .state is a file
    write(&homes.join("alpha/.state"), "in the way");
    migration_failed(op, home, "alpha/.state");
    let beta = dossier_id(&homes.join("beta"));
    assert_eq!(beta, BETA, "an id that the dossier has stays");
    std::fs::remove_file(homes.join("alpha/.state")).unwrap();

    // 5. the links: the repository's info/exclude is a folder
    let exclude = repo.join(".git/info/exclude");
    let _ = std::fs::remove_file(&exclude);
    std::fs::create_dir_all(&exclude).unwrap();
    migration_failed(op, home, "info/exclude");
    std::fs::remove_dir(&exclude).unwrap();
    (alpha, beta)
}

/// After the migration: each row in its dossier or the operator's database, and the new links and ignore lines.
fn migrated_rows_links_and_lines(op: &Operator, home: &Path, repo: &Path, wt: &Path) {
    let (homes, state) = (
        home.join("agentWorkspaces"),
        home.join(".local/state/legionctl"),
    );
    // each row in its place, once: a session's rows by its binding, the rest of the repository's by the oldest
    // dossier there; the kills are keyed (without a key they ask for evidence again)
    let in_db =
        |dossier: &str, query: &str| rows(&homes.join(dossier).join(".state/dossier.db"), query);
    let counts = "SELECT (SELECT count(*) FROM writes), (SELECT count(*) FROM status_changes), \
                  (SELECT count(*) FROM nudges), (SELECT count(*) FROM test_runs), (SELECT count(*) FROM mutants), \
                  (SELECT count(*) FROM test_accepts)";
    assert_eq!(in_db("alpha", counts), json!([[2, 2, 1, 3, 2, 0]]));
    assert_eq!(in_db("beta", counts), json!([[1, 1, 1, 0, 0, 1]]));
    assert_eq!(
        in_db("alpha", "SELECT key FROM annotations"),
        json!([["rust:.:g"]])
    );
    assert_eq!(
        in_db("beta", "SELECT key FROM annotations"),
        json!([["rust:.:f"]])
    );
    assert_eq!(
        in_db("alpha", "SELECT killed FROM mutants ORDER BY id"),
        json!([[r#"[{"test":"a","file":null,"hash":null}]"#], ["[]"]])
    );
    let operator_db = state.join("operator.db");
    assert_eq!(
        rows(
            &operator_db,
            "SELECT (SELECT count(*) FROM bindings), (SELECT count(*) FROM workspaces), \
             (SELECT count(*) FROM sessions), (SELECT count(*) FROM threads), (SELECT count(*) FROM thread_log), \
             (SELECT count(*) FROM members), (SELECT count(*) FROM messages), (SELECT count(*) FROM watch_rules)"
        ),
        json!([[3, 2, 2, 1, 2, 2, 1, 1]])
    );
    assert!(
        op.ok(&["workspace", "list"])
            .contains(&wt.display().to_string()),
        "the workspace rows are the operator's now"
    );

    // the links: in the main checkout and the worktree, the old link and line are gone and staged, the new link
    // is ignored (the repository's info/exclude)
    for dir in [repo, wt] {
        assert!(std::fs::symlink_metadata(dir.join(".agentworkspace")).is_err());
        assert_eq!(
            std::fs::read_link(dir.join("agentWorkspaces")).unwrap(),
            homes
        );
        git(home, dir, &["check-ignore", "-q", "agentWorkspaces"]);
        assert_eq!(
            git(home, dir, &["diff", "--cached", "--name-only"]),
            ".gitignore"
        );
        assert_eq!(
            git(home, dir, &["log", "-1", "--format=%s"]),
            "ignore the old link"
        );
    }
    assert_eq!(
        std::fs::read_to_string(repo.join(".gitignore")).unwrap(),
        "target/\n"
    );
    assert_eq!(
        std::fs::read_to_string(wt.join(".gitignore")).unwrap(),
        "target/\n"
    );
    assert!(
        std::fs::read_to_string(repo.join(".git/info/exclude"))
            .unwrap()
            .lines()
            .any(|l| l == "/agentWorkspaces")
    );
}

/// The one-time move of a realistic old home (design 23, E1, decided 2026-10-15: only by `dossier migrate`): two
/// dossiers (beta with an id), a v13 workspace.db (P5's threads, kills not keyed) with bindings, sessions, a live
/// binding, kills, annotations and an accept, and alpha's worktree with the old link. An operator start moves nothing
/// and the dossier operations refuse; the dry run changes nothing; the migration (with the live binding there) is
/// cut at the backup, the move, the split and the links (an obstacle in the way each time), and each call does the
/// steps again. Then each row is in its place and counted once, the ids stay, the links and ignore lines are new, and
/// `~/.agentworkspace` is in the backup; a later start and a later call do nothing.
#[test]
fn the_dossiers_move_to_their_homes_once_and_keep_their_ids_links_and_rows() {
    let home = home("migrate");
    let (repo, wt) = old_home(&home);
    let (old, homes, state) = (
        home.join(".agentworkspace"),
        home.join("agentWorkspaces"),
        home.join(".local/state/legionctl"),
    );
    let op = start_again(&home, 1);
    nothing_moves_before_the_user_runs_the_migration(&op, &home, &repo);
    // a second start moves nothing either
    drop(op);
    let op = start_again(&home, 2);
    // the socket starts the operator at the first call
    op.ok(&["jobs", "list"]);
    assert!(
        old.join("alpha").is_dir() && !homes.join("alpha").exists(),
        "a start does not migrate"
    );
    let (alpha, beta) = migrate_with_a_cut_at_each_step(&op, &home, &repo);
    // 6. the counts match, and the old root goes to the backup
    let said = op.ok(&["dossier", "migrate"]);
    assert!(
        said.contains("moved the dossiers alpha, beta to their homes"),
        "{said}"
    );
    let done = job_when(&op, "dossier.migrate", |j| j["state"] != "running");
    assert_eq!(done["state"], "done", "{done:#}");
    assert_eq!(done["result"]["dossiers"], json!(["alpha", "beta"]));
    assert_eq!(
        done["result"]["dropped"],
        json!({"status_changes": 1}),
        "the row of the dossier that is gone"
    );
    assert!(!old.exists());
    let backups: Vec<String> = std::fs::read_dir(state.join("backup"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(backups.len(), 2, "{backups:?}");
    let rest = backups
        .iter()
        .find(|b| b.starts_with("agentworkspace-"))
        .unwrap();
    assert!(
        state
            .join("backup")
            .join(rest)
            .join("workspace.db.v6.bak")
            .is_file()
    );
    assert!(backups.iter().any(|b| b.starts_with("workspace.db.")));
    assert_eq!(
        (
            dossier_id(&homes.join("alpha")),
            dossier_id(&homes.join("beta"))
        ),
        (alpha.clone(), beta.clone()),
        "each id stays across the restarts"
    );
    let listed: Value = serde_json::from_str(&op.ok(&["dossier", "list", "--json"])).unwrap();
    let mut ids: Vec<(String, String)> = listed
        .as_array()
        .unwrap()
        .iter()
        .map(|d| {
            (
                d["name"].as_str().unwrap().into(),
                d["id"].as_str().unwrap().into(),
            )
        })
        .collect();
    ids.sort();
    assert_eq!(ids, [("alpha".into(), alpha), ("beta".into(), beta)]);
    migrated_rows_links_and_lines(&op, &home, &repo, &wt);
    drop(op);

    // a later start and a later call have nothing to move
    let op = start_again(&home, 3);
    assert!(
        op.ok(&["dossier", "migrate"])
            .contains("nothing to migrate")
    );
    let page: Value = serde_json::from_str(&op.ok(&["jobs", "list", "--json"])).unwrap();
    let runs = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|j| j["kind"] == "dossier.migrate")
        .count();
    assert_eq!(runs, 5, "four cut calls and the one that is done");
}

/// A dossier root that resolves to the old root (R3-1: `~/agentWorkspaces` is a link to `~/.agentworkspace`, as a user
/// layer with the old default does): the dry run and the migration refuse before anything moves, and each dossier
/// keeps its files. Before the fix the move took the folder for a copy of a cut-off run and removed it.
#[test]
fn a_migration_into_the_old_root_through_a_link_refuses_and_keeps_each_dossier() {
    let home = home("migrate-self");
    let _ = old_home(&home);
    let old = home.join(".agentworkspace");
    std::os::unix::fs::symlink(&old, home.join("agentWorkspaces")).unwrap();
    let op = start_again(&home, 8);
    for args in [
        &["dossier", "migrate", "--dry-run"][..],
        &["dossier", "migrate"],
    ] {
        let out = op.cli(args);
        assert!(
            !out.status.success()
                && String::from_utf8_lossy(&out.stderr).contains("apart from the old root"),
            "{args:?}: {out:?}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(old.join("alpha/plan.md")).unwrap(),
        "# alpha\n"
    );
    assert_eq!(
        std::fs::read_to_string(old.join("beta/plan.md")).unwrap(),
        "# beta\n"
    );
    assert!(old.join("alpha/MEMORY/01-x.md").is_file() && old.join("workspace.db").is_file());
}

/// A live session in a home that is not migrated (design 23, E1; the session that runs the migration is one): its
/// hooks never block it. Session start and prompt pass with one line that says to migrate, stop does not block, and a
/// write is not denied (the guard fails with exit 1). After `dossier migrate` its binding is the moved row: the session keeps its dossier, the write
/// guard denies another dossier again and a write to the old place, and a session without a dossier is held at the
/// prompt as before.
#[test]
fn the_hooks_of_a_live_session_pass_before_the_migration_and_gate_after_it() {
    let home = home("unmigrated");
    let (repo, _) = old_home(&home);
    let op = start_again(&home, 7);
    let hook = |event: &str, fields: Value| {
        let out = op.run(&repo, &[], &["hook", event], &claude_hook(event, &fields));
        assert!(out.status.success(), "{event}: {out:?}");
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    let write = |session: &str, path: PathBuf| {
        hook(
            "pre-tool",
            json!({"session_id": session, "cwd": repo, "tool_name": "Write",
                   "tool_input": {"file_path": path, "content": "x"}}),
        )
    };
    let (old, homes) = (home.join(".agentworkspace"), home.join("agentWorkspaces"));

    assert_eq!(
        hook("session-start", json!({"session_id": "s-new", "cwd": repo})),
        NOT_MIGRATED
    );
    assert_eq!(
        hook(
            "prompt",
            json!({"session_id": "s-new", "cwd": repo, "prompt": "fix the bug"})
        ),
        NOT_MIGRATED
    );
    assert_eq!(
        hook("stop", json!({"session_id": "s-new", "cwd": repo})),
        ""
    );
    // the guard's operations refuse before the migration: a failure (exit 1) that denies nothing
    let unmigrated = op.run(
        &repo,
        &[],
        &["hook", "pre-tool"],
        &claude_hook(
            "pre-tool",
            &json!({"session_id": "s-live", "cwd": repo, "tool_name": "Write",
                    "tool_input": {"file_path": old.join("alpha/plan.md"), "content": "x"}}),
        ),
    );
    assert!(
        unmigrated.status.code() == Some(1)
            && unmigrated.stdout.is_empty()
            && String::from_utf8_lossy(&unmigrated.stderr).contains(NOT_MIGRATED),
        "not denied: {unmigrated:?}"
    );

    op.ok(&["dossier", "migrate"]);
    let (bound, active) = as_session(&op, &home, ("s-live", &repo), &["dossier", "active"]);
    assert!(bound && active.contains("active dossier beta"), "{active}");
    assert!(write("s-live", homes.join("alpha/plan.md")).contains("deny"));
    assert!(
        write("s-live", homes.join("beta/plan.md"))
            .contains("beta/plan.md is in this session's dossier")
    );
    // a write to the old place would make the old root again and refuse every dossier operation (R3-16)
    let stale = write("s-live", old.join("beta/plan.md"));
    assert!(
        stale.contains("deny") && stale.contains("the dossier migration moved away"),
        "{stale}"
    );
    let held: Value = serde_json::from_str(&hook(
        "prompt",
        json!({"session_id": "s-new", "cwd": repo, "prompt": "fix the bug"}),
    ))
    .unwrap();
    assert_eq!(held["decision"], "block", "{held}");
}

/// The run of `args` as session `s` in `cwd`: whether it succeeded, and its stdout and stderr.
fn as_session(
    op: &Operator,
    home: &Path,
    (s, cwd): (&str, &Path),
    args: &[&str],
) -> (bool, String) {
    let o = op.run(
        cwd,
        &[("HOME", home.to_str().unwrap()), ("LEGIONCTL_SESSION", s)],
        args,
        "",
    );
    (
        o.status.success(),
        String::from_utf8_lossy(&o.stdout).to_string() + &String::from_utf8_lossy(&o.stderr),
    )
}

/// The output of `args` as session `s` in `cwd`, which must succeed.
fn session_ok(op: &Operator, home: &Path, (s, cwd): (&str, &Path), args: &[&str]) -> String {
    let (success, out) = as_session(op, home, (s, cwd), args);
    assert!(success, "{args:?}: {out}");
    out
}

/// The first dossier use in a directory (design 23, E1a): a real `agentWorkspaces` folder with two dossiers (one
/// name in use in the home) and a file is imported (copies compared, the clash renamed, the folder kept while the
/// file is there), a cut-off import finishes without a second copy, and the folder goes to the backup and becomes
/// the link. git ignores the link by info/exclude in the main checkout and its worktrees, and the user's `.gitignore`
/// and index stay as they are (EV-review-10 S7: a staged line blocked the first merge); a plain folder gets the link only; a link that the user made stays; the home itself is not imported.
#[test]
fn the_first_use_in_a_repository_imports_its_agentworkspaces_folder_and_links_the_home() {
    let (op, home) = operator("home-link");
    let repo = repository(&home);
    git(&home, &repo, &["rm", "--quiet", ".gitignore"]);
    git(&home, &repo, &["commit", "--quiet", "-m", "no ignore line"]);
    let homes = home.join("agentWorkspaces");
    let backup = home.join(".local/state/legionctl/backup");
    write(&homes.join("notes/plan.md"), "# my notes\n");
    let theirs = repo.join("agentWorkspaces");
    write(&theirs.join("notes/plan.md"), "# their notes\n");
    write(&theirs.join("notes/MEMORY/01-a.md"), "# a\n");
    write(&theirs.join("draft/plan.md"), "# draft\n");
    write(&theirs.join("README.txt"), "not a dossier\n");
    let use_port = |cwd: &Path| as_session(&op, &home, ("s1", cwd), &["dossier", "use", "port"]);

    // both dossiers come in, the clash as notes-imported-1; the file keeps the folder there
    let (_, first) = use_port(&repo);
    assert!(
        first.contains(&format!(
            "imported {} as dossier notes-imported-1 (the name notes is in use)",
            theirs.join("notes").display()
        )) && first.contains("keeps README.txt"),
        "{first}"
    );
    assert_eq!(
        std::fs::read_to_string(homes.join("notes-imported-1/MEMORY/01-a.md")).unwrap(),
        "# a\n"
    );
    assert_eq!(
        std::fs::read_to_string(homes.join("notes/plan.md")).unwrap(),
        "# my notes\n"
    );
    assert!(dossier_id(&homes.join("draft")).starts_with("dsr-"));
    assert!(theirs.join("notes/plan.md").is_file() && !theirs.is_symlink());

    // the file goes; an import cut off before the folder moves (a file where the backup goes) leaves the source
    std::fs::remove_file(theirs.join("README.txt")).unwrap();
    write(&backup, "in the way");
    let (cut, why) = use_port(&repo);
    assert!(!cut && why.contains("moving"), "{why}");
    assert!(theirs.join("draft/plan.md").is_file());
    std::fs::remove_file(&backup).unwrap();

    // the next use finishes: nothing copied twice, the source in the backup, the link in its place
    let (done, second) = use_port(&repo);
    assert!(
        done && second.contains("is dossier notes-imported-1 (imported before)"),
        "{second}"
    );
    let mut names: Vec<String> = std::fs::read_dir(&homes)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["draft", "notes", "notes-imported-1", "port"]);
    assert_eq!(std::fs::read_link(&theirs).unwrap(), homes);
    let saved = std::fs::read_dir(&backup)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    assert!(
        saved.join("notes/MEMORY/01-a.md").is_file(),
        "{}",
        saved.display()
    );

    // git ignores the link by info/exclude (the line without a trailing slash); the checkout has no change
    assert!(
        std::fs::read_to_string(repo.join(".git/info/exclude"))
            .unwrap()
            .lines()
            .any(|l| l == "/agentWorkspaces")
    );
    assert!(!repo.join(".gitignore").exists());
    assert_eq!(git(&home, &repo, &["status", "--porcelain"]), "");
    assert_eq!(
        git(&home, &repo, &["check-ignore", "agentWorkspaces"]),
        "agentWorkspaces"
    );
    links_beyond_the_main_checkout(&op, &home, &repo);
}

/// The link in a worktree whose branch has no line yet (info/exclude), in a plain folder (no .gitignore), beside a
/// link that the user made (kept and named), and in the home itself (nothing to import or link).
fn links_beyond_the_main_checkout(op: &Operator, home: &Path, repo: &Path) {
    let homes = home.join("agentWorkspaces");
    let use_port = |cwd: &Path| session_ok(op, home, ("s1", cwd), &["dossier", "use", "port"]);
    session_ok(op, home, ("s1", repo), &["workspace", "open"]);
    let ws = home.join("worktrees/repo/port");
    assert_eq!(
        std::fs::read_link(ws.join("agentWorkspaces")).unwrap(),
        homes
    );
    assert!(ws.join("agentWorkspaces/port/plan.md").is_file());
    assert_eq!(git(home, &ws, &["status", "--porcelain"]), "");
    assert!(
        std::fs::read_to_string(repo.join(".git/info/exclude"))
            .unwrap()
            .lines()
            .any(|l| l == "/agentWorkspaces")
    );

    let plain = home.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    use_port(&plain);
    assert_eq!(
        std::fs::read_link(plain.join("agentWorkspaces")).unwrap(),
        homes
    );
    assert!(!plain.join(".gitignore").exists());

    let other = home.join("other");
    std::fs::create_dir_all(&other).unwrap();
    std::os::unix::fs::symlink(home.join("elsewhere"), other.join("agentWorkspaces")).unwrap();
    let kept = use_port(&other);
    assert!(
        kept.contains("links to") && kept.contains("does not replace it"),
        "{kept}"
    );
    assert_eq!(
        std::fs::read_link(other.join("agentWorkspaces")).unwrap(),
        home.join("elsewhere")
    );

    use_port(home);
    assert!(std::fs::symlink_metadata(&homes).unwrap().is_dir());
    assert!(homes.join("notes-imported-1").is_dir() && !homes.join("notes-imported-2").exists());
}

/// The descriptions of the mutants in `dossier`'s own database.
fn mutants_of(home: &Path, dossier: &str) -> Value {
    rows(
        &home
            .join("agentWorkspaces")
            .join(dossier)
            .join(".state/dossier.db"),
        "SELECT description FROM mutants ORDER BY id",
    )
}

/// The mutant `description` killed by `adds`, recorded by session `s` in `ws`.
fn record_mutant(op: &Operator, home: &Path, (s, ws): (&str, &Path), description: &str) {
    session_ok(
        op,
        home,
        (s, ws),
        &[
            "code",
            "tests",
            "mutant",
            "record",
            "src/lib.rs",
            "1",
            description,
            "adds",
        ],
    );
}

/// The mutants that `code tests show adds` gives in `ws`.
fn shown_mutants(op: &Operator, home: &Path, (s, ws): (&str, &Path)) -> Vec<String> {
    let shown: Value = serde_json::from_str(&session_ok(
        op,
        home,
        (s, ws),
        &["code", "tests", "show", "adds", "--json"],
    ))
    .unwrap();
    shown["mutants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["description"].as_str().unwrap().to_string())
        .collect()
}

/// Repository facts (design 23, E1 rule 4) and renames (decision 7): a kill recorded in alpha's workspace is in
/// alpha's dossier.db only, and beta's workspace in the same repository reads it. A rename to a name in use is
/// refused; a rename keeps the id, the binding, the workspace row, the facts and the link, and the next fact goes
/// into the renamed dossier. The write guard sees through the link: beta's session may not write alpha's files.
#[test]
fn facts_written_from_one_dossier_are_read_from_another_and_a_rename_keeps_rows_and_links() {
    let (op, home) = operator("facts");
    let repo = repository(&home);
    write(
        &repo.join("tests/e2e.rs"),
        "#[test]\nfn adds() {\n    assert_eq!(1 + 1, 2);\n}\n",
    );
    write(&repo.join("src/lib.rs"), "pub fn f() {}\n");
    git(&home, &repo, &["add", "."]);
    git(&home, &repo, &["commit", "--quiet", "-m", "a test"]);
    for (s, dossier) in [("s-a", "alpha"), ("s-b", "beta")] {
        session_ok(&op, &home, (s, &repo), &["dossier", "use", dossier]);
        session_ok(&op, &home, (s, &repo), &["workspace", "open"]);
    }
    let (ws_a, ws_b) = (
        home.join("worktrees/repo/alpha"),
        home.join("worktrees/repo/beta"),
    );
    let homes = home.join("agentWorkspaces");

    record_mutant(&op, &home, ("s-a", &ws_a), "m1");
    assert_eq!(
        (mutants_of(&home, "alpha"), mutants_of(&home, "beta")),
        (json!([["m1"]]), json!([]))
    );
    assert_eq!(shown_mutants(&op, &home, ("s-b", &ws_b)), ["m1"]);

    // a name in use is refused; a rename keeps the id and moves every row
    let id = dossier_id(&homes.join("alpha"));
    let (renamed, why) = as_session(
        &op,
        &home,
        ("s-a", &repo),
        &["dossier", "rename", "alpha", "beta"],
    );
    assert!(
        !renamed && why.contains("the dossier name beta is in use"),
        "{why}"
    );
    session_ok(
        &op,
        &home,
        ("s-a", &repo),
        &["dossier", "rename", "alpha", "gamma"],
    );
    assert_eq!(dossier_id(&homes.join("gamma")), id);
    assert!(!homes.join("alpha").exists());
    assert!(
        session_ok(&op, &home, ("s-a", &repo), &["dossier", "active"])
            .contains("active dossier gamma")
    );
    let list: Value = serde_json::from_str(&session_ok(
        &op,
        &home,
        ("s-a", &repo),
        &["workspace", "list", "--json"],
    ))
    .unwrap();
    assert!(
        list.as_array()
            .unwrap()
            .iter()
            .any(|w| w["dossier"] == "gamma" && w["path"] == ws_a.to_str().unwrap()),
        "{list:#}"
    );
    assert!(
        ws_a.join("agentWorkspaces/gamma/plan.md").is_file(),
        "the link stays valid"
    );
    record_mutant(&op, &home, ("s-a", &ws_a), "m2");
    assert_eq!(mutants_of(&home, "gamma"), json!([["m1"], ["m2"]]));
    assert_eq!(shown_mutants(&op, &home, ("s-b", &ws_b)), ["m1", "m2"]);
    // beta's workspace writes into beta, not into the oldest dossier of the repository
    record_mutant(&op, &home, ("s-b", &ws_b), "m3");
    assert_eq!(mutants_of(&home, "beta"), json!([["m3"]]));
    let mut shown = shown_mutants(&op, &home, ("s-a", &ws_a));
    shown.sort();
    assert_eq!(shown, ["m1", "m2", "m3"]);

    // beta's session reaches its own dossier through the link, not gamma's
    let guard = |path: PathBuf| {
        let out = op.run(
            &ws_b,
            &[("HOME", home.to_str().unwrap())],
            &["hook", "pre-tool"],
            &claude_hook(
                "pre-tool",
                &json!({"session_id": "s-b", "cwd": ws_b, "tool_name": "Write",
                        "tool_input": {"file_path": path, "content": "x"}}),
            ),
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    };
    assert!(guard(ws_b.join("agentWorkspaces/gamma/plan.md")).contains("active in session s-a"));
    assert!(
        guard(ws_b.join("agentWorkspaces/beta/plan.md"))
            .contains("beta/plan.md is in this session's dossier")
    );
}

/// The step that a message about an off operator gives (U-14).
const OPERATOR_ON: &str =
    "turn it on with `systemctl --user enable --now legionctl-operator.socket`";

/// The hooks of the shipped plugin (hooks.json, the `PreCompact` block of 2026-10-17, todo-176): each command runs on
/// this build with each recorded payload of its event. With the operator up, every hook exits 0 and blocks only by its
/// rule: the dossier gate (prompt) and the dossier nudge (stop) for a session without a dossier, nothing for a session
/// with one. With the operator down, every hook fails with exit 1 and blocks nothing. An event that this build does
/// not know, and any other words after `hook`, exit 1 (clap's exit 2 would block) with this build's name.
#[test]
fn every_plugin_hook_runs_on_this_build_and_blocks_only_by_its_rule() {
    let (op, home) = operator("plugin-hooks");
    let repo = repository(&home);
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/agent-plugin");
    let fixtures = plugin.join("../../tests/fixtures/claude-code-hooks");
    let manifest: Value =
        serde_json::from_str(&std::fs::read_to_string(plugin.join("hooks/hooks.json")).unwrap())
            .unwrap();
    let events: Vec<String> = manifest["hooks"]
        .as_object()
        .unwrap()
        .values()
        .flat_map(|groups| groups.as_array().unwrap())
        .flat_map(|group| group["hooks"].as_array().unwrap())
        .map(|hook| {
            let command = hook["command"].as_str().unwrap();
            command
                .strip_prefix("legionctl hook ")
                .unwrap_or_else(|| panic!("not a legionctl hook: {command}"))
                .to_string()
        })
        .collect();
    assert_eq!(events.len(), 10, "{events:?}");
    let ok = |args: &[&str]| {
        let out = op.run(&repo, &[("LEGIONCTL_SESSION", "s-bound")], args, "");
        assert!(out.status.success(), "{args:?}: {out:?}");
    };
    ok(&["dossier", "use", "hooks"]);
    ok(&["workspace", "open"]);
    let down = home.join("down");
    std::fs::create_dir_all(&down).unwrap();
    let mut blocked = std::collections::BTreeSet::new();
    for event in &events {
        let payloads: Vec<PathBuf> = std::fs::read_dir(&fixtures)
            .unwrap()
            .map(|e| e.unwrap().path())
            .filter(|p| {
                let stem = p.file_stem().unwrap().to_str().unwrap();
                stem == event || stem.starts_with(&format!("{event}-"))
            })
            .collect();
        assert!(!payloads.is_empty(), "no recorded payload of {event}");
        for (payload, session) in payloads
            .iter()
            .flat_map(|p| [(p, "s-free"), (p, "s-bound")])
        {
            let mut input: Value =
                serde_json::from_str(&std::fs::read_to_string(payload).unwrap()).unwrap();
            input["session_id"] = json!(session);
            input["cwd"] = json!(repo);
            let input = input.to_string();
            let up = op.run(&repo, &[], &["hook", event], &input);
            assert_eq!(up.status.code(), Some(0), "{payload:?} {session}: {up:?}");
            let said = String::from_utf8_lossy(&up.stdout);
            if said.contains(r#""decision":"block""#)
                || said.contains(r#""permissionDecision":"deny""#)
            {
                blocked.insert(format!("{event} {session}"));
            }
            let out = op.run(
                &repo,
                &[("XDG_RUNTIME_DIR", down.to_str().unwrap())],
                &["hook", event],
                &input,
            );
            let err = String::from_utf8_lossy(&out.stderr);
            assert!(
                out.status.code() == Some(1)
                    && out.stdout.is_empty()
                    && err.starts_with("legionctl hook: legionctl-operator is off (no answer on ")
                    && err.contains(OPERATOR_ON),
                "{payload:?} {session}, operator down: {out:?}"
            );
        }
    }
    assert_eq!(
        blocked.into_iter().collect::<Vec<_>>(),
        ["prompt s-free", "stop s-free"],
        "only the dossier gate and the dossier nudge block, and only a session without a dossier"
    );
    reminders_and_setup_rows(&op, &home, &repo, &fixtures);
    let exe = env!("CARGO_BIN_EXE_legionctl");
    for args in [
        &["hook", "post-compact"][..],
        &["hook"],
        &["hook", "pre-tool", "extra"],
        &["hook", "-x"],
    ] {
        let out = op.run(&repo, &[], args, "{}");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            out.status.code() == Some(1)
                && out.stdout.is_empty()
                && err.starts_with(&format!("legionctl hook: this legionctl ({exe} "))
                && err.contains(
                    "the legionctl plugin is newer than this legionctl, and the hook did nothing"
                )
                && err.contains("the events of this legionctl: session-start, prompt, pre-tool"),
            "{args:?}: {out:?}"
        );
    }
}

/// The dossier reminder is for a session with a dossier, and the session start names the setup rows that setup
/// check gives (EV-review-12: the reminder came on every prompt in a plain folder; the hook gave a bare count that
/// the user could not match with setup check).
fn reminders_and_setup_rows(op: &Operator, home: &Path, repo: &Path, fixtures: &Path) {
    // the dossier reminder is for a session with a dossier; a plain folder with none got it on every prompt
    // (EV-review-12, live in a Scratch HOME)
    let plain = home.join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    let prompt = |cwd: &Path, session: &str| -> String {
        let mut input: Value =
            serde_json::from_str(&std::fs::read_to_string(fixtures.join("prompt.json")).unwrap())
                .unwrap();
        input["session_id"] = json!(session);
        input["cwd"] = json!(cwd);
        let out = op.run(cwd, &[], &["hook", "prompt"], &input.to_string());
        assert_eq!(out.status.code(), Some(0), "{out:?}");
        String::from_utf8(out.stdout).unwrap()
    };
    let (free, bound) = (prompt(&plain, "s-free"), prompt(repo, "s-bound"));
    let reminder = "Keep this session's dossier current";
    assert!(
        free.contains("Reminder: Write all user-facing text") && !free.contains(reminder),
        "{free}"
    );
    assert!(bound.contains(reminder), "{bound}");
    // the session start names the setup rows that setup check gives (it gave a bare count that the user could not
    // match: "1 need attention" while setup check said all set up)
    let check: Value =
        serde_json::from_slice(&op.run(repo, &[], &["setup", "check", "--json"], "").stdout)
            .unwrap();
    let rows: Vec<String> = check["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["status"] != "current")
        .map(|i| {
            format!(
                "{} for {} is {}",
                i["what"].as_str().unwrap(),
                i["agent"].as_str().unwrap(),
                i["status"].as_str().unwrap()
            )
        })
        .collect();
    assert!(!rows.is_empty(), "{check}");
    let mut input: Value = serde_json::from_str(
        &std::fs::read_to_string(fixtures.join("session-start.json")).unwrap(),
    )
    .unwrap();
    input["session_id"] = json!("s-free");
    input["cwd"] = json!(plain);
    let start = String::from_utf8(
        op.run(&plain, &[], &["hook", "session-start"], &input.to_string())
            .stdout,
    )
    .unwrap();
    assert!(
        start.contains(&format!(
            "legionctl setup: {}. Run /legionctl:setup.",
            rows.join("; ")
        )),
        "{start}\n{rows:?}"
    );
}

/// `doctor` names a legionctl plugin of another build (todo-176): Claude Code reads a folder marketplace's plugin from
/// that folder, so a marketplace that is not the package folder (here: a checkout) gives the plugin of that tree, and
/// its hooks can name events that this build does not know. The package folder as the marketplace gives no finding.
#[test]
fn doctor_names_a_claude_code_plugin_that_is_not_of_this_build() {
    let (op, home) = operator("plugin-doctor");
    let plugin_findings = || -> Vec<String> {
        String::from_utf8_lossy(&op.cli(&["doctor"]).stdout)
            .lines()
            .filter_map(|l| l.strip_prefix("- "))
            .filter(|l| {
                l.starts_with("the legionctl plugin")
                    || l.starts_with("the Claude Code marketplace")
            })
            .map(String::from)
            .collect()
    };
    assert_eq!(
        plugin_findings(),
        Vec::<String>::new(),
        "no plugin installed"
    );
    // a checkout as the marketplace, with a newer plugin than this build
    let checkout = home.join("checkout");
    write(
        &checkout.join(".claude-plugin/marketplace.json"),
        r#"{"name": "lenovolegionlinux", "plugins": [{"name": "legionctl", "source": "./plugin"}]}"#,
    );
    write(
        &checkout.join("plugin/hooks/hooks.json"),
        r#"{"hooks": {"PreCompact": [{"hooks": [{"type": "command", "command": "legionctl hook pre-compact"}]}],
            "PostCompact": [{"hooks": [{"type": "command", "command": "legionctl hook post-compact"}]}]}}"#,
    );
    let marketplace = |path: &Path| {
        write(
            &home.join(".claude/plugins/known_marketplaces.json"),
            &json!({"lenovolegionlinux": {"source": {"source": "directory", "path": path}}})
                .to_string(),
        );
    };
    marketplace(&checkout);
    // the install path is the copy of the install, which Claude Code does not read for a folder marketplace
    let copy = home.join(".claude/plugins/cache/lenovolegionlinux/legionctl/0.6.0");
    write(&copy.join("hooks/hooks.json"), r#"{"hooks": {}}"#);
    write(
        &home.join(".claude/plugins/installed_plugins.json"),
        &json!({"version": 2, "plugins": {"legionctl@lenovolegionlinux": [{"scope": "user", "installPath": copy}]}})
            .to_string(),
    );
    let package = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations");
    let fix = "legionctl setup apply installs it from the package";
    assert_eq!(
        plugin_findings(),
        [
            format!(
                "the Claude Code marketplace lenovolegionlinux is {}, not the package folder {}: its plugin and this \
                 legionctl can be of different builds; {fix}",
                checkout.display(),
                package.display()
            ),
            format!(
                "the legionctl plugin ({}) has hooks for events that this legionctl does not know: post-compact; each \
                 of these hooks fails; {fix}",
                checkout.join("plugin").display()
            ),
        ]
    );
    marketplace(&package);
    assert_eq!(
        plugin_findings(),
        Vec::<String>::new(),
        "the package's own plugin"
    );
}

/// A session in the home folder itself (a plain folder workspace whose folder is $HOME) has `~/agentWorkspaces` as the
/// dossiers' home, not as a link to it: r324's doctor said "~/agentWorkspaces is a folder that keeps files that are not
/// dossiers, not ~/agentWorkspaces" for it.
#[test]
fn a_workspace_in_the_home_folder_is_not_a_finding_of_the_dossiers_home() {
    let (op, home) = operator("home-ws");
    write(
        &home.join(".config/legionctl/home-ws.jsonc"),
        &json!({
            "session": {"agents": {"probe": ["true"]}, "scope": false, "writable": []},
            "workspace": {"root": home.join("layers")}
        })
        .to_string(),
    );
    let started = op.run(
        &home,
        &[("HOME", home.to_str().unwrap())],
        &["session", "start", "probe", "--dossier", "notes", "--json"],
        "",
    );
    assert!(
        started.status.success(),
        "{}",
        String::from_utf8_lossy(&started.stderr)
    );
    assert!(home.join("agentWorkspaces/notes").is_dir());
    let doctor = String::from_utf8_lossy(&op.cli(&["doctor"]).stdout).to_string();
    assert!(!doctor.contains("agentWorkspaces is a"), "{doctor}");
}

/// `n` users' `views save` in parallel while the daemon's events flood the log, each a change with its call record row,
/// its version fact and its audit: the calls that failed, and the ok rows, audits and version facts there are.
fn parallel_changes(tag: &str, n: usize) -> (Vec<String>, [i64; 3]) {
    use legionctl_core::db::{Db, EventFilter, OPERATOR};
    let (op, home) = operator(tag);
    let db = home.join(".local/state/legionctl/operator.db");
    op.ok(&["events"]);
    let h = home.to_str().unwrap();
    let save = |i: usize| {
        let view = format!(
            r#"{{"name": "v{i}", "widget": "table", "source": {{"op": "thread.list", "path": "$"}}, "columns": ["title"]}}"#
        );
        op.run(&home, &[("HOME", h)], &["views", "save"], &view)
    };
    let done = std::sync::atomic::AtomicBool::new(false);
    let (failed, flooded) = std::thread::scope(|s| {
        let flood = s.spawn(|| flood::recorder_flood(&db, &done));
        let saves: Vec<_> = (0..n).map(|i| s.spawn(move || save(i))).collect();
        let outs: Vec<_> = saves.into_iter().map(|t| t.join().unwrap()).collect();
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        let failed: Vec<String> = outs
            .into_iter()
            .filter(|o| !o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stderr).to_string())
            .collect();
        (failed, flood.join().unwrap())
    });
    assert!(flooded > 100, "the flood ran all along");
    let db = Db::open(&db, OPERATOR).unwrap();
    let count = |sql: &str| -> i64 { db.conn().query_row(sql, [], |r| r.get(0)).unwrap() };
    let filter = EventFilter {
        kinds: &["views.save"],
        ..Default::default()
    };
    let audits = i64::try_from(db.events_page(&filter, 0, 1000).unwrap().0.len()).unwrap();
    (
        failed,
        [
            count("SELECT count(*) FROM calls WHERE name = 'views.save' AND outcome = 'ok'"),
            audits,
            count(
                "SELECT count(DISTINCT entity) FROM facts WHERE entity LIKE 'view:v%' AND attribute = 'version'",
            ),
        ],
    )
}

/// Many changes at once (EV-validation-one-context: 1 of 40 parallel `views save` failed "the call record: database
/// is locked ... busy for 5 s"): 40, then 200 users' changes in parallel while the daemon's events flood the log. Each
/// call succeeds, and each has its row in the call record, its audit and its view's version fact.
#[test]
fn parallel_changes_all_land_while_the_event_log_floods() {
    for n in [40, 200] {
        let (failed, rows) = parallel_changes(&format!("par{n}"), n);
        assert!(
            failed.is_empty(),
            "{} of {n} failed: {:?}",
            failed.len(),
            failed.first()
        );
        let n = i64::try_from(n).unwrap();
        assert_eq!(
            rows,
            [n, n, n],
            "the call record's ok rows, the audits and the version facts"
        );
    }
}

/// todo-189, r361: the daemon's `session.open` events at 20 times the peak of r361, written as the recorder writes them
/// (batched, on the event log's connection, which does not sync each commit), while agents' tool calls and headless
/// runs come in (control state: each commit syncs, B7). Each call's hook answers and its audit lands, and each
/// run keeps its ACP session id and ends: no write waits out the busy timeout (each commit held the write lock across
/// its own fsync, and the waiters starved: "keeping the ACP session id", "audit of internal.agent.tool not recorded:
/// database is locked").
#[test]
fn tool_calls_and_runs_land_in_the_event_log_while_another_writer_floods_it() {
    use legionctl_core::db::{Db, EventFilter, OPERATOR};
    let (op, home) = operator("db-lock");
    let db = home.join(".local/state/legionctl/operator.db");
    let repo = repository(&home);
    let script = adapter::node_script(&home);
    adapter::executable(&home.join(".local/bin/fake-acp"), &script);
    write(
        &home.join(".config/legionctl/run.jsonc"),
        &json!({"session": {"acp": {"fake": adapter::COMMAND}, "scope": false, "writable": []}})
            .to_string(),
    );
    let run = |a: usize| {
        let dossier = format!("flood{a}");
        let out = op.run(
            &repo,
            &[("HOME", home.to_str().unwrap())],
            &["session", "run", "fake", "hi", "--dossier", &dossier],
            "",
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    let calls = |agent: usize| {
        for i in 0..25 {
            let fields = json!({"session_id": "flood", "cwd": home, "tool_name": "Bash",
                                "tool_input": {"command": format!("echo {agent}-{i}"), "description": "run"}});
            let out = op.run(
                &home,
                &[],
                &["hook", "post-tool"],
                &claude_hook("post-tool", &fields),
            );
            assert!(
                out.status.success() && out.stderr.is_empty(),
                "{}",
                String::from_utf8_lossy(&out.stderr)
            );
        }
    };
    calls(0);
    let done = std::sync::atomic::AtomicBool::new(false);
    std::thread::scope(|s| {
        let flood = s.spawn(|| flood::recorder_flood(&db, &done));
        let agents: Vec<_> = (1..9).map(|a| s.spawn(move || calls(a))).collect();
        let runs: Vec<_> = (1..4).map(|a| s.spawn(move || run(a))).collect();
        let ended: Vec<_> = agents
            .into_iter()
            .chain(runs)
            .map(std::thread::ScopedJoinHandle::join)
            .collect();
        // the flood stops first, also when a caller failed: else the scope waits for it forever
        done.store(true, std::sync::atomic::Ordering::Relaxed);
        for e in ended {
            e.unwrap();
        }
        assert!(flood.join().unwrap() > 100, "the flood ran all along");
    });
    let filter = EventFilter {
        kinds: &["internal.agent.tool"],
        ..Default::default()
    };
    let audits = Db::open(&db, OPERATOR)
        .unwrap()
        .events_page(&filter, 0, 1000)
        .unwrap()
        .0;
    assert_eq!(
        audits.len(),
        225,
        "every tool call's audit is in the event log"
    );
    let jobs = Db::open(&db, OPERATOR)
        .unwrap()
        .jobs_page(0, 100)
        .unwrap()
        .0;
    assert!(
        jobs.len() == 3
            && jobs
                .iter()
                .all(|j| j.state == "done" && j.input["acp"].is_string()),
        "each run kept its ACP session id and ended: {jobs:?}"
    );
}

/// A write that waits out the busy timeout (another process holds the write lock) says so in one plain sentence, not
/// as SQLite's "database is locked: Error code 5".
#[test]
fn a_write_that_waits_out_the_lock_says_so_in_one_sentence() {
    let (op, home) = operator("db-busy");
    op.ok(&["events", "--limit", "1"]);
    let db = home.join(".local/state/legionctl/operator.db");
    // the agent loaded its skills before the database got busy (its first call)
    op.run(
        &home,
        &[("LEGIONCTL_SESSION", "flood")],
        &["dossier", "active"],
        "",
    );
    let holder = rusqlite::Connection::open(&db).unwrap();
    holder.execute_batch("BEGIN IMMEDIATE").unwrap();
    let out = op.run(
        &home,
        &[("LEGIONCTL_SESSION", "flood")],
        &["dossier", "use", "flood"],
        "",
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success()
            && stderr.contains("other writes kept the legionctl database busy for 5 s; try again")
            && !stderr.contains("Error code 5"),
        "{stderr}"
    );
}

/// The legionctl hooks answer Codex's own payloads (todo-205: the plugin's hooks.json runs in Codex on the same
/// events): each payload as codex 0.163 sent it through the installed plugin, recorded 2026-10-18 (tests/fixtures/
/// codex-hooks, anonymized). Session start and each prompt give Codex the session's dossier and the reminders as
/// context (Codex reads plain output and the JSON's additionalContext as Claude Code does); the tool, stop and end
/// hooks pass.
#[test]
fn the_legionctl_hooks_answer_codexs_own_payloads() {
    let (op, home) = operator("codex-hooks");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/codex-hooks");
    let session = "01a14ef6-5f77-7523-b576-8e1af9113d99";
    assert!(
        op.run(
            &home,
            &[("LEGIONCTL_SESSION", session)],
            &["dossier", "use", "codex-hooks"],
            ""
        )
        .status
        .success()
    );
    let hook = |event: &str, fixture: &str| -> (bool, String, String) {
        let mut payload: Value =
            serde_json::from_str(&std::fs::read_to_string(fixtures.join(fixture)).unwrap())
                .unwrap();
        payload["cwd"] = json!(home);
        let out = op.run(&home, &[], &["hook", event], &payload.to_string());
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    };
    // Codex takes a start or prompt hook's plain output as context, or the JSON's additionalContext, as Claude Code does
    let context = |stdout: &str| -> String {
        match serde_json::from_str::<Value>(stdout) {
            Ok(answer) => answer["hookSpecificOutput"]["additionalContext"]
                .as_str()
                .unwrap_or_else(|| panic!("{answer:#}"))
                .to_string(),
            Err(_) => stdout.to_string(),
        }
    };
    let (ok, start, err) = hook("session-start", "session-start.json");
    assert!(ok, "{err}");
    assert!(context(&start).contains("codex-hooks"), "{start}");
    let (ok, prompt, err) = hook("prompt", "prompt.json");
    assert!(ok, "{err}");
    assert!(
        context(&prompt).contains("legionctl:ste100-style"),
        "{prompt}"
    );
    for (event, fixture) in [
        ("pre-tool", "pre-tool-bash.json"),
        ("post-tool", "post-tool-bash.json"),
        ("stop", "stop.json"),
        ("session-end", "session-end.json"),
    ] {
        let (ok, _, err) = hook(event, fixture);
        assert!(ok, "{event}: {err}");
    }
}

/// A Codex run uses legionctl's tools (todo-205, live 2026-10-18: Codex answered "Unavailable" to a load call): Codex
/// starts MCP servers with a cleared environment, so legionctl's server gets the session's operator variables in its
/// own `env` at session/new. The stand-in starts the server as Codex does (only PATH and that `env`) and calls its
/// load tool, which answers.
#[test]
fn a_codex_run_reaches_legionctls_tools_with_the_cleared_environment_codex_gives_mcp_servers() {
    let (op, home) = operator("codex-mcp");
    let repo = repository(&home);
    adapter::executable(
        &home.join(".local/bin/fake-acp"),
        &adapter::node_script(&home),
    );
    write(
        &home.join(".config/legionctl/run.jsonc"),
        &json!({"session": {"acp": {"codex": adapter::COMMAND}, "scope": false, "writable": []}})
            .to_string(),
    );
    let out = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &[
            "session",
            "run",
            "codex",
            "MCPLOAD",
            "--dossier",
            "codex-mcp",
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
    assert!(
        text.starts_with("mcp load: ") && !text.contains("mcp error"),
        "{text}"
    );
}
