//! The desktop app against the real operator on the recorded machine (unplugged, AC policy still applied, no
//! daemon): it shows the machine, previews the policy change, reports a refused apply, and shows doctor's findings.
//! Real clicks through GPUI's headless window; the operator answers in real time.

mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::Operator;
use common::scratch::Scratch;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext, Bounds, Entity, TestAppContext, VisualTestContext, WindowBounds, WindowHandle,
    WindowOptions, point, px, size,
};
use legionctl_api::client::Client;
use legionctl_app::{LegionApp, Page, Pending};

const FIXTURE: &str = "../../tests/fixtures/sysroot-82wq-2026-10-12";
const PROFILE: &str = "../../profiles/lenovo-82wq-legion-pro-7-16irx8h.json";
const LAYERS: &str = "../../configsets/lenovo-82wq-legion-pro-7-16irx8h";

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap().flatten() {
        let target = to.join(entry.file_name());
        let kind = entry.file_type().unwrap();
        if kind.is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(entry.path()).unwrap(), &target).unwrap();
        } else if kind.is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Run the GPUI executor until `ready` holds, in real time (the operator works outside the test clock).
fn wait(
    cx: &mut TestAppContext,
    app: &Entity<LegionApp>,
    what: &str,
    ready: impl Fn(&LegionApp) -> bool,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        cx.run_until_parked();
        if app.read_with(cx, |a, _| ready(a)) {
            return;
        }
        let state = app.read_with(cx, |a, _| format!("{:?}\n{:?}", a.state, a.screen));
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {state}"
        );
        // the thread screen takes the stream's events on a timer of the test clock
        cx.executor().advance_clock(Duration::from_millis(100));
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// A copy of the recorded machine at `root`, with its installed profile and configset.
fn recorded_machine(root: &Path) {
    copy_tree(&Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE), root);
    let installed = root.join("usr/share/legionctl/profiles/lenovo-82wq-legion-pro-7-16irx8h.json");
    std::fs::create_dir_all(installed.parent().unwrap()).unwrap();
    std::fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join(PROFILE),
        installed,
    )
    .unwrap();
    copy_tree(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join(LAYERS),
        &root.join("usr/share/legionctl/configsets/lenovo-82wq-legion-pro-7-16irx8h"),
    );
}

/// The operator on a copy of the recorded machine, unplugged with the AC policy still applied, without a daemon; `tag`
/// keeps the folders of tests that run at the same time apart.
fn unplugged_machine(tag: &'static str) -> Operator {
    let dir =
        Scratch::new(std::env::temp_dir().join(format!("legionctl-{tag}-{}", std::process::id())));
    let root = dir.join("root");
    recorded_machine(&root);
    std::fs::write(root.join("sys/class/power_supply/ADP0/online"), "0\n").unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let mut op = Operator::start(
        tag,
        &[
            ("PATH", bin.into()),
            ("HOME", dir.join("home").into()),
            ("LEGIONCTL_SYSROOT", root.into()),
            // never this machine's Docker (its WinBoat): the pages list containers and run their actions
        ],
    );
    op.held.push(dir);
    op
}

