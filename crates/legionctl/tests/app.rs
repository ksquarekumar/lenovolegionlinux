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

/// The operator on a copy of the recorded machine, unplugged with the AC policy still applied, without a daemon.
fn unplugged_machine() -> Operator {
    let dir = std::env::temp_dir().join(format!("legionctl-app-{}", std::process::id()));
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
    std::fs::write(root.join("sys/class/power_supply/ADP0/online"), "0\n").unwrap();
    let bin = dir.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    Operator::start(
        "app",
        &[
            ("PATH", bin.into()),
            ("HOME", dir.join("home").into()),
            ("LEGIONCTL_SYSROOT", root.into()),
        ],
    )
}

#[gpui_kit::test]
fn the_app_shows_the_machine_previews_the_policy_reports_a_refused_apply_and_lists_findings(
    cx: &mut TestAppContext,
) {
    let op = unplugged_machine();
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

    cx.update(|window, cx| {
        window.render_frame(cx);
        eprintln!(
            "DEBUG window {:?} apply {:?} preview {:?}",
            window.viewport_size(),
            window.find("apply").bounds(),
            window.find("preview-policy").bounds()
        );
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
    drop(runtime);
}
