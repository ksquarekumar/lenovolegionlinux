//! The desktop app against the real operator on the recorded machine (unplugged, AC policy still applied, no
//! daemon): it shows the machine, previews the policy change, reports a refused apply, and shows doctor's findings.
//! Real clicks through GPUI's headless window; the operator answers in real time.

mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::Operator;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext, Entity, TestAppContext, VisualTestContext, WindowHandle, WindowOptions,
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
        let state = app.read_with(cx, |a, _| format!("{:?}", a.state));
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {what}: {state}"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// The operator on a copy of the recorded machine, unplugged with the AC policy still applied, without a daemon; `tag`
/// keeps the folders of tests that run at the same time apart.
fn unplugged_machine(tag: &'static str) -> Operator {
    let dir = std::env::temp_dir().join(format!("legionctl-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let root = dir.join("root");
    copy_tree(&Path::new(env!("CARGO_MANIFEST_DIR")).join(FIXTURE), &root);
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
    std::fs::write(root.join("sys/class/power_supply/ADP0/online"), "0\n").unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    Operator::start(
        tag,
        &[
            ("PATH", bin.into()),
            ("HOME", dir.join("home").into()),
            ("LEGIONCTL_SYSROOT", root.into()),
            // never this machine's Docker (its WinBoat): the pages list containers and run their actions
            ("DOCKER_HOST", "unix:///nonexistent/docker.sock".into()),
        ],
    )
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