#[gpui_kit::test]
fn the_app_shows_the_machine_previews_the_policy_reports_a_refused_apply_and_lists_findings(
    cx: &mut TestAppContext,
) {
    let op = unplugged_machine("app");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = Client::at("app", &op.runtime);
    let handle = runtime.handle().clone();
    cx.update(gpui_kit::init);
    let window: WindowHandle<gpui_kit::base::Root> = cx.update(|cx| {
        let (window, _) = gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
            cx.new(|cx| LegionApp::new(client, handle, Page::Now, cx))
        })
        .unwrap();
        window.downcast().unwrap()
    });
    let app: Entity<LegionApp> = window
        .update(cx, |root, _, _| root.view().clone().downcast().unwrap())
        .unwrap();
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    let cx = &mut vcx;

    // the glance: unplugged, firmware still in the AC mode
    wait(cx, &app, "the status", |a| a.state.status.is_some());
    app.read_with(cx, |a, _| {
        let status = a.state.status.as_ref().unwrap();
        assert_eq!(status.power_source, "battery");
        assert_eq!(status.platform_profile.as_deref(), Some("max-power"));
    });

    // preview: the battery policy's writes, shown before anything changes
    cx.update(|window, cx| window.click("preview-policy", cx));
    wait(cx, &app, "the policy preview", |a| {
        a.state.pending.is_some()
    });
    app.read_with(cx, |a, _| {
        let Some(Pending::Policy(changes)) = &a.state.pending else {
            panic!("{:?}", a.state.pending)
        };
        assert!(changes.dry_run);
        assert!(
            changes
                .lines
                .iter()
                .any(|l| l.contains("platform-profile-0/profile: balanced")),
            "{changes:?}"
        );
        // one line for the governor of all 32 CPUs, not 32 lines
        let governor = "would write /sys/devices/system/cpu/cpufreq/policy*/scaling_governor: powersave (32 CPUs)";
        assert!(changes.lines.iter().any(|l| l == governor), "{changes:?}");
    });

    // apply: no daemon on this bus, so the operator refuses; the app says why and keeps running
    cx.update(|window, cx| window.click("apply", cx));
    wait(cx, &app, "the apply result", |a| {
        a.state.message.is_some() && !a.state.busy
    });
    app.read_with(cx, |a, _| {
        assert!(a.state.pending.is_none());
        let message = a.state.message.as_deref().unwrap();
        assert!(message.contains("system bus"), "{message}");
    });

    // the Health tab: doctor's findings for this machine
    cx.update(|window, cx| window.within("pages").click(2usize, cx));
    wait(cx, &app, "the findings", |a| a.state.findings.is_some());
    app.read_with(cx, |a, _| {
        assert_eq!(a.page, Page::Health);
        let findings = a.state.findings.as_ref().unwrap();
        assert!(
            findings
                .iter()
                .any(|f| f == "platform_profile is 'max-power', expected 'balanced'"),
            "{findings:?}"
        );
    });
    // the operator's event log names the app as the actor of the refused change
    let events = op.ok(&["events"]);
    assert!(
        events
            .lines()
            .any(|l| l.contains(" app ") && l.contains("power.apply")),
        "{events}"
    );

    // the Bench tab: every run in the log, also past its first page (1000), so the newest is there
    let db = std::env::temp_dir().join(format!(
        "legionctl-app-{}/home/.local/state/legionctl/operator.db",
        std::process::id()
    ));
    let runs = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1001) \
                INSERT INTO events (ts, actor, kind, payload) SELECT 1700000000 + i, 'cli', 'bench.result', \
                json_object('scenario', 'cpu', 'label', 'run-' || i, 'seconds', 5, 'scores', json_object(), \
                'problems', json_array(), 'summary', json_object('samples', 1, 'throttle', json_array()), \
                'knobs', json_object()) FROM n";
    let stored = std::process::Command::new("sqlite3")
        .arg(&db)
        .arg(runs)
        .status()
        .unwrap();
    assert!(stored.success());
    cx.update(|window, cx| window.within("pages").click(3usize, cx));
    wait(cx, &app, "the runs", |a| a.state.runs.is_some());
    app.read_with(cx, |a, _| {
        assert_eq!(a.page, Page::Bench);
        let runs = a.state.runs.as_ref().unwrap();
        assert_eq!(runs.len(), 1001);
        assert_eq!(runs[1000].result.label.as_deref(), Some("run-1001"));
    });
    drop(runtime);
}

