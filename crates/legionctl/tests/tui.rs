//! End-to-end: the thread screen (`legionctl tui`, design 23, Part C, P7) in a terminal (a pty) against the real
//! operator, a real thread and the stand-in ACP agent (tests/fixtures/fake-acp-agent.py) as the chat bar's agent. A
//! small terminal emulator in the driver keeps the screen as a grid of cells, so the test reads the place of each
//! pane and of the chat bar, not only the text.

mod common;

use std::collections::HashMap;
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

fn git(home: &Path, dir: &Path, args: &[&str]) {
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
}

/// The operator with its own HOME (workspaces under it, the stand-in agent as `fake` of config session.acp) and a
/// repository with one commit on main.
fn lab(tag: &str) -> (Operator, PathBuf, PathBuf) {
    let home = Scratch::new(format!(
        "/var/tmp/legionctl-tui-{tag}-{}",
        std::process::id()
    ));
    write(
        &home.join(".gitconfig"),
        "[user]\n  name = Test\n  email = test@example.com\n[init]\n  defaultBranch = main\n",
    );
    let agent =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-acp-agent.py");
    write(
        &home.join(".config/legionctl/tui.jsonc"),
        &json!({
            "workspace": {"root": home.join("worktrees")},
            "session": {"acp": {"fake": ["python3", agent]}, "scope": false, "writable": []}
        })
        .to_string(),
    );
    let repo = home.join("repo");
    write(&repo.join("README.md"), "repo\n");
    git(&home, &repo, &["init", "--quiet"]);
    git(&home, &repo, &["add", "."]);
    git(&home, &repo, &["commit", "--quiet", "-m", "start"]);
    let bin = Path::new(env!("CARGO_BIN_EXE_legionctl"))
        .parent()
        .unwrap()
        .to_path_buf();
    let path: OsString = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    std::fs::create_dir_all(home.join("root")).unwrap();
    let plugin = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../integrations/agent-plugin");
    let mut op = Operator::start(
        &format!("tui-{tag}"),
        &[
            ("LEGIONCTL_SYSROOT", home.join("root").into()),
            ("HOME", home.as_os_str().to_owned()),
            ("PATH", path),
            ("LEGIONCTL_PLUGIN", plugin.into()),
        ],
    );
    let path = home.to_path_buf();
    op.held.push(home);
    (op, path, repo)
}

/// Runs `legionctl <args>` in a 200x50 pty and does each step in order: wait until the screen shows `wait`, run the
/// command `run`, keep the screen as `snap`, and type `keys`. Returns the kept screens (rows of cells).
const DRIVER: &str = r#"
import codecs, fcntl, json, os, pty, re, select, struct, subprocess, sys, termios, time
W, H = 200, 50
pid, fd = pty.fork()
if pid == 0:
    os.execv(sys.argv[1], sys.argv[1:])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", H, W, 0, 0))
grid = [[" "] * W for _ in range(H)]
cy = cx = 0
pending = ""
decode = codecs.getincrementaldecoder("utf-8")("replace").decode
def feed(data):
    global cy, cx, pending
    text = pending + data
    i = 0
    while i < len(text):
        c = text[i]
        if c == "\x1b":
            m = re.match(r"\x1b\[([0-9;?]*)([A-Za-z@])", text[i:])
            if not m:
                if re.fullmatch(r"\x1b(\[[0-9;?]*)?", text[i:]):
                    break
                i += 2
                continue
            nums = [int(n) if n else 0 for n in m.group(1).lstrip("?").split(";")]
            cmd = m.group(2)
            if cmd in "Hf":
                cy, cx = max(nums[0], 1) - 1, max(nums[1] if len(nums) > 1 else 1, 1) - 1
            elif cmd == "J" and nums[0] == 2:
                for row in grid:
                    row[:] = [" "] * W
            elif cmd == "K":
                grid[cy][cx:] = [" "] * (W - cx)
            elif cmd == "C":
                cx += max(nums[0], 1)
            i += m.end()
        elif c == "\r":
            cx, i = 0, i + 1
        elif c == "\n":
            cy, i = cy + 1, i + 1
        elif c < " ":
            i += 1
        else:
            if 0 <= cy < H and 0 <= cx < W:
                grid[cy][cx] = c
            cx, i = cx + 1, i + 1
    pending = text[i:]