/// The Resources page (note 22) against the real operator on the recorded machine, without a daemon: it lists the
/// configset layers with their actions; a click on an action shows its route first (rules.disable has no dry run of
/// its own, so nothing runs), and Apply runs the routed operation as a call of its own: the operator refuses it
/// without the system bus, the page says why, and the event log names `rules.disable` with the app as its actor.
#[gpui_kit::test]
fn the_resources_page_lists_the_resources_and_an_action_click_calls_its_operation(
    cx: &mut TestAppContext,
) {
    let op = unplugged_machine("resources");
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = Client::at("app", &op.runtime);
    let handle = runtime.handle().clone();
    cx.update(gpui_kit::init);
    let window: WindowHandle<gpui_kit::base::Root> = cx.update(|cx| {
        let (window, _) = gpui_kit::open_window(WindowOptions::default(), cx, |_, cx| {
            cx.new(|cx| LegionApp::new(client, handle, Page::Resources, cx))
        })
        .unwrap();
        window.downcast().unwrap()
    });
    let app: Entity<LegionApp> = window
        .update(cx, |root, _, _| root.view().clone().downcast().unwrap())
        .unwrap();
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    let cx = &mut vcx;

    wait(cx, &app, "the resources", |a| a.state.resources.is_some());
    app.read_with(cx, |a, _| {
        let list = &a.state.resources.as_ref().unwrap().resources;
        let ac = list
            .iter()
            .find(|r| r.id == "layer:shipped/20-ac")
            .unwrap_or_else(|| panic!("{list:?}"));
        assert_eq!(ac.group, "configset");
        let actions: Vec<(&str, &str)> = ac
            .actions
            .iter()
            .map(|a| (a.name.as_str(), a.op.as_str()))
            .collect();
        assert_eq!(actions, [("disable", "rules.disable")]);
    });

    cx.update(|window, cx| window.click("layer:shipped/20-ac/disable", cx));
    wait(cx, &app, "the route", |a| a.state.pending.is_some());
    app.read_with(cx, |a, _| {
        let Some(Pending::Resource(id, action, acted)) = &a.state.pending else {
            panic!("{:?}", a.state.pending)
        };
        assert_eq!(
            (id.as_str(), action.as_str()),
            ("layer:shipped/20-ac", "disable")
        );
        assert_eq!(acted.op, "rules.disable");
        assert_eq!(acted.input, serde_json::json!({"layer": "shipped/20-ac"}));
        assert!(
            !acted.run && acted.output.is_none(),
            "a dry run runs nothing: {acted:?}"
        );
    });
    let before = op.ok(&["events"]);
    assert!(!before.contains("rules.disable"), "{before}");

    cx.update(|window, cx| window.click("apply", cx));
    wait(cx, &app, "the apply result", |a| {
        a.state.message.is_some() && !a.state.busy
    });
    app.read_with(cx, |a, _| {
        let message = a.state.message.as_deref().unwrap();
        assert!(message.contains("system bus"), "{message}");
    });
    let events = op.ok(&["events"]);
    assert!(
        events
            .lines()
            .any(|l| l.contains(" app ") && l.contains("rules.disable")),
        "{events}"
    );
    drop(runtime);
}

/// The TUI's resources view (note 22) in a terminal (a pty) against the real operator on the recorded machine, without
/// a daemon or Docker: `r` shows the list (the desktop GPU, then the layers), Down twice selects the layer 20-ac, `1`
/// shows its first action's route (rules.disable has no dry run of its own, so nothing runs), and Enter runs the
/// routed operation as a call of its own: the operator refuses it without the system bus, and the event log names it
/// with the TUI as its actor.
#[test]
fn the_tui_resources_view_lists_the_resources_and_a_key_runs_the_route() {
    let op = unplugged_machine("tui");
    // keys in order, each after the text it waits for; prints the screen text without escape codes
    let driver = r#"
import os, pty, re, select, struct, sys, fcntl, termios, time
pid, fd = pty.fork()
if pid == 0:
    os.execv(sys.argv[1], [sys.argv[1], "tui"])
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", 50, 220, 0, 0))
seen = ""
def until(text, limit=20):
    global seen
    end = time.time() + limit
    while text not in seen:
        if time.time() > end:
            sys.stdout.write(seen); sys.exit("timed out waiting for " + text)
        r, _, _ = select.select([fd], [], [], 0.2)
        if r:
            try:
                seen += re.sub(r"\x1b\[[0-9;?]*[A-Za-z]", " ", os.read(fd, 65536).decode("utf-8", "replace"))
            except OSError:
                break
for wait, key in [("r resources", b"r"), ("used by", b"\x1b[B"), ("", b"\x1b[B"), ("", b"1"),
                  ("Enter applies it", b"\r"), ("system bus", b"q")]:
    until(wait)
    os.write(fd, key)
os.waitpid(pid, 0)
sys.stdout.write(seen)
"#;
    let out = std::process::Command::new("python3")
        .args(["-c", driver, env!("CARGO_BIN_EXE_legionctl")])
        .env_clear()
        .env("XDG_RUNTIME_DIR", &op.runtime)
        .env("PATH", std::env::var_os("PATH").unwrap())
        .env("TERM", "xterm")
        .output()
        .unwrap();
    let screen = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success(),
        "{screen}{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(screen.contains("layer:shipped/20-ac"), "the list: {screen}");
    assert!(
        screen.contains(r#"disable layer:shipped/20-ac: dry run: would run rules.disable {"layer":"shipped/20-ac"}"#),
        "the route of the selected row: {screen}"
    );
    let events = op.ok(&["events"]);
    assert!(
        events
            .lines()
            .any(|l| l.contains(" tui ") && l.contains("rules.disable")),
        "rules.disable by the TUI: {events}"
    );
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// The operator on the recorded machine with its own HOME (the stand-in ACP agent as `fake` of config session.acp),
/// a repository, and
/// thread 1 on dossier port: main0 (session `main`) and observer0 (session `obs`). Returns the operator, HOME and
/// the repository.
fn thread_lab(tag: &str) -> (Operator, PathBuf, PathBuf) {
    let home = Scratch::new(format!(
        "/var/tmp/legionctl-app-{tag}-{}",
        std::process::id()
    ));
    write(
        &home.join(".gitconfig"),
        "[user]\n  name = Test\n  email = test@example.com\n[init]\n  defaultBranch = main\n",
    );
    let agent =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/fake-acp-agent.py");
    write(
        &home.join(".config/legionctl/app.jsonc"),
        &serde_json::json!({
            "workspace": {"root": home.join("worktrees")},
            "session": {"acp": {"fake": ["python3", agent]}, "scope": false, "writable": []}
        })
        .to_string(),
    );
    let repo = home.join("repo");
    write(&repo.join("README.md"), "repo\n");
    for args in [
        &["init", "--quiet"][..],
        &["add", "."],
        &["commit", "--quiet", "-m", "start"],
    ] {
        let out = std::process::Command::new("git")
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
    let bin = Path::new(env!("CARGO_BIN_EXE_legionctl"))
        .parent()
        .unwrap()
        .to_path_buf();
    let path = std::env::join_paths(
        std::iter::once(bin).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    recorded_machine(&home.join("root"));
    let mut op = Operator::start(
        &format!("ap-{tag}"),
        &[
            ("LEGIONCTL_SYSROOT", home.join("root").into()),
            ("HOME", home.as_os_str().to_owned()),
            ("PATH", path),
        ],
    );
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
    ] {
        lab_cli(&op, &home, &repo, args, "");
    }
    let path = home.to_path_buf();
    op.held.push(home);
    (op, path, repo)
}

/// stdout of `legionctl <args>` of the session `main` in the repository, with `stdin`; it must succeed.
fn lab_cli(op: &Operator, home: &Path, repo: &Path, args: &[&str], stdin: &str) -> String {
    let out = op.run(
        repo,
        &[
            ("HOME", home.to_str().unwrap()),
            ("LEGIONCTL_SESSION", "main"),
        ],
        args,
        stdin,
    );
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
}

/// The app in a headless window of `width` x 700 against `op`, and its entity.
fn open_app(
    cx: &mut TestAppContext,
    op: &Operator,
    width: f32,
    agent: Option<(&Path, &Path)>,
) -> (
    Entity<LegionApp>,
    VisualTestContext,
    tokio::runtime::Runtime,
) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = Client::at("app", &op.runtime);
    let handle = runtime.handle().clone();
    let agent = agent.map(|(home, repo)| (home.to_path_buf(), repo.to_path_buf()));
    cx.update(gpui_kit::init);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(width), px(700.)),
        })),
        ..WindowOptions::default()
    };
    let window: WindowHandle<gpui_kit::base::Root> = cx.update(|cx| {
        let (window, _) = gpui_kit::open_window(options, cx, |_, cx| {
            cx.new(|cx| {
                let app = LegionApp::new(client, handle, Page::Now, cx);
                match agent {
                    Some((_, repo)) => app.with_agent("fake".into(), Some("chat".into()), repo),
                    None => app,
                }
            })
        })
        .unwrap();
        window.downcast().unwrap()
    });
    let app: Entity<LegionApp> = window
        .update(cx, |root, _, _| root.view().clone().downcast().unwrap())
        .unwrap();
    (
        app,
        VisualTestContext::from_window(window.into(), cx),
        runtime,
    )
}