def rows():
    return ["".join(r) for r in grid]
def read(seconds):
    end = time.time() + seconds
    while time.time() < end:
        r, _, _ = select.select([fd], [], [], 0.05)
        if r:
            try:
                feed(decode(os.read(fd, 65536)))
            except OSError:
                return
def until(text, limit=60):
    end = time.time() + limit
    while text not in "\n".join(rows()):
        if time.time() > end:
            sys.stdout.write(json.dumps({"timeout": text, "screen": rows()}))
            sys.exit(1)
        read(0.1)
kept = {}
for step in json.loads(os.environ["STEPS"]):
    until(step.get("wait", ""))
    if "run" in step:
        subprocess.run(step["run"], check=True, capture_output=True)
    read(0.4)
    if "snap" in step:
        kept[step["snap"]] = rows()
    if "keys" in step:
        os.write(fd, step["keys"].encode())
end = time.time() + 20
while os.waitpid(pid, os.WNOHANG) == (0, 0):
    if time.time() > end:
        os.kill(pid, 9)
        sys.stdout.write(json.dumps({"timeout": "the end of the screen", "screen": rows()}))
        sys.exit(1)
    read(0.1)
sys.stdout.write(json.dumps(kept))
"#;

fn screens(
    op: &Operator,
    home: &Path,
    cwd: &Path,
    args: &[&str],
    steps: &Value,
) -> HashMap<String, Vec<Vec<char>>> {
    let out = Command::new("python3")
        .args(["-c", DRIVER, env!("CARGO_BIN_EXE_legionctl")])
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .env("XDG_RUNTIME_DIR", &op.runtime)
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("HOME", home)
        .env("TERM", "xterm")
        .env("STEPS", steps.to_string())
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{}{}",
        text.replace("\", \"", "\n"),
        String::from_utf8_lossy(&out.stderr)
    );
    let kept: HashMap<String, Vec<String>> = serde_json::from_str(&text).unwrap();
    kept.into_iter()
        .map(|(k, rows)| (k, rows.iter().map(|r| r.chars().collect()).collect()))
        .collect()
}