/// The bounds of the element `id` in the last frame: (x, width, height).
fn place(cx: &mut VisualTestContext, id: &'static str) -> (f32, f32, f32) {
    cx.update(|window, cx| {
        window.render_frame(cx);
        let b = window.find(id).bounds();
        (
            f32::from(b.origin.x),
            f32::from(b.size.width),
            f32::from(b.size.height),
        )
    })
}

fn near(got: f32, want: f32, what: &str) {
    assert!((got - want).abs() <= 1.0, "{what}: {got}, not {want}");
}

/// The thread screen of the app at 1000 px: pane 1 is 7% (70 px), pane 2 30% (300 px), pane 3 the rest (630 px).
/// Ctrl+1 folds pane 1 to its 24 px rail and pane 3 takes the space; a click on the rail opens it; the fold button
/// of pane 2 folds it. The chat bar is at the bottom centre of pane 3, 60% of its width; three lines make it taller,
/// many lines stop at 40% of the pane; Ctrl+Right makes it 70% wide. Pane 2 shows the thread's log.
#[gpui_kit::test]
fn the_app_thread_screen_sizes_its_panes_folds_them_and_grows_the_chat_bar(
    cx: &mut TestAppContext,
) {
    let (op, _home, _repo) = thread_lab("panes");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1000., None);
    let cx = &mut vcx;
    wait(cx, &app, "the thread", |a| a.screen.thread.is_some());
    app.read_with(cx, |a, _| {
        let t = a.screen.thread.as_ref().unwrap();
        assert_eq!(t.thread.title, "port the parser");
        let lines: Vec<String> = t.log.iter().map(legionctl_api::chat::log_line).collect();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("observer0 join thread-observer")),
            "{lines:?}"
        );
    });

    let (x1, w1, _) = place(cx, "pane-1");
    let (x2, w2, _) = place(cx, "pane-2");
    let (x3, w3, h3) = place(cx, "pane-3");
    near(x1, 0., "pane 1 starts at the left");
    near(w1, 70., "pane 1 is 7%");
    near(x2, 70., "pane 2 follows pane 1");
    near(w2, 300., "pane 2 is 30%");
    near(x3, 370., "pane 3 follows pane 2");
    near(w3, 630., "pane 3 is the rest");

    // the chat bar: 60% of pane 3, centred, one line
    cx.update(|window, cx| window.click("chat", cx));
    let (xc, wc, hc1) = place(cx, "chat");
    near(wc, 378., "the chat bar is 60% of pane 3");
    near(
        xc,
        370. + (630. - 378.) / 2.,
        "the chat bar is centred in pane 3",
    );
    cx.update(|window, cx| {
        window.input("one", cx);
        window.press("shift-enter", cx);
        window.input("two", cx);
        window.press("shift-enter", cx);
        window.input("three", cx);
    });
    app.read_with(cx, |a, _| assert_eq!(a.screen.chat, "one\ntwo\nthree"));
    let (_, _, hc3) = place(cx, "chat");
    assert!(
        hc3 > hc1 + 20.,
        "three lines make the chat bar taller: {hc1} then {hc3}"
    );
    cx.update(|window, cx| {
        for _ in 0..60 {
            window.press("shift-enter", cx);
        }
    });
    let (_, _, hc_most) = place(cx, "chat");
    assert!(
        hc_most <= h3 * 0.4 + 1. && hc_most > hc3,
        "the chat bar stops at 40% of pane 3 ({h3}): {hc_most}"
    );
    cx.update(|window, cx| window.press("ctrl-right", cx));
    let (_, wc_wide, _) = place(cx, "chat");
    near(wc_wide, 441., "Ctrl+Right makes the chat bar 70% of pane 3");

    // fold pane 1 with Ctrl+1: a 24 px rail, and pane 3 takes the space
    cx.update(|window, cx| window.press("ctrl-1", cx));
    let (_, w1, _) = place(cx, "pane-1");
    let (x3, w3, _) = place(cx, "pane-3");
    near(w1, 24., "folded pane 1 is a 24 px rail");
    near(x3, 324., "pane 3 starts after the rail and pane 2");
    near(w3, 676., "pane 3 takes the width of pane 1");
    cx.update(|window, cx| window.click("pane-1", cx));
    near(
        place(cx, "pane-1").1,
        70.,
        "a click on the rail opens pane 1",
    );
    cx.update(|window, cx| window.click("fold-2", cx));
    near(place(cx, "pane-2").1, 24., "the fold button folds pane 2");
    near(
        place(cx, "pane-3").1,
        906.,
        "pane 3 takes the width of pane 2",
    );
    drop(runtime);
}

/// The chat bar with the stand-in ACP agent: a prompt runs it once (a headless run); its commands then come first
/// in the `/` completion, marked (agent), with the input hint; `/agent:plan x` reaches it as `/plan x`, and a registry
/// line runs with the CLI's words. Pane 2 lists both runs.
#[gpui_kit::test]
fn the_app_chat_bar_offers_the_agents_commands_first_and_runs_a_registry_line(
    cx: &mut TestAppContext,
) {
    let (op, home, repo) = thread_lab("chat");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    let cx = &mut vcx;
    cx.update(|window, cx| {
        window.click("chat", cx);
        window.input("hello", cx);
        window.press("enter", cx);
    });
    wait(cx, &app, "the first run and the agent's commands", |a| {
        !a.screen.commands.is_empty()
    });
    cx.update(|window, cx| window.input("/", cx));
    app.read_with(cx, |a, _| {
        let entries = a.screen.completions();
        assert_eq!(entries[0].text, "/agent:plan", "the agent's commands first");
        assert_eq!(entries[0].about, "Plan a change <what to plan> (agent)");
        assert!(entries[0].agent && entries[1].agent && !entries[2].agent);
    });
    assert!(cx.update(|window, _| window.find("completion-0").visible()));
    cx.update(|window, cx| {
        window.input("agent:plan x", cx);
        window.press("enter", cx);
    });
    wait(cx, &app, "the agent command's run", |a| {
        a.screen.runs.len() == 2 && a.screen.runs[1].1.is_some()
    });
    app.read_with(cx, |a, _| {
        let answer = a.state.message.clone().unwrap_or_default();
        assert!(answer.contains("prompt: /plan x"), "{answer}");
        assert!(
            answer.contains("acp: loaded"),
            "the same ACP session: {answer}"
        );
    });
    cx.update(|window, cx| {
        window.input("/thread show 1", cx);
        window.press("enter", cx);
    });
    wait(cx, &app, "the registry line", |a| {
        a.screen.runs.len() == 3 && a.screen.runs[2].1.is_some()
    });
    app.read_with(cx, |a, _| {
        let runs: Vec<_> = a
            .screen
            .runs
            .iter()
            .map(|(l, ok)| (l.as_str(), *ok))
            .collect();
        assert_eq!(
            runs,
            [
                ("hello", Some(true)),
                ("/agent:plan x", Some(true)),
                ("/thread show 1", Some(true))
            ]
        );
        let shown = a.state.message.clone().unwrap_or_default();
        assert!(
            shown.contains("thread 1 (open, dossier port): port the parser"),
            "{shown}"
        );
    });
    let events: serde_json::Value =
        serde_json::from_str(&op.ok(&["events", "--limit", "1000", "--json"])).unwrap();
    assert!(
        events["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "session.run"
                && e["actor"] == "app"
                && e["payload"]["input"]["prompt"] == "/plan x"),
        "the run by the app: {events}"
    );
    drop(runtime);
}