fn text(screen: &[Vec<char>]) -> String {
    screen
        .iter()
        .map(|r| r.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// The columns of `c` in row `y`.
fn columns(screen: &[Vec<char>], y: usize, c: char) -> Vec<usize> {
    (0..screen[y].len())
        .filter(|&x| screen[y][x] == c)
        .collect()
}

/// The chat bar: (left column, width, height), from its title row down to its bottom corner.
fn chat_bar(screen: &[Vec<char>]) -> (usize, usize, usize) {
    let top = screen
        .iter()
        .position(|r| r.iter().collect::<String>().contains(" chat → fake"))
        .unwrap_or_else(|| panic!("no chat bar:\n{}", text(screen)));
    let left = columns(screen, top, '┌')
        .into_iter()
        .rfind(|&x| x > 74 - 1)
        .unwrap();
    let right = (left + 1..screen[top].len())
        .find(|&x| screen[top][x] == '┐')
        .unwrap();
    let bottom = (top + 1..screen.len())
        .find(|&y| screen[y][left] == '└')
        .unwrap();
    (left, right - left + 1, bottom - top + 1)
}

/// The text of pane 2 (columns 14 to 73 at 200 columns).
fn pane2(screen: &[Vec<char>]) -> String {
    screen
        .iter()
        .map(|r| r[14..74].iter().collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `legionctl <args>` of the session `session` in the repository: (success, stdout and stderr).
fn cli(op: &Operator, home: &Path, repo: &Path, session: &str, args: &[&str]) -> (bool, String) {
    let out = op.run(
        repo,
        &[
            ("HOME", home.to_str().unwrap()),
            ("LEGIONCTL_SESSION", session),
        ],
        args,
        "",
    );
    (
        out.status.success(),
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr),
    )
}

/// [`lab`] with thread 1 on dossier port: main0 (session `main`), observer0 (session `obs`) and a note to observer0.
fn thread_lab(tag: &str) -> (Operator, PathBuf, PathBuf) {
    let (op, home, repo) = lab(tag);
    for args in [
        &["dossier", "use", "port"][..],
        &["thread", "start", "port the parser"],
        &[
            "thread",
            "join",
            "1",
            "observer0",
            "thread-observer",
            "--member-session",
            "obs",
        ],
        &["thread", "send", "observer0", "note", "check the lint"],
    ] {
        let (ok, out) = cli(&op, &home, &repo, "main", args);
        assert!(ok, "{args:?}: {out}");
    }
    (op, home, repo)
}

const SCREEN: [&str; 7] = [
    "tui",
    "--thread",
    "1",
    "--agent",
    "fake",
    "--dossier",
    "chat",
];

/// The thread screen at 200x50: panes of 7% (14 columns), 30% (60) and the rest (126); pane 2 shows the thread's log
/// and a note sent while the screen runs. The chat bar starts at 60% of pane 3, centred at its bottom, 3 rows high;
/// three lines make it 5 rows; Ctrl+Right makes it 70%. Alt+1 and Alt+2 fold panes 1 and 2 to one column each with
/// their label from top to bottom, and pane 3 takes the rest of the width.
#[test]
fn the_thread_tui_sizes_its_panes_folds_them_and_grows_the_chat_bar() {
    let (op, home, repo) = thread_lab("panes");
    let legionctl = env!("CARGO_BIN_EXE_legionctl");
    let steps = json!([
        {"wait": "check the lint", "snap": "start",
         "run": ["env", "LEGIONCTL_SESSION=main", legionctl, "thread", "send", "observer0", "note", "run the tests"]},
        {"wait": "run the tests", "snap": "live", "keys": "\t"},
        {"wait": "Enter send", "snap": "empty", "keys": "line one\u{1b}\rline two\u{1b}\rline three"},
        {"wait": "line three", "snap": "tall", "keys": "\u{1b}[1;5C"},
        {"snap": "wide", "keys": "\u{1b}"},
        {"keys": "\u{1b}1"},
        {"keys": "\u{1b}2"},
        {"snap": "folded", "keys": "q"},
    ]);
    let shots = screens(&op, &home, &repo, &SCREEN, &steps);
    let start = &shots["start"];
    assert_eq!(
        columns(start, 0, '┌'),
        [0, 14, 74],
        "pane corners:\n{}",
        text(start)
    );
    assert_eq!(
        columns(start, 0, '┐'),
        [13, 73, 199],
        "pane ends:\n{}",
        text(start)
    );
    assert!(
        pane2(start).contains("main0 message → observer0: check the lint"),
        "the thread's log in pane 2:\n{}",
        text(start)
    );
    assert!(
        pane2(&shots["live"]).contains("main0 message → observer0: run the tests"),
        "a note sent while the screen runs:\n{}",
        text(&shots["live"])
    );
    // 60% of pane 3 (126 columns), centred; 3 rows; 5 rows with three lines; 70% after Ctrl+Right
    assert_eq!(
        chat_bar(&shots["empty"]),
        (99, 75, 3),
        "{}",
        text(&shots["empty"])
    );
    let tall = &shots["tall"];
    assert_eq!(chat_bar(tall), (99, 75, 5), "{}", text(tall));
    let lines: Vec<usize> = ["line one", "line two", "line three"]
        .iter()
        .map(|l| {
            tall.iter()
                .position(|r| r.iter().collect::<String>().contains(l))
                .unwrap()
        })
        .collect();
    assert_eq!(
        lines,
        [lines[0], lines[0] + 1, lines[0] + 2],
        "{}",
        text(tall)
    );
    assert_eq!(
        chat_bar(&shots["wide"]),
        (93, 88, 5),
        "{}",
        text(&shots["wide"])
    );
    let folded = &shots["folded"];
    let column = |x: usize| folded.iter().map(|r| r[x]).collect::<String>();
    assert!(
        column(0).starts_with("1views") && column(1).starts_with("2events"),
        "folded panes:\n{}",
        text(folded)
    );
    assert_eq!(columns(folded, 0, '┌'), [2], "{}", text(folded));
    assert_eq!(columns(folded, 0, '┐'), [199], "{}", text(folded));
}

/// The chat bar with the stand-in agent: text is an intent (`ask`, DESIGN-09 G6): no stored view answers hello, so the
/// agent composes in a headless run (its request has the intent) and the screen says that no view came; its agent's
/// `available_commands_update` gives `/agent:plan` in the completion of `/`, marked as the agent's and before the
/// screen's commands and the registry's operations; `/agent:plan x` reaches the agent as the prompt `/plan x` in the
/// composer's ACP session (loaded, not new), as the TUI's call; a registry command (`/thread show 1`) runs with the
/// CLI's words; pane 2 lists the runs; the intent of a stored view opens it. `session commands` gives the agent's
/// commands, and refuses a session that does not exist.
#[test]
fn the_thread_tui_runs_an_agent_command_and_a_registry_operation() {
    let (op, home, repo) = thread_lab("chat");
    let steps = json!([
        {"wait": "check the lint", "keys": "\thello\r"},
        {"wait": "no view (composer", "snap": "answer", "keys": "/"},
        {"wait": "/agent:plan", "snap": "complete", "keys": "agent:plan x\r"},
        {"wait": "prompt: /plan x", "snap": "command", "keys": "/thread show 1\r"},
        {"wait": "dossier port): port the parser", "snap": "registry", "keys": "everything legionctl can do\r"},
        {"wait": "Operations · version 0", "snap": "intent", "keys": "\u{1b}"},
        {"keys": "q"},
    ]);
    let shots = screens(&op, &home, &repo, &SCREEN, &steps);
    let answer = text(&shots["answer"]);
    assert!(answer.contains("Intent: hello"), "{answer}");
    let complete = text(&shots["complete"]);
    let at = |entry: &str| {
        complete
            .find(entry)
            .unwrap_or_else(|| panic!("{entry} not offered:\n{complete}"))
    };
    assert!(
        at("/agent:plan  Plan a change <what to plan> (agent)") < at("/fold 1  Fold"),
        "the agent's commands first, then the screen's and the registry's:\n{complete}"
    );
    let command = text(&shots["command"]);
    assert!(
        command.contains("prompt: /plan x") && command.contains("acp: loaded "),
        "the agent's command as the prompt /plan x, in the composer's ACP session:\n{command}"
    );
    let session = command
        .split("session ")
        .filter_map(|rest| rest.split_whitespace().next())
        .find(|id| id.len() == 36)
        .unwrap();
    let registry = &shots["registry"];
    assert!(
        pane2(registry).contains("✓ /agent:plan x") && pane2(registry).contains("✓ /thread show 1"),
        "the runs in pane 2:\n{}",
        text(registry)
    );
    // a stored view's intent opens that view in pane 3, from the store
    let intent = text(&shots["intent"]);
    assert!(
        intent.contains("operations") && intent.contains("rows: Operation"),
        "{intent}"
    );
    let (ok, listed) = cli(&op, &home, &repo, "main", &["session", "commands", session]);
    assert!(
        ok && listed.contains("/plan <what to plan>  Plan a change"),
        "{listed}"
    );
    let (ok, refused) = cli(&op, &home, &repo, "main", &["session", "commands", "nope"]);
    assert!(!ok && refused.contains("no session nope"), "{refused}");
    let events: Value =
        serde_json::from_str(&op.ok(&["events", "--limit", "1000", "--json"])).unwrap();
    assert!(
        events["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "session.run"
                && e["actor"] == "tui"
                && e["payload"]["input"]["prompt"] == "/plan x"),
        "{events}"
    );
}