/// A view saved through `views save` and placed on the thread with `views place` shows in pane 3 with its widget:
/// a table of the thread's members. The screen follows the event stream: a new member (the view refreshes on
/// thread.*) and a changed view file (views.save) show without a poll.
#[gpui_kit::test]
fn the_app_thread_page_updates_from_the_event_stream(cx: &mut TestAppContext) {
    let (op, home, repo) = thread_lab("views");
    let view = r#"// the members of a thread
{
  "name": "members",
  "params": { "thread": "integer" },
  "widget": "table",
  "source": { "op": "thread.show", "input": { "thread": "{thread}" }, "path": "$.members" },
  "columns": ["name", "archetype", "pace"],
  "refresh": { "events": ["thread.*"] }
}"#;
    lab_cli(&op, &home, &repo, &["views", "save"], view);
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., None);
    let cx = &mut vcx;
    wait(cx, &app, "the thread", |a| a.screen.thread.is_some());
    lab_cli(
        &op,
        &home,
        &repo,
        &["views", "place", "1", "members", "thread=1"],
        "",
    );
    wait(cx, &app, "the placed view", |a| {
        a.screen
            .view_data
            .as_ref()
            .is_some_and(|v| v.rows.len() == 2)
    });
    app.read_with(cx, |a, _| {
        let v = a.screen.view_data.as_ref().unwrap();
        assert_eq!(v.columns, ["name", "archetype", "pace"]);
        assert_eq!(v.rows[0], ["main0", "main-agent", "on-trigger"]);
        assert_eq!(v.rows[1], ["observer0", "thread-observer", "on-trigger"]);
    });
    assert!(cx.update(|window, _| window.find("view-members-row-1").visible()));

    // a member joins: the event refreshes the view
    lab_cli(
        &op,
        &home,
        &repo,
        &[
            "thread",
            "join",
            "1",
            "observer1",
            "thread-observer",
            "--pace",
            "on-stop",
        ],
        "",
    );
    wait(cx, &app, "the new member", |a| {
        a.screen
            .view_data
            .as_ref()
            .is_some_and(|v| v.rows.len() == 3)
    });
    app.read_with(cx, |a, _| {
        let v = a.screen.view_data.as_ref().unwrap();
        assert_eq!(v.rows[2], ["observer1", "thread-observer", "on-stop"]);
    });

    // the view changes: its new columns show
    lab_cli(
        &op,
        &home,
        &repo,
        &["views", "save"],
        &view.replace(r#"["name", "archetype", "pace"]"#, r#"["name"]"#),
    );
    wait(cx, &app, "the changed view", |a| {
        a.screen
            .view_data
            .as_ref()
            .is_some_and(|v| v.columns == ["name"])
    });
    app.read_with(cx, |a, _| {
        let v = a.screen.view_data.as_ref().unwrap();
        assert_eq!(v.rows, [["main0"], ["observer0"], ["observer1"]]);
        assert!(a.screen.events >= 2, "{}", a.screen.events);
    });
    drop(runtime);
}
