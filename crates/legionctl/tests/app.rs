//! The desktop app against the real operator on the recorded machine (unplugged, AC policy still applied, no
//! daemon): it shows the machine, previews the policy change, reports a refused apply, and shows doctor's findings.
//! Real clicks through GPUI's headless window; the operator answers in real time.

#[path = "common/adapter.rs"]
mod adapter;
#[path = "common/clauth.rs"]
mod clauth;
mod common;
#[path = "common/plain.rs"]
mod plain;
#[path = "common/portal.rs"]
mod portal;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::Operator;
use common::scratch::Scratch;
use gpui_kit::SharedString;
use gpui_kit::test::TestWindowExt;
use gpui_kit::{
    AppContext, Bounds, Entity, TestAppContext, VisualTestContext, WindowBounds, WindowHandle,
    WindowOptions, point, px, size,
};
use legionctl_api::ViewData;
use legionctl_api::client::Client;
use legionctl_app::{LegionApp, Space, Tab};

const FIXTURE: &str = "../../tests/fixtures/sysroot-82wq-2026-10-12";
const PROFILE: &str = "../../profiles/lenovo-82wq-legion-pro-7-16irx8h.json";
const LAYERS: &str = "../../configsets/lenovo-82wq-legion-pro-7-16irx8h";

/// The folder of the app's runs and layout facts in the tests that have no repository of their own.
fn here() -> PathBuf {
    std::env::current_dir().unwrap()
}

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

/// The events of operation `kind` (`events --json`), oldest first.
fn events_of(op: &Operator, kind: &str) -> Vec<serde_json::Value> {
    let page: serde_json::Value = serde_json::from_str(&op.ok(&["events", "--json"])).unwrap();
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|e| e["kind"] == kind)
        .cloned()
        .collect()
}

/// Scroll the tab's body until the element `id` is whole inside it: a click goes to an element's centre, and the
/// centre of an element cut at the body's edge hits whatever is there (a group head that did not open).
fn reveal(cx: &mut VisualTestContext, id: &str) {
    for _ in 0..60 {
        // None: shown; else the scroll toward it (down while it is not there yet)
        let step = cx.update(|window, cx| {
            window.render_frame(cx);
            let body = window.find("body").bounds();
            match window.try_find(SharedString::from(id.to_string())) {
                // an element outside the body (the title bar, the chat bar) shows without a scroll
                Some(e) if e.visible() && !e.path().contains(&"body".into()) => None,
                Some(e) if e.visible() => {
                    let b = e.bounds();
                    if b.top() < body.top() {
                        Some(px(100.))
                    } else if b.bottom() > body.bottom() {
                        Some(-(b.bottom() - body.bottom() + px(20.)).min(px(200.)))
                    } else {
                        None
                    }
                }
                _ => Some(px(-200.)),
            }
        });
        let Some(dy) = step else { return };
        cx.update(|window, cx| {
            window.scroll("body", gpui_kit::ScrollDelta::Pixels(point(px(0.), dy)), cx);
        });
    }
    panic!("{id} does not show");
}

/// A view tab.
fn view(name: &str) -> Tab {
    Tab::View {
        name: name.into(),
        params: Vec::new(),
    }
}

/// The view of the tab that shows, or the part `name` of its stack.
fn part<'a>(data: &'a ViewData, name: &str) -> Option<&'a ViewData> {
    if data.name == name {
        return Some(data);
    }
    data.parts.iter().find_map(|p| part(p, name))
}

/// The part `name` of the view that the tab shows, once it has read.
fn shown(a: &LegionApp, name: &str) -> Option<ViewData> {
    a.screen.view().and_then(|v| part(v, name)).cloned()
}

/// The seeds (DESIGN-09 G2) show the old pages' data through the one renderer, on the recorded machine unplugged
/// with the AC policy still applied: Machine now has the firmware mode and the power source in its stat and the
/// sensors by source with the temperatures' state; its button "Check the power policy" is a machine action, so it shows
/// its plan (the dry run: the battery policy's writes, one line for the governor of 32 CPUs) and Apply asks the daemon
/// exactly as the CLI does: without the system bus it is refused, the tab says why, and the event log names the app.
/// Health groups doctor's findings with their counts; Bench reads every page of the runs (1001, past the first 1000)
/// and shows the newest first, and one chart per kind of run.
#[gpui_kit::test]
fn the_seed_views_show_the_machine_and_a_generated_machine_button_asks_as_the_cli(
    cx: &mut TestAppContext,
) {
    let op = unplugged_machine("app");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., None);
    let cx = &mut vcx;

    // the glance: unplugged, firmware still in the AC mode
    wait(cx, &app, "the machine now", |a| {
        shown(a, "sensors").is_some_and(|s| !s.rows.is_empty())
    });
    app.read_with(cx, |a, _| {
        let mode = shown(a, "mode").unwrap();
        assert_eq!(
            (mode.text.as_str(), mode.note.as_str()),
            ("max-power", "on battery"),
            "{mode:?}"
        );
        let sensors = shown(a, "sensors").unwrap();
        let source = sensors
            .rows
            .iter()
            .position(|r| r[0] == "power source")
            .unwrap_or_else(|| panic!("{sensors:?}"));
        assert_eq!(sensors.groups[source], "power_supply", "grouped by source");
        assert_eq!(
            sensors.rows[source][1], "battery",
            "a reading's text where it has no number"
        );
    });

    // the button: a machine action shows its plan first
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("view-machine-now-action-0", cx);
    });
    wait(cx, &app, "the policy's plan", |a| {
        a.state.pending.as_ref().is_some_and(|p| p.plan.is_some())
    });
    app.read_with(cx, |a, _| {
        let pending = a.state.pending.as_ref().unwrap();
        assert_eq!(pending.class, legionctl_api::Class::Machine);
        let plan = pending.plan.as_deref().unwrap();
        assert!(plan.contains("platform-profile-0/profile: balanced"), "{plan}");
        // one line for the governor of all 32 CPUs, not 32 lines
        let governor = "would write /sys/devices/system/cpu/cpufreq/policy*/scaling_governor: powersave (32 CPUs)";
        assert!(plan.lines().any(|l| l.contains(governor)), "{plan}");
    });
    assert!(
        events_of(&op, "power.apply")
            .iter()
            .all(|e| e["actor"] != "app" || e["payload"]["input"]["dry_run"] == true),
        "only the dry run ran"
    );

    // apply: no daemon on this bus, so the operator refuses, as for the CLI; the app says why and keeps running
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("apply", cx);
    });
    wait(cx, &app, "the apply result", |a| {
        a.state.message.is_some() && a.state.pending.is_none()
    });
    app.read_with(cx, |a, _| {
        let message = a.state.message.as_deref().unwrap();
        assert!(message.contains("system bus"), "{message}");
    });
    let cli = op.cli(&["power", "apply"]);
    assert!(
        String::from_utf8_lossy(&cli.stderr).contains("system bus"),
        "the CLI gets the same refusal"
    );
    assert!(
        events_of(&op, "power.apply")
            .iter()
            .any(|e| e["actor"] == "app" && e["payload"]["input"]["dry_run"] != true),
        "the app's apply is an event"
    );

    health_and_bench(cx, &app);
    drop(runtime);
}

/// Health groups doctor's findings; Bench reads every page of the runs, the newest first, one chart per kind.
fn health_and_bench(cx: &mut VisualTestContext, app: &Entity<LegionApp>) {
    health_findings(cx, app);
    bench_runs(cx, app);
}

/// Health: doctor's findings in groups with their counts, each with the actions that fix it.
fn health_findings(cx: &mut VisualTestContext, app: &Entity<LegionApp>) {
    cx.update(|window, cx| window.click("nav-health", cx));
    wait(cx, app, "the findings", |a| {
        shown(a, "health").is_some_and(|h| !h.rows.is_empty())
    });
    app.read_with(cx, |a, _| {
        let health = shown(a, "health").unwrap();
        let at = health
            .rows
            .iter()
            .position(|r| r[0] == "platform_profile is 'max-power', expected 'balanced'")
            .unwrap_or_else(|| panic!("{health:?}"));
        assert_eq!(health.groups[at], "To check");
        assert_eq!(
            health.groups.len(),
            health.rows.len(),
            "each finding is in a group"
        );
        // r324: a fixed "Install the missing agent setup" button stayed when the setup was complete; now only a
        // finding has a button, and only when legionctl can fix it
        assert!(health.actions.is_empty(), "{:?}", health.actions);
        let buttons = |text: &str| -> Vec<(String, String, serde_json::Value)> {
            let i = health
                .rows
                .iter()
                .position(|r| r[0] == text)
                .unwrap_or_else(|| panic!("{text}: {:?}", health.rows));
            health.row_actions[i]
                .iter()
                .map(|b| (b.label.clone(), b.op.clone(), b.input.clone()))
                .collect()
        };
        assert_eq!(
            buttons("an external display runs on NVIDIA but the desktop may be drawn on Intel"),
            [(
                "Draw the desktop on NVIDIA".into(),
                "gpu.primary".into(),
                serde_json::json!({"choice": "nvidia"})
            )]
        );
        assert_eq!(
            buttons("setup: ctx for all is not installed"),
            [(
                "Install ctx".into(),
                "setup.apply".into(),
                serde_json::json!({"install_ctx": true})
            )]
        );
        assert_eq!(
            buttons("setup: legionctl mcp for codex is missing")[0].2,
            serde_json::json!({"install_ctx": false})
        );
        assert_eq!(
            buttons("platform_profile is 'max-power', expected 'balanced'"),
            Vec::<(String, String, serde_json::Value)>::new()
        );
        // a check that could not run says so and when it runs again; no row is a command line for a terminal
        assert_eq!(
            buttons(
                "the desktop check could not run (systemctl is not installed); the next health check runs it \
                 again"
            ),
            Vec::<(String, String, serde_json::Value)>::new()
        );
        for row in &health.rows {
            for hint in [
                "`legionctl",
                "(legionctl",
                "run legionctl",
                "sudo ",
                "see systemctl",
                "journalctl",
                "docs/",
            ] {
                assert!(!row[0].contains(hint), "{hint} in {row:?}");
            }
        }
    });
}

/// Bench: every page of the runs, the newest first, one chart per kind.
fn bench_runs(cx: &mut VisualTestContext, app: &Entity<LegionApp>) {
    let db = std::env::temp_dir().join(format!(
        "legionctl-app-{}/home/.local/state/legionctl/operator.db",
        std::process::id()
    ));
    let runs = "WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 1001) \
                INSERT INTO events (ts, actor, kind, payload) SELECT 1700000000 + i, 'cli', 'bench.result', \
                json_object('scenario', 'cpu', 'label', CASE WHEN i % 2 = 0 THEN 'app' ELSE 'live-cpu' END, \
                'seconds', 5, 'scores', json_object('cpu bogo ops/s', 1000 + i), \
                'problems', json_array(), 'summary', json_object('samples', 1, 'throttle', json_array()), \
                'knobs', json_object()) FROM n";
    let stored = std::process::Command::new("sqlite3")
        .arg(&db)
        .arg(runs)
        .status()
        .unwrap();
    assert!(stored.success());
    cx.update(|window, cx| window.click("nav-bench", cx));
    wait(cx, app, "the runs", |a| {
        shown(a, "bench-runs").is_some_and(|b| !b.rows.is_empty())
    });
    app.read_with(cx, |a, _| {
        let runs = shown(a, "bench-runs").unwrap();
        assert_eq!(runs.rows.len(), 200, "the newest 200");
        assert_eq!(
            runs.rows[0][1], "live-cpu",
            "run 1001 first: {:?}",
            runs.rows[0]
        );
        assert_eq!(runs.rows[0][2], "2001", "{:?}", runs.rows[0]);
        let chart = shown(a, "bench-cpu").unwrap();
        let kinds: Vec<(&str, usize)> = chart
            .series
            .iter()
            .map(|s| (s.name.as_str(), s.points.len()))
            .collect();
        assert_eq!(
            kinds,
            [("live-cpu", 501), ("app", 500)],
            "one line per kind, every page"
        );
    });
}

/// The operation catalog (DESIGN-09 G3): the seed Operations lists each public operation by its group (folded, with
/// its count); Open on `facts.set` opens its form, made from its input schema (the required fields first, the optional
/// ones on demand, a choice as buttons). The first field takes the keys; Enter without a required field says which;
/// the fields paste (Ctrl+V, the middle button); with them, the session class runs at once and stores the fact with
/// the user as its source.
#[gpui_kit::test]
fn an_operation_of_the_catalog_runs_through_the_form_of_its_input_schema(cx: &mut TestAppContext) {
    let (op, home, repo) = thread_lab("form");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    let cx = &mut vcx;
    let click = |cx: &mut VisualTestContext, id: SharedString| {
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click(id, cx);
        });
    };
    click(cx, "space-agents".into());
    wait(cx, &app, "the views", |a| {
        a.screen.views.iter().any(|v| v.name == "operations")
    });
    click(cx, "nav-operations".into());
    wait(cx, &app, "the catalog", |a| {
        shown(a, "operations").is_some_and(|v| !v.rows.is_empty())
    });
    let row = app.read_with(cx, |a, _| {
        let catalog = shown(a, "operations").unwrap();
        assert!(
            catalog.rows.len() > 150,
            "every public operation: {}",
            catalog.rows.len()
        );
        assert!(catalog.rows.iter().all(|r| !r[0].starts_with("internal.")));
        catalog
            .rows
            .iter()
            .position(|r| r[0] == "facts.set")
            .unwrap()
    });
    reveal(cx, "view-operations-group-facts");
    click(cx, "view-operations-group-facts".into());
    reveal(cx, &format!("view-operations-row-{row}-action-0"));
    click(cx, format!("view-operations-row-{row}-action-0").into());
    app.read_with(cx, |a, _| {
        let form = a.state.form.as_ref().unwrap();
        let required: Vec<&str> = form
            .fields
            .iter()
            .filter(|f| f.required)
            .map(|f| f.name.as_str())
            .collect();
        assert_eq!(required, ["entity", "value"]);
    });
    // the first field has the keys: Enter goes on as Continue does
    keys(cx, &["enter"]);
    app.read_with(cx, |a, _| {
        let why = a
            .state
            .form
            .as_ref()
            .unwrap()
            .error
            .clone()
            .unwrap_or_default();
        assert!(why.starts_with("entity is required"), "{why}");
    });
    // a form field pastes as the chat bar does (todo-195): typed text, then Ctrl+V; the middle button
    cx.update(|window, cx| {
        window.input("col", cx);
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("or".into()));
        window.press("ctrl-v", cx);
        cx.write_to_primary(gpui_kit::ClipboardItem::new_string("\"teal\"".into()));
        window.render_frame(cx);
    });
    middle_click(cx, "form-field-value");
    click(cx, "form-more".into());
    click(cx, "form-scope-project".into());
    click(cx, "form-submit".into());
    // a change of the user's own session runs at once: no confirm
    app.read_with(cx, |a, _| {
        assert!(a.state.form.is_none() && a.state.pending.is_none());
    });
    wait(cx, &app, "the fact", |a| {
        a.state
            .message
            .as_deref()
            .is_some_and(|m| m.contains("fact"))
    });
    let facts: serde_json::Value = serde_json::from_str(&lab_cli(
        &op,
        &home,
        &repo,
        &["facts", "at", "--entity", "color", "--json"],
        "",
    ))
    .unwrap();
    assert_eq!(facts[0]["value"], "teal", "{facts}");
    assert_eq!(
        (facts[0]["source"].as_str(), facts[0]["scope"].as_str()),
        (Some("user"), Some("project"))
    );
    drop(runtime);
}

/// git in the lab's repository as the user.
fn lab_git(home: &Path, repo: &Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
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

/// The lab's ctx (HOME/.local/bin comes before the host's PATH): the stand-in, never the host's ctx.
fn lab_ctx(home: &Path, ctx: &Path) {
    let at = home.join(".local/bin/ctx");
    let _ = std::fs::remove_file(&at);
    assert!(ctx.is_file(), "no ctx at {}", ctx.display());
    std::os::unix::fs::symlink(ctx, at).unwrap();
}

/// A ctx that cannot index: no graph can be made.
fn broken_ctx(home: &Path) {
    let broken = home.join("broken-ctx");
    write(
        &broken,
        "#!/bin/sh\necho 'ctx: cannot index here' >&2\nexit 2\n",
    );
    std::fs::set_permissions(&broken, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    lab_ctx(home, &broken);
}

/// The ctx stand-in, then `code graph refresh` of main and HEAD, as a user does after the reason.
fn refresh_with_standin(op: &Operator, home: &Path, repo: &Path) {
    lab_ctx(
        home,
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/ctx-standin"),
    );
    for rev in ["main", "HEAD"] {
        let out = op.run(
            repo,
            &[("HOME", home.to_str().unwrap())],
            &["code", "graph", "refresh", "--ref", rev],
            "",
        );
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// The lab's repository: src/lib.rs (add, two) and tools/x.rs on main, then on branch work a commit that changes
/// two.
fn graph_repo(home: &Path, repo: &Path) {
    write(
        &repo.join("Cargo.toml"),
        "[package]\nname = \"fx\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    write(
        &repo.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn two() -> i32 {\n    add(1, 1)\n}\n",
    );
    write(&repo.join("tools/x.rs"), "pub fn x() -> i32 {\n    1\n}\n");
    lab_git(home, repo, &["add", "."]);
    lab_git(home, repo, &["commit", "--quiet", "-m", "lib"]);
    lab_git(home, repo, &["checkout", "--quiet", "-b", "work"]);
    write(
        &repo.join("src/lib.rs"),
        "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n\npub fn two() -> i32 {\n    add(2, 0)\n}\n",
    );
    lab_git(home, repo, &["commit", "--quiet", "-am", "two adds 2"]);
}

/// The code tree of a folder in no git repository asks for one (r324 showed git's error); Choose a repository with
/// `repo` gives the tab its own folder.
fn choose_the_repository(cx: &mut VisualTestContext, app: &Entity<LegionApp>, repo: &Path) {
    wait(cx, app, "the question for a repository", |a| {
        shown(a, "code-tree").is_some_and(|v| v.needs_repository)
    });
    app.read_with(cx, |a, _| {
        let tree = shown(a, "code-tree").unwrap();
        assert_eq!(tree.error, None, "a question, not git's error");
        assert!(tree.note.contains("is not in one"), "{}", tree.note);
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("view-code-tree-choose-repo", cx);
    });
    wait(cx, app, "the folder's form", |a| a.state.form.is_some());
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("form-field-repo", cx);
        window.input(repo.to_str().unwrap(), cx);
        window.click("form-submit", cx);
    });
    app.read_with(cx, |a, _| {
        assert!(
            a.tab()
                .is_some_and(|t| t.key().contains(&format!("repo={}", repo.display()))),
            "the tab keeps its folder: {:?}",
            a.tab()
        );
    });
}

/// The code graph seed (DESIGN-09 G4, todo-141) on a repository with the ctx stand-in: on branch work, the commit
/// after main changes `two`. The app starts in the home (as from the desktop), which is in no git repository: the
/// tree part asks for a repository (r324 showed git's error), and Choose a repository gives the tab its own folder,
/// which the tab keeps (a click on a node too). A ctx that cannot index gives no graph: the tree part shows why, not
/// an empty tab. With the stand-in, the tree's top has the folder src with its symbol count, marked as changed since main (tools is
/// not); a click on src reads its level (src/lib.rs, marked), a click on the file its symbols (two is marked, add is
/// not); a click on add selects it, and the stack's other part reads what uses it: two.
#[gpui_kit::test]
fn the_code_graph_seed_opens_levels_on_click_marks_changes_and_shows_what_uses_a_selected_symbol(
    cx: &mut TestAppContext,
) {
    let (op, home, repo) = thread_lab("graph");
    graph_repo(&home, &repo);
    broken_ctx(&home);
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &home)));
    let cx = &mut vcx;
    let click = |cx: &mut VisualTestContext, id: &str| {
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click(SharedString::from(id.to_string()), cx);
        });
    };
    click(cx, "space-work");
    wait(cx, &app, "the views", |a| {
        a.screen.views.iter().any(|v| v.name == "code-graph")
    });
    click(cx, "nav-code-graph");
    choose_the_repository(cx, &app, &repo);
    wait(cx, &app, "why there is no graph", |a| {
        shown(a, "code-tree").is_some_and(|v| v.error.is_some())
    });
    app.read_with(cx, |a, _| {
        let error = shown(a, "code-tree").unwrap().error.clone().unwrap();
        assert!(
            error.starts_with("ctx graph index refused tree ")
                && error.ends_with(": ctx: cannot index here"),
            "{error}"
        );
    });
    // with a working ctx, a refresh makes the graphs, and its event reads the tab again
    refresh_with_standin(&op, &home, &repo);
    wait(cx, &app, "the tree", |a| {
        shown(a, "code-tree").is_some_and(|v| !v.nodes.is_empty())
    });
    app.read_with(cx, |a, _| {
        let tree = shown(a, "code-tree").unwrap();
        let top: Vec<(&str, Option<u64>, bool)> = tree
            .nodes
            .iter()
            .map(|n| (n.id.as_str(), n.count, n.marked))
            .collect();
        assert!(top.contains(&("src", Some(2), true)), "{top:?}");
        assert!(top.contains(&("tools", Some(1), false)), "{top:?}");
        assert_eq!(tree.legend, "changed since main");
        let impact = shown(a, "code-impact").unwrap();
        assert_eq!(
            impact.note,
            "Click a symbol in the tree to see what calls it"
        );
    });
    click(cx, "node-code-tree:src");
    wait(cx, &app, "src's level", |a| {
        a.screen.lazy.contains_key("code-tree:src")
    });
    app.read_with(cx, |a, _| {
        let level = a.screen.lazy["code-tree:src"].as_ref().unwrap();
        assert_eq!(level.len(), 1, "{level:?}");
        assert_eq!(
            (level[0].id.as_str(), level[0].marked),
            ("src/lib.rs", true)
        );
    });
    click(cx, "node-code-tree:src/lib.rs");
    wait(cx, &app, "the file's symbols", |a| {
        a.screen.lazy.contains_key("code-tree:src/lib.rs")
    });
    let add = app.read_with(cx, |a, _| {
        let symbols = a.screen.lazy["code-tree:src/lib.rs"].as_ref().unwrap();
        let marks: Vec<(&str, bool, bool)> = symbols
            .iter()
            .map(|n| (n.label.as_str(), n.marked, n.leaf))
            .collect();
        assert!(
            marks.contains(&("two", true, true)) && marks.contains(&("add", false, true)),
            "{marks:?}"
        );
        symbols
            .iter()
            .find(|n| n.label == "add")
            .unwrap()
            .id
            .clone()
    });
    click(cx, &format!("node-code-tree:{add}"));
    wait(cx, &app, "what uses add", |a| {
        shown(a, "code-impact").is_some_and(|v| !v.rows.is_empty())
    });
    app.read_with(cx, |a, _| {
        assert!(
            a.tab()
                .is_some_and(|t| t.key().contains(&format!("key={add}")))
        );
        let impact = shown(a, "code-impact").unwrap();
        assert!(
            impact.rows.iter().any(|r| r[0].ends_with("two")),
            "{:?}",
            impact.rows
        );
    });
    drop(runtime);
}

/// The chat bar's text in a view tab is a message for the ui-composer (DESIGN-09 G5, user 2026-10-17: one input, one
/// meaning). The intent of a seed opens it from the store: no model runs (no session.run). A new intent goes to the
/// ui-composer (the chat bar's agent with its model from `views.composer_models`) with the tab's view: its answer is
/// the view's next version (Health, version 1), and the intent is a fact of the user; the same intent again, from
/// another tab, opens that view with no model.
#[gpui_kit::test]
fn an_intent_opens_a_stored_view_with_no_model_and_a_new_one_gets_the_composers_view(
    cx: &mut TestAppContext,
) {
    let (op, home, repo) = thread_lab("intent");
    write(
        &home.join("fake-views/fan.jsonc"),
        r#"{"name": "health", "title": "Fan speeds", "widget": "table", "source": {"op": "sensors"},
            "columns": ["name", "value"], "filter": {"unit": "RPM"}}"#,
    );
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    let cx = &mut vcx;
    // the model runs of the operator: its session.run jobs
    let runs = || {
        let jobs: serde_json::Value =
            serde_json::from_str(&op.ok(&["jobs", "list", "--json"])).unwrap();
        jobs["items"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|j| j["kind"] == "session.run")
            .count()
    };
    let say = |cx: &mut VisualTestContext, text: &str| {
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("chat", cx);
            window.input(text, cx);
            window.press("enter", cx);
        });
    };
    say(cx, "what is wrong with the machine");
    wait(cx, &app, "the stored view", |a| {
        a.tab().is_some_and(|t| t.key().starts_with("view:health:"))
    });
    assert_eq!(runs(), 0, "a stored view answers with no model");

    say(cx, "fan speeds of the machine");
    wait(cx, &app, "the composer's view", |a| {
        a.screen.view().is_some_and(|v| v.title == "Fan speeds")
    });
    app.read_with(cx, |a, _| {
        let v = a.screen.view().unwrap();
        assert_eq!(
            (v.name.as_str(), v.version),
            ("health", 1),
            "the tab's view (Health, from the first intent), its next version"
        );
        let said = a.state.message.clone().unwrap_or_default();
        assert!(
            said.contains("model: fake-small") && said.contains("Here is the view"),
            "{said}"
        );
        assert!(
            !said.contains("tokens in") && !said.contains("(composer"),
            "the agent's words, not the CLI's route and cost: {said}"
        );
        assert!(
            !said.contains(r#""filter": {"unit": "RPM"}"#),
            "the spec is taken out of the words: {said}"
        );
    });
    assert_eq!(runs(), 1);
    let facts: serde_json::Value = serde_json::from_str(&lab_cli(
        &op,
        &home,
        &repo,
        &[
            "facts",
            "at",
            "--entity",
            "intent:fan speeds of the machine",
            "--json",
        ],
        "",
    ))
    .unwrap();
    assert_eq!(
        (facts[0]["value"].as_str(), facts[0]["source"].as_str()),
        (Some("health"), Some("user"))
    );

    cx.update(|window, cx| window.click("nav-resources", cx));
    say(cx, "Fan speeds of the machine?");
    wait(cx, &app, "the stored answer", |a| {
        a.tab().is_some_and(|t| t.key().starts_with("view:health:"))
    });
    assert_eq!(
        runs(),
        1,
        "the same intent again: the stored view, no model"
    );
    drop(runtime);
}

/// The stand-in composer's answers of the change test: two tables, a view outside the widget set, a part with no
/// title, and a code view that the model puts in the Machine space.
fn composed_views(views: &Path) {
    let spec = |title: &str| {
        format!(
            r#"{{"name": "anything", "title": "{title}", "widget": "table", "source": {{"op": "sensors"}},
                "columns": ["name", "value"]}}"#
        )
    };
    write(&views.join("sensors.jsonc"), &spec("Sensors, plain"));
    write(&views.join("add.jsonc"), &spec("Sensors, with more"));
    write(
        &views.join("bad.jsonc"),
        r#"{"name": "x", "widget": "piechart", "source": {"op": "sensors"}}"#,
    );
    // a part without a title would show its id as the card's heading
    write(
        &views.join("untitled.jsonc"),
        r#"{"name": "y", "title": "Heat", "widget": "stack", "views": [
            {"name": "temp-stat", "widget": "table", "source": {"op": "sensors"}, "columns": ["name"]}]}"#,
    );
    // the model says machine; the source is the code graph, so the view is Work's
    write(
        &views.join("what.jsonc"),
        r#"{"name": "branch-changes", "title": "Branch changes", "space": "machine", "widget": "table",
            "source": {"op": "code.graph.changes", "input": {"base": "main"}}, "columns": ["key"]}"#,
    );
}

/// A message in a view tab (DESIGN-09 G5): the composer's answer is the next version of the tab's view (version 2,
/// the same name); Undo goes back to version 1. A composed view that fails its check, or has a part with no title, is shown as an
/// error with its reason, and nothing is saved. A composed view's space follows its source, not the model's word.
#[gpui_kit::test]
fn a_change_is_the_next_version_of_the_tabs_view_and_a_bad_view_shows_its_error(
    cx: &mut TestAppContext,
) {
    let (op, home, repo) = thread_lab("change");
    composed_views(&home.join("fake-views"));
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    let cx = &mut vcx;
    let say = |cx: &mut VisualTestContext, text: &str| {
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("chat", cx);
            window.input(text, cx);
            window.press("enter", cx);
        });
    };
    say(cx, "sensors in a plain table");
    wait(cx, &app, "version 1", |a| {
        a.screen.view().is_some_and(|v| v.title == "Sensors, plain")
    });
    let name = app.read_with(cx, |a, _| a.screen.view().unwrap().name.clone());
    say(cx, "add the unit");
    wait(cx, &app, "version 2", |a| {
        a.screen
            .view()
            .is_some_and(|v| v.title == "Sensors, with more")
    });
    app.read_with(cx, |a, _| {
        let v = a.screen.view().unwrap();
        assert_eq!(
            (v.name.as_str(), v.version, v.versions.clone()),
            (name.as_str(), 2, vec![0, 1, 2])
        );
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("view-undo", cx);
    });
    wait(cx, &app, "version 1 again", |a| {
        a.screen.view().is_some_and(|v| v.version == 1)
    });
    app.read_with(cx, |a, _| {
        assert_eq!(a.screen.view().unwrap().title, "Sensors, plain");
    });

    say(cx, "bad view");
    wait(cx, &app, "the check's error", |a| {
        a.state
            .message
            .as_deref()
            .is_some_and(|m| m.starts_with("The agent's view is not valid, so nothing changed"))
    });
    app.read_with(cx, |a, _| {
        let said = a.state.message.clone().unwrap();
        assert!(said.contains("piechart"), "{said}");
        assert_eq!(
            a.screen.view().unwrap().versions,
            [0, 1, 2],
            "nothing saved"
        );
    });
    say(cx, "untitled part");
    wait(cx, &app, "the title's error", |a| {
        a.state.message.as_deref().is_some_and(|m| {
            m.contains("each view and each part needs a title in the user's words: temp-stat")
        })
    });
    app.read_with(cx, |a, _| {
        assert_eq!(
            a.screen.view().unwrap().versions,
            [0, 1, 2],
            "nothing saved"
        );
    });

    say(cx, "what changed on this branch");
    wait(cx, &app, "the composed code view in the list", |a| {
        a.screen
            .views
            .iter()
            .any(|v| v.name == name && v.space.as_deref() == Some("work"))
    });
    app.read_with(cx, |a, _| {
        let listed = a.screen.views.iter().find(|v| v.name == name);
        assert_eq!(listed.and_then(|v| v.space.as_deref()), Some("work"));
    });
    drop(runtime);
}

/// The Resources seed (note 22) against the real operator on the recorded machine, without a daemon: it lists the
/// configset layers in their group with their actions, which come with the data (each action names its operation);
/// a click shows the route first (rules.disable is a machine operation with no dry run, so the tab asks inline and
/// nothing runs), and Apply runs it as a call of its own: the operator refuses it without the system bus, the tab
/// says why, and the event log names `rules.disable` with the app as its actor. The confirm is for the keyboard too:
/// Enter and Esc discard, Tab goes to Discard first, and each button has its name for a screen reader.
#[gpui_kit::test]
fn the_resources_seed_lists_the_resources_and_a_row_action_runs_its_operation(
    cx: &mut TestAppContext,
) {
    let op = unplugged_machine("resources");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., None);
    let cx = &mut vcx;
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("nav-resources", cx);
    });
    wait(cx, &app, "the resources", |a| {
        shown(a, "resources").is_some_and(|r| !r.rows.is_empty())
    });
    let row = app.read_with(cx, |a, _| {
        let list = shown(a, "resources").unwrap();
        let row = list
            .row_actions
            .iter()
            .position(|b| {
                b.iter()
                    .any(|b| b.input == serde_json::json!({"layer": "shipped/20-ac"}))
            })
            .unwrap_or_else(|| panic!("{list:?}"));
        assert_eq!(list.groups[row], "Configuration layers");
        let actions: Vec<(&str, &str)> = list.row_actions[row]
            .iter()
            .map(|b| (b.label.as_str(), b.op.as_str()))
            .collect();
        assert_eq!(actions, [("disable", "rules.disable")]);
        row
    });

    let id = format!("view-resources-row-{row}-action-0");
    cx.update(|window, cx| {
        window.render_frame(cx);
        // the group of the layers is open (a short group)
        window.click(SharedString::from(id.clone()), cx);
    });
    app.read_with(cx, |a, _| {
        let pending = a.state.pending.as_ref().unwrap();
        assert_eq!(pending.button.op, "rules.disable");
        assert!(
            pending.plan.is_none(),
            "no dry run: an inline confirm of the operation and its input"
        );
    });
    // todo-195 (4): the keys are on the card and the safe choice is the default: Enter discards, Esc discards, and
    // the keys go back to the chat bar; Tab goes to Discard, then Apply, and Enter there applies
    cx.update(|window, cx| {
        window.render_frame(cx);
        for (id, label) in [("discard", "Discard"), ("apply", "Apply")] {
            let button = window.find(id);
            assert_eq!(button.role(), Some(gpui_kit::Role::Button), "{id}");
            assert_eq!(button.label(), Some(label), "{id}");
        }
        window.press("enter", cx);
    });
    app.read_with(cx, |a, _| {
        assert!(a.state.pending.is_none(), "Enter discards");
    });
    let open = |cx: &mut VisualTestContext| {
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click(SharedString::from(id.clone()), cx);
            window.render_frame(cx);
        });
        app.read_with(cx, |a, _| assert!(a.state.pending.is_some()));
    };
    cx.update(|window, cx| window.click("chat", cx));
    open(cx);
    keys(cx, &["escape"]);
    app.read_with(cx, |a, _| {
        assert!(a.state.pending.is_none(), "Esc discards");
    });
    cx.update(|window, cx| window.input("k", cx));
    assert_eq!(
        chat_text(cx, &app),
        "k",
        "the keys are back in the chat bar"
    );
    keys(cx, &["backspace"]);
    open(cx);
    keys(cx, &["tab", "enter"]);
    app.read_with(cx, |a, _| {
        assert!(a.state.pending.is_none(), "Discard is the first button");
    });
    assert!(
        events_of(&op, "rules.disable").is_empty(),
        "nothing ran before Apply"
    );

    open(cx);
    keys(cx, &["tab", "tab", "enter"]);
    wait(cx, &app, "the apply result", |a| {
        a.state.message.is_some() && a.state.pending.is_none()
    });
    app.read_with(cx, |a, _| {
        let message = a.state.message.as_deref().unwrap();
        assert!(message.contains("system bus"), "{message}");
    });
    assert!(
        events_of(&op, "rules.disable")
            .iter()
            .any(|e| e["actor"] == "app"),
        "the app's apply is an event"
    );
    drop(runtime);
}

/// Views persist as versions (DESIGN-09 G2): a saved change of the Health seed is version 1 and shows; Undo shows
/// the seed (version 0), Redo version 1 again; Pin keeps version 1 as the one Reset goes back to; after a version 2,
/// Reset shows version 1; each step is a fact `view:health` `version` of the user, and a data refresh does not change
/// the version.
#[gpui_kit::test]
fn a_view_keeps_its_versions_and_the_tab_undoes_redoes_pins_and_resets_them(
    cx: &mut TestAppContext,
) {
    let (op, home, repo) = thread_lab("versions");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    let cx = &mut vcx;
    let seed = std::fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config/views/health.jsonc"),
    )
    .unwrap();
    let save = |title: &str| {
        lab_cli(
            &op,
            &home,
            &repo,
            &["views", "save"],
            &seed.replace("\"title\": \"Health\"", &format!("\"title\": \"{title}\"")),
        );
    };
    save("Health, mine");
    cx.update(|window, cx| window.click("nav-health", cx));
    let version = |a: &LegionApp| a.screen.view().map(|v| (v.version, v.title.clone()));
    wait(cx, &app, "version 1", |a| {
        version(a) == Some((1, "Health, mine".into()))
    });
    app.read_with(cx, |a, _| {
        assert_eq!(a.screen.view().unwrap().versions, [0, 1]);
    });
    let click = |cx: &mut VisualTestContext, id: &'static str| {
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click(id, cx);
        });
    };
    click(cx, "view-undo");
    wait(cx, &app, "the seed", |a| {
        version(a) == Some((0, "Health".into()))
    });
    click(cx, "view-redo");
    wait(cx, &app, "version 1 again", |a| {
        version(a) == Some((1, "Health, mine".into()))
    });
    click(cx, "view-pin");
    wait(cx, &app, "the pin", |a| {
        a.state
            .message
            .as_deref()
            .is_some_and(|m| m.contains("pinned"))
    });
    save("Health, two");
    wait(cx, &app, "version 2", |a| {
        version(a) == Some((2, "Health, two".into()))
    });
    click(cx, "view-reset");
    wait(cx, &app, "the pinned version", |a| {
        version(a) == Some((1, "Health, mine".into()))
    });
    let facts: serde_json::Value = serde_json::from_str(&lab_cli(
        &op,
        &home,
        &repo,
        &[
            "facts",
            "at",
            "--entity",
            "view:health",
            "--winners",
            "--json",
        ],
        "",
    ))
    .unwrap();
    let shown_version = facts
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["attribute"] == "version")
        .unwrap();
    assert_eq!(
        (
            shown_version["value"].as_u64(),
            shown_version["source"].as_str()
        ),
        (Some(1), Some("user"))
    );
    drop(runtime);
}

fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

/// The operator on the recorded machine with its own HOME (the stand-in ACP agent as `fake` and `other` of config
/// session.acp, `fake` the chat bar's agent through config app.agent), a repository, and
/// thread 1 on dossier port: main0 (session `main`) and observer0 (session `obs`). Returns the operator, HOME and
/// the repository.
fn thread_lab(tag: &str) -> (Operator, PathBuf, PathBuf) {
    thread_lab_on(tag, &[])
}

/// [`thread_lab`] with more environment for the operator.
fn thread_lab_on(tag: &str, env: &[(&str, std::ffi::OsString)]) -> (Operator, PathBuf, PathBuf) {
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
            "session": {
                "acp": {"fake": ["python3", agent], "other": ["python3", agent, "other"]},
                "scope": false,
                "writable": []
            },
            "app": {"agent": "fake"},
            "views": {"composer_models": {"fake": "fake-small", "other": "other-small"}}
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
    // the home's own tools (a test puts the ctx stand-in there) come first
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    let path = std::env::join_paths(
        [home.join(".local/bin"), bin]
            .into_iter()
            .chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .unwrap();
    recorded_machine(&home.join("root"));
    let mut lab_env = vec![
        ("LEGIONCTL_SYSROOT", home.join("root").into()),
        ("HOME", home.as_os_str().to_owned()),
        ("PATH", path),
        // the stand-in composer answers an intent with the view of <first word>.jsonc there
        ("FAKE_ACP_VIEWS", home.join("fake-views").into()),
        // a WAIT prompt holds until the test makes this file (a chat's folder is an overlay: its lower
        // folder's new files need not show)
        ("FAKE_ACP_RELEASE", home.join("release").into()),
    ];
    lab_env.extend(env.iter().cloned());
    let mut op = Operator::start(&format!("ap-{tag}"), &lab_env);
    // the user's dossier of the app's chats and runs, made and chosen as the user does (no session)
    for args in [
        &["dossier", "create", "chat"][..],
        &["dossier", "use", "chat"],
    ] {
        user_cli(&op, &home, &repo, args);
    }
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

/// stdout of `legionctl <args>` of the user (no agent session) in `cwd`; it must succeed.
fn user_cli(op: &Operator, home: &Path, cwd: &Path, args: &[&str]) -> String {
    let out = op.run(cwd, &[("HOME", home.to_str().unwrap())], args, "");
    assert!(
        out.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap()
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

/// The app in a headless window of `width` x 700 against `op` with the seed Machine now as its tab, and its entity; `runs`
/// is the repository of the app's views and config (the lab's).
fn open_app(
    cx: &mut TestAppContext,
    op: &Operator,
    width: f32,
    runs: Option<(&Path, &Path)>,
) -> (
    Entity<LegionApp>,
    VisualTestContext,
    tokio::runtime::Runtime,
) {
    let (app, mut vcx, runtime) = open_app_at(cx, &op.runtime, width, runs);
    // the layout facts first: they set the tabs
    wait(&mut vcx, &app, "the layout", |a| a.screen.restored);
    (app, vcx, runtime)
}

/// The app of [`open_app`] with the operator of the runtime folder `operator` (`$XDG_RUNTIME_DIR`), before its layout.
fn open_app_at(
    cx: &mut TestAppContext,
    operator: &Path,
    width: f32,
    runs: Option<(&Path, &Path)>,
) -> (
    Entity<LegionApp>,
    VisualTestContext,
    tokio::runtime::Runtime,
) {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = Client::at("app", operator);
    let handle = runtime.handle().clone();
    let repo = runs.map_or_else(here, |(_, repo)| repo.to_path_buf());
    cx.update(|cx| {
        gpui_kit::init(cx);
        legionctl_app::init(cx);
    });
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds {
            origin: point(px(0.), px(0.)),
            size: size(px(width), px(700.)),
        })),
        ..WindowOptions::default()
    };
    let window: WindowHandle<gpui_kit::base::Root> = cx.update(|cx| {
        let (window, _) = gpui_kit::open_window(options, cx, |window, cx| {
            cx.new(|cx| LegionApp::new(client, handle, Some(view("machine-now")), repo, window, cx))
        })
        .unwrap();
        window.downcast().unwrap()
    });
    let app: Entity<LegionApp> = window
        .update(cx, |root, _, _| root.view().clone().downcast().unwrap())
        .unwrap();
    let vcx = VisualTestContext::from_window(window.into(), cx);
    (app, vcx, runtime)
}

/// What the stand-in systemd manager was asked, in order.
type ManagerCalls = std::sync::Arc<std::sync::Mutex<Vec<String>>>;

/// The user's systemd manager on its private socket: `StartUnit` of the socket links the app's runtime folder to the
/// test operator's (`legionctl/` and the token), as a real start makes the socket answer.
struct Manager {
    calls: ManagerCalls,
    app: PathBuf,
    operator: PathBuf,
}

#[zbus::interface(name = "org.freedesktop.systemd1.Manager")]
impl Manager {
    fn enable_unit_files(
        &self,
        files: Vec<String>,
        runtime: bool,
        force: bool,
    ) -> (bool, Vec<(String, String, String)>) {
        let mut call = "EnableUnitFiles".to_string();
        for file in files {
            call = format!("{call} {file}");
        }
        self.calls
            .lock()
            .unwrap()
            .push(format!("{call} {runtime} {force}"));
        (true, Vec::new())
    }
    fn reload(&self) {
        self.calls.lock().unwrap().push("Reload".into());
    }
    /// The first start fails as for a masked unit; the next one starts it.
    fn start_unit(
        &self,
        name: &str,
        mode: &str,
    ) -> zbus::fdo::Result<zbus::zvariant::OwnedObjectPath> {
        let mut calls = self.calls.lock().unwrap();
        let first = !calls.iter().any(|c| c.starts_with("StartUnit"));
        calls.push(format!("StartUnit {name} {mode}"));
        if first {
            return Err(zbus::fdo::Error::Failed(
                "Unit legionctl-operator.socket is masked.".into(),
            ));
        }
        for part in ["legionctl", legionctl_api::USER_TOKEN] {
            std::os::unix::fs::symlink(self.operator.join(part), self.app.join(part)).unwrap();
        }
        Ok(zbus::zvariant::OwnedObjectPath::try_from("/org/freedesktop/systemd1/job/1").unwrap())
    }
}

/// U-14, user decision 2026-10-17: when the operator is off, the app shows one card that says so with the line that
/// turns it on, and Turn on does that step itself: through the user's systemd manager on its private socket (as
/// `systemctl --user enable --now legionctl-operator.socket` does; no root), with no other call. A failed start shows
/// its cause in the card; after a start the app finds the operator, the card goes and the layout that could not be read
/// at the start is read.
#[gpui_kit::test]
fn the_offline_card_turns_the_operator_on_through_the_users_service_manager(
    cx: &mut TestAppContext,
) {
    let op = unplugged_machine("turn-on");
    // the operator's token exists before the app's first answer
    op.ok(&["load"]);
    let folder = Scratch::new(format!("/var/tmp/lc-turn-on-app-{}", std::process::id()));
    let private = folder.join("systemd/private");
    std::fs::create_dir_all(private.parent().unwrap()).unwrap();
    let calls = ManagerCalls::default();
    let (app, mut vcx, runtime) = open_app_at(cx, &folder, 1200., None);
    let listener = runtime
        .block_on(async { tokio::net::UnixListener::bind(&private) })
        .unwrap();
    let manager = (
        calls.clone(),
        folder.to_path_buf(),
        op.runtime.to_path_buf(),
    );
    runtime.spawn(async move {
        let mut kept = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            let (calls, app, operator) = manager.clone();
            kept.push(
                zbus::connection::Builder::unix_stream(stream)
                    .server(zbus::Guid::generate())
                    .unwrap()
                    .p2p()
                    .serve_at(
                        "/org/freedesktop/systemd1",
                        Manager {
                            calls,
                            app,
                            operator,
                        },
                    )
                    .unwrap()
                    .build()
                    .await
                    .unwrap(),
            );
        }
    });
    let cx = &mut vcx;
    wait(cx, &app, "the offline card", |a| a.state.offline.is_some());
    let why = app.read_with(cx, |a, _| a.state.offline.clone().unwrap());
    assert!(
        why.starts_with("legionctl-operator is off (no answer on ")
            && why.ends_with(
                "turn it on with `systemctl --user enable --now legionctl-operator.socket`"
            ),
        "{why}"
    );
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("turn-on", cx);
    });
    // a failed start says why in the card, and the card stays
    wait(cx, &app, "the failed start", |a| a.state.turn_on.is_some());
    app.read_with(cx, |a, _| {
        assert_eq!(
            a.state.turn_on.as_deref(),
            Some("Turn on failed: org.freedesktop.DBus.Error.Failed: Unit legionctl-operator.socket is masked.")
        );
        assert!(a.state.offline.is_some());
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("turn-on", cx);
    });
    wait(cx, &app, "the operator answers", |a| {
        a.state.offline.is_none() && a.state.turn_on.is_none() && a.screen.restored
    });
    assert_eq!(
        *calls.lock().unwrap(),
        [
            "EnableUnitFiles legionctl-operator.socket false false",
            "Reload",
            "StartUnit legionctl-operator.socket replace",
        ]
        .repeat(2)
    );
    drop(runtime);
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

/// The top and the bottom of the element `id` in the last frame.
fn rows_of(cx: &mut VisualTestContext, id: &'static str) -> (f32, f32) {
    cx.update(|window, cx| {
        window.render_frame(cx);
        let b = window.find(id).bounds();
        (f32::from(b.origin.y), f32::from(b.origin.y + b.size.height))
    })
}

/// DESIGN-09 at 1000 px (it replaces the 7%/30% panes of P8): the rail is 56 px, the navigator 30% of the window
/// (300 px), the tab the rest. The chat bar belongs to the tab: below its content (it never covers the last row, U2),
/// centred, 60% of the tab's width; three lines make it taller, many lines stop at 40% of the tab; Ctrl+Right outside a
/// text field makes it 70%. A drag of the navigator's edge sets its width; a click on the space that shows folds the navigator.
#[gpui_kit::test]
fn the_app_rail_navigator_and_tab_have_their_sizes_and_the_chat_bar_never_covers_the_tab(
    cx: &mut TestAppContext,
) {
    let (op, _home, _repo) = thread_lab("panes");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1000., None);
    let cx = &mut vcx;
    wait(cx, &app, "the layout and the sensors", |a| {
        a.screen.restored && shown(a, "sensors").is_some()
    });
    let (xr, wr, _) = place(cx, "rail");
    let (xn, wn, _) = place(cx, "navigator");
    let (x3, w3, h3) = place(cx, "pane-3");
    near(xr, 0., "the rail starts at the left");
    near(wr, 56., "the rail is 56 px");
    near(xn, 56., "the navigator follows the rail");
    near(wn, 300., "the navigator is 30% of the window");
    near(x3, 356., "the tab follows the navigator");
    near(w3, 644., "the tab is the rest");

    // the chat bar: below the content, never over it, 60% of the tab's inner width, centred
    let (_, body_bottom) = rows_of(cx, "body");
    let (chat_top, _) = rows_of(cx, "chat-area");
    assert!(
        chat_top >= body_bottom - 0.5,
        "the chat bar starts below the content: {chat_top} < {body_bottom}"
    );
    let (xa, wa, _) = place(cx, "chat-area");
    let inner = wa - 48.;
    cx.update(|window, cx| window.click("chat", cx));
    let (xc, wc, hc1) = place(cx, "chat");
    // one line of text sits level with the agent chip
    let middle = |cx: &mut VisualTestContext, id: &'static str| {
        let (top, bottom) = rows_of(cx, id);
        f32::midpoint(top, bottom)
    };
    near(
        middle(cx, "chat-text"),
        middle(cx, "agent-chip"),
        "the text is level with the agent chip",
    );
    near(wc, inner * 0.6, "the chat bar is 60% of the tab");
    near(
        xc,
        xa + 24. + (inner - wc) / 2.,
        "the chat bar is centred in the tab",
    );
    cx.update(|window, cx| {
        window.input("one", cx);
        window.press("shift-enter", cx);
        window.input("two", cx);
        window.press("shift-enter", cx);
        window.input("three", cx);
    });
    app.read_with(cx, |a, _| assert_eq!(a.screen.chat(), "one\ntwo\nthree"));
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
    let (_, _, ha) = place(cx, "chat-area");
    assert!(
        ha <= (h3 - 36.) * 0.4 + 1.,
        "the chat bar stops at 40% of the tab ({h3}): {ha}"
    );
    let (_, body_bottom) = rows_of(cx, "body");
    let (chat_top, _) = rows_of(cx, "chat-area");
    assert!(
        chat_top >= body_bottom - 0.5,
        "a tall chat bar still starts below the content"
    );
    // in a text field Ctrl+Right moves by a word; outside one it widens the chat bar
    cx.update(|window, cx| {
        window.click_at("body", point(px(4.), px(4.)), cx);
        window.press("ctrl-right", cx);
    });
    near(
        place(cx, "chat").1,
        inner * 0.7,
        "Ctrl+Right makes the chat bar 70% of the tab",
    );

    // the navigator's edge: a drag to 40% of the window
    let (edge, to) = (356., 456.);
    cx.update(|window, cx| window.drag(point(px(edge), px(300.)), point(px(to), px(300.)), cx));
    near(
        place(cx, "navigator").1,
        400.,
        "the drag sets the navigator's width",
    );
    near(
        place(cx, "pane-3").0,
        to,
        "the tab follows the navigator's edge",
    );
    app.read_with(cx, |a, _| assert!(a.screen.drag.is_none()));
    // a click on the space that shows folds the navigator, and opens it again
    cx.update(|window, cx| window.click("space-machine", cx));
    near(
        place(cx, "pane-3").0,
        56.,
        "a folded navigator gives the tab its width",
    );
    cx.update(|window, cx| window.click("space-machine", cx));
    near(place(cx, "navigator").1, 400., "it opens with its width");
    drop(runtime);
}

/// The app's layout is the user's facts (DESIGN-09 Rule 1, DESIGN-10): tabs open from the navigator, close, move by a
/// drag onto another tab; the space and the navigator's width change. A new app on the same operator (a restart)
/// shows the same tabs in the same order with the same active tab, the same space and width, and `facts at` names
/// the user as their source.
#[gpui_kit::test]
fn tabs_the_space_and_the_navigator_width_persist_across_an_app_restart(cx: &mut TestAppContext) {
    let (op, home, repo) = thread_lab("tabs");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    {
        let cx = &mut vcx;
        wait(cx, &app, "the layout", |a| {
            a.screen.restored && a.screen.tabs.len() == 1
        });
        cx.update(|window, cx| window.click("nav-health", cx));
        cx.update(|window, cx| window.click("nav-bench", cx));
        app.read_with(cx, |a, _| {
            assert_eq!(a.screen.tabs.len(), 3);
            assert_eq!(a.screen.active, 2, "a new tab shows");
        });
        // close Machine now, then drag Bench onto Health
        cx.update(|window, cx| window.click("tab-close-0", cx));
        cx.update(|window, cx| window.drag_to("tab-1", "tab-0", cx));
        app.read_with(cx, |a, _| {
            assert_eq!(a.screen.tabs, [view("bench"), view("health")]);
            assert_eq!(a.screen.active, 0, "the moved tab stays active");
        });
        let (edge, to) = (56. + 1200. * 0.3, 56. + 1200. * 0.25);
        cx.update(|window, cx| window.drag(point(px(edge), px(300.)), point(px(to), px(300.)), cx));
        // the Agents space, then the agent session's thread as a tab
        cx.update(|window, cx| window.click("space-agents", cx));
        wait(cx, &app, "the threads", |a| !a.screen.threads.is_empty());
        cx.update(|window, cx| window.click("nav-thread-1", cx));
        wait(cx, &app, "the thread tab", |a| {
            a.screen.shown.contains_key(&1)
        });
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.drag_to("tab-2", "tab-1", cx);
        });
        app.read_with(cx, |a, _| {
            assert_eq!(
                a.screen.tabs[1],
                Tab::Thread { id: 1 },
                "{:?} {}",
                a.screen.tabs,
                a.screen.active
            );
        });
    }
    let want = [view("bench"), Tab::Thread { id: 1 }, view("health")];
    // the facts reach the operator: the user's
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let facts: serde_json::Value = serde_json::from_str(&lab_cli(
            &op,
            &home,
            &repo,
            &["facts", "at", "--entity", "app", "--winners", "--json"],
            "",
        ))
        .unwrap();
        let tabs = facts
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["attribute"] == "tabs")
            .cloned()
            .unwrap_or_default();
        if tabs["value"]["tabs"] == serde_json::to_value(&want).unwrap() {
            assert_eq!(tabs["source"], "user", "{tabs}");
            assert_eq!(tabs["value"]["active"], 1, "{tabs}");
            break;
        }
        assert!(Instant::now() < deadline, "the tabs fact: {facts:#}");
        // the app's writes run on its executor
        vcx.run_until_parked();
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(app);
    drop(vcx);
    drop(runtime);

    // a restart: the same layout
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    let cx = &mut vcx;
    wait(cx, &app, "the layout", |a| a.screen.restored);
    app.read_with(cx, |a, _| {
        // the command line's tab (Machine now) comes at the end of the kept ones
        assert_eq!(a.screen.tabs[..3], want, "{:?}", a.screen.tabs);
        assert_eq!(a.screen.space, Space::Agents);
        assert!(
            (a.screen.navigator - 0.25).abs() < 0.01,
            "{}",
            a.screen.navigator
        );
    });
    near(place(cx, "navigator").1, 300., "the kept width");
    drop(runtime);
}

/// The user's bugs of 2026-10-17: the chat bar has an input history, and the keyboard scrolls the tab. Up and Down
/// recall the lines sent, as a shell does: the text typed before the first Up comes back after the newest line. The
/// history is the user's facts, so a new app on the same operator (a restart) recalls the same lines. `PageDown`, End
/// and Home scroll the tab's body from the chat bar; Tab with nothing to complete moves the focus to the body, where
/// Down scrolls one line and Tab goes back to the chat bar.
#[gpui_kit::test]
fn the_chat_bar_recalls_its_lines_after_a_restart_and_the_keyboard_scrolls_the_tab(
    cx: &mut TestAppContext,
) {
    let (op, home, repo) = thread_lab("keys");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    {
        let cx = &mut vcx;
        wait(cx, &app, "the chat bar's agent", |a| {
            a.screen.agent().is_some()
        });
        cx.update(|window, cx| {
            window.click("chat", cx);
            window.input("/thread show 1", cx);
            window.press("enter", cx);
        });
        wait(cx, &app, "the first line's run", |a| {
            a.screen.runs.first().is_some_and(|r| r.1.is_some())
        });
        cx.update(|window, cx| {
            window.input("hello", cx);
            window.press("enter", cx);
            window.input("dr", cx);
        });
        let chat = |cx: &mut VisualTestContext, key: &str| {
            cx.update(|window, cx| window.press(key, cx));
            app.read_with(cx, |a, _| a.screen.chat().to_string())
        };
        assert_eq!(chat(cx, "up"), "hello");
        assert_eq!(chat(cx, "up"), "/thread show 1");
        assert_eq!(chat(cx, "up"), "/thread show 1", "the oldest line stays");
        assert_eq!(chat(cx, "down"), "hello");
        assert_eq!(chat(cx, "down"), "dr", "the typed text comes back");
        assert_eq!(chat(cx, "down"), "dr");
        cx.update(|window, cx| {
            for _ in 0..2 {
                window.press("backspace", cx);
            }
        });
        // the health seed is longer than the window
        cx.update(|window, cx| window.click("nav-health", cx));
        wait(cx, &app, "a tab that scrolls", |a| {
            a.body_scrolled().1 > 100.
        });
        let scrolled = |cx: &mut VisualTestContext, key: &str| {
            cx.update(|window, cx| {
                window.press(key, cx);
                window.render_frame(cx);
            });
            app.read_with(cx, |a, _| a.body_scrolled())
        };
        let (_, max) = app.read_with(cx, |a, _| a.body_scrolled());
        let (down, _) = scrolled(cx, "pagedown");
        assert!(down > 100. && down <= max, "PageDown: {down} of {max}");
        assert_eq!(scrolled(cx, "end").0, max, "End");
        assert_eq!(scrolled(cx, "home").0, 0., "Home");
        cx.update(|window, cx| window.click("chat", cx));
        assert_eq!(
            scrolled(cx, "tab").0,
            0.,
            "Tab moves the focus, it does not scroll"
        );
        // the body's padding: a click there gives it the keys
        cx.update(|window, cx| window.click_at("body", point(px(4.), px(4.)), cx));
        assert_eq!(
            scrolled(cx, "down").0,
            40.,
            "Down in the body scrolls one line"
        );
        cx.update(|window, cx| {
            window.click("chat", cx);
            window.input("x", cx);
        });
        app.read_with(cx, |a, _| {
            assert_eq!(
                a.screen.chat(),
                "x",
                "a click gives the keys back to the chat bar"
            );
        });
    }
    drop(app);
    drop(vcx);
    drop(runtime);

    // a restart: the same lines
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    let cx = &mut vcx;
    wait(cx, &app, "the kept lines", |a| {
        a.screen.history.lines.len() == 2
    });
    cx.update(|window, cx| {
        window.click("chat", cx);
        window.press("up", cx);
    });
    app.read_with(cx, |a, _| assert_eq!(a.screen.chat(), "hello"));
    drop(runtime);
}

/// The chat bar's text now.
fn chat_text(cx: &mut VisualTestContext, app: &Entity<LegionApp>) -> String {
    app.read_with(cx, |a, _| a.screen.chat().to_string())
}

/// What the clipboard holds now.
fn clipboard(cx: &mut VisualTestContext) -> Option<String> {
    cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
}

/// A click of the middle button on the element `id`.
fn middle_click(cx: &mut VisualTestContext, id: &'static str) {
    let at = cx.update(|window, cx| {
        window.render_frame(cx);
        window.find(id).bounds().center()
    });
    let plain = gpui_kit::Modifiers::default();
    cx.simulate_mouse_down(at, gpui_kit::MouseButton::Middle, plain);
    cx.simulate_mouse_up(at, gpui_kit::MouseButton::Middle, plain);
}

/// Press each key of `keys` in turn.
fn keys(cx: &mut VisualTestContext, keys: &[&str]) {
    cx.update(|window, cx| {
        for k in keys {
            window.press(k, cx);
        }
    });
}

/// todo-195 (1), user 2026-10-17: "I cannot even copy-paste into the app's text fields". The chat bar was the app's
/// own text (a key handler that only appended typed characters): no paste, no cursor, no selection. Now each text
/// field is the kit's input: Ctrl+V, Ctrl+Shift+V and Shift+Insert paste the clipboard, the middle button pastes the
/// primary selection, the right-click menu has Paste; Ctrl+A, Ctrl+C and Ctrl+X select, copy and cut; Home and the
/// arrows move the cursor, so a word goes in front of the text.
#[gpui_kit::test]
fn the_chat_bar_pastes_copies_cuts_and_edits_like_every_text_field_of_the_desktop(
    cx: &mut TestAppContext,
) {
    let op = unplugged_machine("paste");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., None);
    let cx = &mut vcx;
    let clip = |cx: &mut VisualTestContext, text: &str| {
        let item = gpui_kit::ClipboardItem::new_string(text.to_string());
        cx.update(|_, cx| cx.write_to_clipboard(item));
    };
    clip(cx, "fans at full speed");
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("chat", cx);
        window.press("ctrl-v", cx);
    });
    assert_eq!(chat_text(cx, &app), "fans at full speed", "Ctrl+V");
    keys(cx, &["home"]);
    cx.update(|window, cx| window.input("why ", cx));
    assert_eq!(
        chat_text(cx, &app),
        "why fans at full speed",
        "Home, then text"
    );
    keys(
        cx,
        &["end", "left", "left", "left", "left", "left", "backspace"],
    );
    assert_eq!(
        chat_text(cx, &app),
        "why fans at fullspeed",
        "the arrows move the cursor"
    );
    keys(cx, &["ctrl-a", "ctrl-x"]);
    assert_eq!(chat_text(cx, &app), "", "Ctrl+A, Ctrl+X");
    assert_eq!(clipboard(cx).as_deref(), Some("why fans at fullspeed"));
    clip(cx, "ab");
    keys(cx, &["ctrl-shift-v", "shift-insert"]);
    assert_eq!(chat_text(cx, &app), "abab", "Ctrl+Shift+V and Shift+Insert");
    clip(cx, "-");
    keys(cx, &["ctrl-a", "ctrl-c"]);
    assert_eq!(clipboard(cx).as_deref(), Some("abab"), "Ctrl+C");
    keys(cx, &["backspace"]);
    // the middle button: the primary selection of another app
    cx.update(|_, cx| {
        cx.write_to_primary(gpui_kit::ClipboardItem::new_string(
            "from the terminal".into(),
        ));
    });
    middle_click(cx, "chat");
    assert_eq!(
        chat_text(cx, &app),
        "from the terminal",
        "the middle button"
    );
    // the right button: the menu's Paste
    keys(cx, &["ctrl-a", "backspace"]);
    clip(cx, "menu");
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.right_click("chat", cx);
    });
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.find("popup-menu").visible(), "the right-click menu");
        window.within("popup-menu").click(2usize, cx);
    });
    assert_eq!(chat_text(cx, &app), "menu", "the menu's Paste");
    drop(runtime);
}

/// User 2026-10-17 on r340: "in the agent input fields, keyboard navigation, completions and input history do not
/// work". Live on r340 (headless GNOME Shell, EVIDENCE/validation/59-app-input): Up recalled and Tab completed, but the
/// arrows did not move in the list of commands, Enter sent the half-typed line instead of the picked command, Esc
/// did not close the list, the cursor could not move in the text, and Tab with nothing to complete moved the keys to
/// the tab's body, where typing did nothing. The earlier test typed only at the end and pressed Up on an empty bar,
/// so it passed. Now: Up and Down move in the list, Enter takes the pick, Esc closes the list (a change opens it
/// again); with no list, Up on the first line and Down on the last recall the lines sent; in a text of two lines Up
/// first moves to the line above; Tab with nothing to complete and Shift+Tab leave the bar and come back. Each
/// command shows once (the app's own `/dossier create` was listed again from the operations).
#[gpui_kit::test]
fn the_chat_bar_list_moves_with_the_arrows_enter_takes_the_pick_and_the_history_follows_the_lines(
    cx: &mut TestAppContext,
) {
    let op = unplugged_machine("list");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., None);
    let cx = &mut vcx;
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("chat", cx);
        window.input("/view health", cx);
        window.press("enter", cx);
    });
    assert_eq!(chat_text(cx, &app), "", "the line went");
    cx.update(|window, cx| window.input("/dos", cx));
    let entries: Vec<String> = app.read_with(cx, |a, _| {
        a.screen.completions().into_iter().map(|e| e.text).collect()
    });
    assert!(entries.len() >= 3, "{entries:?}");
    let mut once: Vec<&str> = entries.iter().map(|e| e.trim_end()).collect();
    once.sort_unstable();
    once.dedup();
    assert_eq!(once.len(), entries.len(), "each command once: {entries:?}");
    keys(cx, &["down", "down", "up"]);
    app.read_with(cx, |a, _| {
        assert_eq!(a.screen.list.pick, Some(0), "Down, Down, Up");
    });
    keys(cx, &["up"]);
    app.read_with(cx, |a, _| {
        assert_eq!(
            a.screen.list.pick,
            Some(entries.len().min(12) - 1),
            "Up wraps to the last"
        );
    });
    keys(cx, &["down", "down"]);
    assert_eq!(
        chat_text(cx, &app),
        "/dos",
        "the arrows move in the list, not in the history"
    );
    keys(cx, &["enter"]);
    assert_eq!(
        chat_text(cx, &app),
        format!("{} ", entries[1].trim_end()),
        "Enter takes the pick and sends nothing"
    );
    keys(cx, &["escape"]);
    app.read_with(cx, |a, _| {
        assert!(a.screen.completions().is_empty(), "Esc closes the list");
    });
    keys(cx, &["up"]);
    assert_eq!(
        chat_text(cx, &app),
        "/view health",
        "with no list, Up recalls"
    );
    keys(cx, &["down"]);
    assert_eq!(
        chat_text(cx, &app),
        format!("{} ", entries[1].trim_end()),
        "Down past the newest gives back the typed text"
    );
    keys(cx, &["ctrl-a", "backspace"]);
    cx.update(|window, cx| {
        window.input("one", cx);
        window.press("shift-enter", cx);
        window.input("two", cx);
        window.press("up", cx);
        window.input("+", cx);
    });
    assert_eq!(
        chat_text(cx, &app),
        "one+\ntwo",
        "Up on the second line moves the cursor"
    );
    keys(cx, &["ctrl-j"]);
    assert_eq!(chat_text(cx, &app), "one+\n\ntwo", "Ctrl+J is a new line");
    keys(cx, &["ctrl-a", "backspace", "tab"]);
    cx.update(|window, cx| window.input("x", cx));
    assert_eq!(
        chat_text(cx, &app),
        "",
        "Tab with nothing to complete leaves the bar"
    );
    keys(cx, &["shift-tab"]);
    cx.update(|window, cx| window.input("y", cx));
    assert_eq!(chat_text(cx, &app), "y", "Shift+Tab comes back");
    drop(runtime);
}

/// todo-195 (3, 4): every control is reachable with the keys. Tab from the chat bar goes through each control of the
/// window and comes back; Shift+Tab goes the same way back. The app's own controls (the rail's spaces, the
/// navigator's entries, the tabs and their close buttons, the agent chip, the dossier chip) are buttons with a name
/// for a screen reader, and Enter presses the one that has the keys: Shift+Tab from the rail's Work goes to Machine,
/// Enter shows it; Shift+Tab from the chat bar is the agent chip, Enter starts `/agent` in the bar.
#[gpui_kit::test]
fn the_keyboard_reaches_each_control_in_one_order_and_each_has_a_name(cx: &mut TestAppContext) {
    let op = unplugged_machine("order");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., None);
    let cx = &mut vcx;
    let focused = |cx: &mut VisualTestContext| cx.update(|window, cx| window.focused(cx));
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("chat", cx);
    });
    let start = focused(cx).expect("the chat bar has the keys");
    let walk = |cx: &mut VisualTestContext, key: &str| {
        let mut stops = Vec::new();
        loop {
            keys(cx, &[key]);
            let now = focused(cx).expect("a control has the keys");
            if now == start {
                return stops;
            }
            stops.push(now);
            assert!(stops.len() < 300, "{key} never comes back to the chat bar");
        }
    };
    let forward = walk(cx, "tab");
    let mut back = walk(cx, "shift-tab");
    back.reverse();
    assert_eq!(forward, back, "Shift+Tab goes the way of Tab back");
    let named = cx.update(|window, cx| {
        window.render_frame(cx);
        let mut ids: Vec<String> = [
            "space-machine",
            "space-work",
            "space-agents",
            "space-activity",
        ]
        .map(String::from)
        .into();
        ids.extend(["nav-health", "tab-0", "tab-close-0", "agent-chip", "place"].map(String::from));
        for id in &ids {
            let e = window.find(SharedString::from(id.clone()));
            assert_eq!(e.role(), Some(gpui_kit::Role::Button), "{id}");
            assert!(e.label().is_some_and(|l| !l.is_empty()), "{id} has a name");
        }
        ids.len()
    });
    assert!(
        forward.len() >= named,
        "{} stops for {named} controls",
        forward.len()
    );
    cx.update(|window, cx| window.click("space-work", cx));
    app.read_with(cx, |a, _| assert_eq!(a.screen.space, Space::Work));
    keys(cx, &["shift-tab", "enter"]);
    app.read_with(cx, |a, _| {
        assert_eq!(a.screen.space, Space::Machine, "Shift+Tab, Enter");
    });
    keys(cx, &["tab", "tab", "space"]);
    app.read_with(cx, |a, _| {
        assert_eq!(a.screen.space, Space::Agents, "Tab, Tab, Space");
    });
    cx.update(|window, cx| {
        window.click("chat", cx);
        window.press("shift-tab", cx);
        window.press("enter", cx);
    });
    assert_eq!(chat_text(cx, &app), "/agent ", "the agent chip by the keys");
    drop(runtime);
}

/// A private session bus in its own folder: the folder, the bus process and its address.
fn private_bus(tag: &str) -> (Scratch, common::process::Kill, String) {
    let dir = Scratch::new(format!("/var/tmp/lc-{tag}-bus-{}", std::process::id()));
    let path = dir.join("bus");
    let bus = common::process::Kill::spawn(
        std::process::Command::new("dbus-daemon")
            .args(["--session", "--nofork", "--nopidfile"])
            .arg(format!("--address=unix:path={}", path.display())),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !path.exists() {
        assert!(Instant::now() < deadline, "the private bus");
        std::thread::sleep(Duration::from_millis(50));
    }
    let address = format!("unix:path={}", path.display());
    (dir, bus, address)
}

/// A mouse selection across the element `id`, then Ctrl+C: what the clipboard has (it had "-" before).
fn select_and_copy(cx: &mut VisualTestContext, id: &'static str) -> Option<String> {
    cx.update(|_, cx| cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("-".into())));
    let cell = cx.update(|window, cx| {
        window.render_frame(cx);
        window.find(id).bounds()
    });
    let y = cell.origin.y + cell.size.height / 2.;
    let (from, to) = (
        point(cell.origin.x + px(1.), y),
        point(cell.origin.x + cell.size.width - px(1.), y),
    );
    // the selection reads the text's place in each frame: a frame after each step
    let plain = gpui_kit::Modifiers::default();
    cx.simulate_mouse_down(from, gpui_kit::MouseButton::Left, plain);
    cx.update(TestWindowExt::render_frame);
    cx.simulate_mouse_move(to, Some(gpui_kit::MouseButton::Left), plain);
    cx.update(TestWindowExt::render_frame);
    cx.simulate_mouse_up(to, gpui_kit::MouseButton::Left, plain);
    cx.update(|window, cx| window.press("ctrl-c", cx));
    cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))
}

/// The files of the feedback entry `entry` of the test below: `first` is the first row of its view.
fn feedback_entry(entry: &Path, first: &str, png: &[u8]) {
    let note = std::fs::read_to_string(entry.join("note.md")).unwrap();
    for line in [
        "class: feedbacks",
        "immutable: true",
        "daemon: does not answer",
        "space: Machine",
        "tab: view:resources:",
        "view: resources version 0",
        "links: [state.json, screenshot.png]",
        "# Feedback\n\nFans row is wrong\n",
    ] {
        assert!(note.contains(line), "{line} in {note}");
    }
    assert!(
        note.contains("\noperator: ") && note.contains("\napp: ") && note.contains("\ntime: 20"),
        "{note}"
    );
    let state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(entry.join("state.json")).unwrap()).unwrap();
    assert_eq!(state["view"]["name"], "resources", "{state}");
    assert_eq!(
        state["view"]["rows"][0][0],
        first.split('\t').next().unwrap()
    );
    assert!(
        state["events"].as_array().is_some_and(|e| !e.is_empty()),
        "{state}"
    );
    assert_eq!(std::fs::read(entry.join("screenshot.png")).unwrap(), png);
}

/// todo-168: the text on display is selectable and copyable, and the user's feedback becomes an immutable dossier
/// entry that agents read. A table row's copy button puts its cells on the clipboard. `/feedback <note>` (and the
/// title bar's Feedback, which puts `/feedback ` in the chat bar) asks the desktop portal for a screenshot (the user
/// picks the window: interactive), and keeps feedbacks/0001-<slug>/ in the chat bar thread's dossier: note.md with
/// its front matter (time, the app, operator and daemon builds, space, tab, view and version, links), state.json (the
/// tab's view with its data and the last events) and screenshot.png (the portal's picture; its file is removed). The
/// call is the event feedback.new; a later write to the entry (its state.json) is refused.
#[gpui_kit::test]
fn copy_puts_a_row_on_the_clipboard_and_feedback_keeps_the_note_state_and_screenshot_in_the_dossier(
    cx: &mut TestAppContext,
) {
    const PNG: &[u8] = b"\x89PNG stand-in picture";
    let (bus_dir, _bus, address) = private_bus("feedback");
    let bus_runtime = tokio::runtime::Runtime::new().unwrap();
    let (_portal, asked) = bus_runtime.block_on(portal::screenshots(&address, &bus_dir, PNG));
    let (op, home, repo) = thread_lab_on(
        "feedback",
        &[("DBUS_SESSION_BUS_ADDRESS", address.clone().into())],
    );
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    let cx = &mut vcx;
    cx.update(|window, cx| window.click("nav-resources", cx));
    wait(cx, &app, "the resources rows", |a| {
        a.screen
            .view()
            .is_some_and(|v| v.name == "resources" && !v.rows.is_empty())
    });
    let first = app.read_with(cx, |a, _| a.screen.view().unwrap().rows[0].join("\t"));
    let selected = select_and_copy(cx, "view-resources-row-0-cell-1");
    let state = app.read_with(cx, |a, _| a.screen.view().unwrap().rows[0][1].clone());
    assert_eq!(
        selected.as_deref(),
        Some(state.as_str()),
        "the selected text"
    );
    cx.update(|window, cx| window.click("view-resources-row-0-copy", cx));
    let copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()));
    assert_eq!(copied.as_deref(), Some(first.as_str()), "the row's cells");

    cx.update(|window, cx| window.click("feedback", cx));
    app.read_with(cx, |a, _| assert_eq!(a.screen.chat(), "/feedback "));
    cx.update(|window, cx| {
        window.input("Fans row is wrong", cx);
        window.press("enter", cx);
    });
    wait(cx, &app, "the feedback", |a| {
        a.state
            .message
            .as_deref()
            .is_some_and(|m| m.starts_with("feedback kept in"))
    });
    let (dossier, shown) = app.read_with(cx, |a, _| {
        (
            a.screen
                .thread
                .as_ref()
                .map(|t| t.thread.dossier.clone())
                .or(a.screen.dossier.clone())
                .unwrap(),
            a.state.message.clone().unwrap(),
        )
    });
    let rel = format!("{dossier}/feedbacks/0001-fans-row-is-wrong");
    assert_eq!(shown, format!("feedback kept in {rel}"));
    let entry = home.join("agentWorkspaces").join(&rel);
    feedback_entry(&entry, &first, PNG);
    let asked = asked.lock().unwrap().clone();
    assert_eq!(asked.len(), 1);
    assert!(asked[0].0, "the user picks the window");
    assert!(!asked[0].1.exists(), "the portal's file is removed");
    let events: serde_json::Value =
        serde_json::from_str(&op.ok(&["events", "--limit", "1000", "--json"])).unwrap();
    assert!(
        events["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["kind"] == "feedback.new"
                && e["actor"] == "app"
                && e["payload"]["input"]["note"] == "Fans row is wrong"),
        "{events}"
    );
    // an agent session on the entry's dossier
    let agent = [
        ("HOME", home.to_str().unwrap()),
        ("LEGIONCTL_SESSION", "reader"),
    ];
    let used = op.run(&repo, &agent, &["dossier", "use", &dossier], "");
    assert!(used.status.success(), "{used:?}");
    let refused = op.run(
        &repo,
        &agent,
        &[
            "dossier",
            "write",
            "--overwrite",
            &format!("{rel}/state.json"),
        ],
        "{}\n",
    );
    assert!(
        !refused.status.success()
            && String::from_utf8_lossy(&refused.stderr).contains("immutable once written"),
        "{refused:?}"
    );
    drop(runtime);
}

/// todo-184: the Help menu's FAQ shows every topic, and each topic names a view or an operation that exists, so a
/// topic of a removed or renamed feature fails here (the FAQ is collateral of the features, as the skills are).
#[gpui_kit::test]
fn every_help_topic_shows_and_names_a_feature_that_exists(cx: &mut TestAppContext) {
    let (op, home, repo) = thread_lab("faq");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    let cx = &mut vcx;
    wait(cx, &app, "the views", |a| !a.screen.views.is_empty());
    cx.update(|window, cx| window.click("help", cx));
    cx.update(|window, cx| window.within("popup-menu").click(3usize, cx));
    app.read_with(cx, |a, _| {
        let shown = a.state.message.clone().unwrap_or_default();
        for topic in legionctl_app::help::FAQ {
            assert!(
                shown.contains(topic.question),
                "{} in {shown}",
                topic.question
            );
            for feature in topic.features {
                let exists = match *feature {
                    legionctl_app::help::Feature::View(name) => {
                        a.screen.views.iter().any(|v| v.name == name)
                    }
                    legionctl_app::help::Feature::Op(name) => legionctl_api::OPS
                        .iter()
                        .any(|o| o.name == name && !o.internal()),
                };
                assert!(
                    exists,
                    "Help topic {:?} names {feature:?}, which the app does not have",
                    topic.question
                );
            }
        }
    });
    drop(runtime);
}

/// todo-169, todo-184: the title bar's Help menu opens About as a tab (the seed view of the read operation `about`:
/// legionctl's version, a machine daemon that does not answer, the machine, the licenses, the source and the build
/// details) and shows the keys; Ctrl+K is the command palette (the chat bar with `/`, which lists the views and
/// the operations). A view's `ms` and `bytes` formats show a duration and a size; a format outside the set is refused.
#[gpui_kit::test]
fn help_opens_about_and_the_keys_ctrl_k_searches_and_numbers_show_by_their_unit(
    cx: &mut TestAppContext,
) {
    let (op, home, repo) = thread_lab("about");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    let cx = &mut vcx;
    cx.update(|window, cx| window.click("help", cx));
    cx.update(|window, cx| window.within("popup-menu").click(0usize, cx));
    wait(cx, &app, "the About tab", |a| {
        a.screen
            .view()
            .is_some_and(|v| v.name == "about" && v.rows.len() == 6)
    });
    let rows = app.read_with(cx, |a, _| a.screen.view().unwrap().rows.clone());
    // a list row: part · what · state
    let row = |part: &str| {
        let line = rows
            .iter()
            .map(|r| r.join(" · "))
            .find(|r| r.starts_with(&format!("{part} · ")));
        line.unwrap_or_else(|| panic!("no {part} in {rows:?}"))
    };
    assert!(
        row("legionctl").starts_with("legionctl · 0.") && row("legionctl").ends_with(" · ok"),
        "{rows:?}"
    );
    assert!(
        row("machine daemon").contains("legionctl-system")
            && row("machine daemon").ends_with(" · warn"),
        "{rows:?}"
    );
    assert!(
        row("machine").contains("profile lenovo-82wq-legion-pro-7-16irx8h"),
        "{rows:?}"
    );
    assert!(row("licenses").contains("/usr/share/licenses/legionctl"));
    // the build hashes are in the details row only
    assert!(row("details").contains("API build "), "{rows:?}");

    cx.update(|window, cx| window.click("help", cx));
    cx.update(|window, cx| window.within("popup-menu").click(2usize, cx));
    app.read_with(cx, |a, _| {
        assert!(
            a.state
                .message
                .as_deref()
                .is_some_and(|m| m.starts_with("Ctrl+K: search"))
        );
    });
    cx.update(|window, cx| window.press("ctrl-k", cx));
    app.read_with(cx, |a, _| {
        assert_eq!(a.screen.chat(), "/");
        let all: Vec<String> = a.screen.completions().into_iter().map(|e| e.text).collect();
        for want in ["/view about", "/about", "/feedback "] {
            assert!(all.iter().any(|e| e == want), "{want} in {all:?}");
        }
    });
    cx.update(|window, cx| window.input("x", cx));
    app.read_with(cx, |a, _| {
        assert_eq!(a.screen.chat(), "/x", "the chat bar takes the keys");
    });
    drop(runtime);

    let view = |formats: &str| {
        format!(
            r#"{{"name": "event-times", "widget": "table", "source": {{"op": "events", "input": {{"limit": 1000}},
                "path": "$.items"}}, "columns": ["kind", "payload.ms", "id"], "formats": {formats}}}"#
        )
    };
    write(
        &home.join(".config/legionctl/views/event-times.jsonc"),
        &view(r#"{"payload.ms": "ms", "id": "bytes"}"#),
    );
    let shown = lab_cli(
        &op,
        &home,
        &repo,
        &["views", "render", "event-times", "--json"],
        "",
    );
    let shown: serde_json::Value = serde_json::from_str(&shown).unwrap();
    let rows = shown["rows"].as_array().unwrap();
    assert!(
        rows.iter()
            .filter(|r| r[1] != "")
            .all(|r| r[1].as_str().unwrap().ends_with(" ms")
                || r[1].as_str().unwrap().ends_with(" s")),
        "{rows:?}"
    );
    assert_eq!(rows[0][2], "1 B", "{rows:?}");
    let refused = op.run(
        &repo,
        &[("HOME", home.to_str().unwrap())],
        &["views", "check"],
        &view(r#"{"id": "megabytes"}"#),
    );
    assert!(
        !refused.status.success()
            && String::from_utf8_lossy(&refused.stderr)
                .contains("id has the format megabytes, which is not one of ago, time, int, 1dp, count, join, bytes, ms"),
        "{refused:?}"
    );
}

/// A thread tab says what each row of the thread's log means (todo-127, A3): a watch rule that a member adds reaches
/// the app through the event stream and reads as a sentence in the app; no row is raw JSON.
#[gpui_kit::test]
fn the_app_thread_pane_shows_a_watch_rule_as_a_sentence(cx: &mut TestAppContext) {
    let (op, home, repo) = thread_lab("rule");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1000., None);
    let cx = &mut vcx;
    wait(cx, &app, "the threads", |a| !a.screen.threads.is_empty());
    // the thread is a tab (DESIGN-09): the Agents space lists an agent session's thread
    cx.update(|window, cx| window.click("space-agents", cx));
    cx.update(|window, cx| window.click("nav-thread-1", cx));
    wait(cx, &app, "the thread tab", |a| {
        a.screen.shown.contains_key(&1)
    });
    lab_cli(
        &op,
        &home,
        &repo,
        &[
            "thread",
            "rules",
            "add",
            "1",
            "observer0",
            "make lint",
            "--path",
            "crates/",
        ],
        "",
    );
    wait(cx, &app, "the rule's row", |a| {
        a.screen
            .thread
            .as_ref()
            .is_some_and(|t| t.log.iter().any(|r| r.kind == "thread.rule"))
    });
    app.read_with(cx, |a, _| {
        let log = &a.screen.thread.as_ref().unwrap().log;
        let rule = log.iter().find(|r| r.kind == "thread.rule").unwrap();
        assert_eq!(
            legionctl_api::chat::log_detail(rule),
            "reminder: after 4 Edit/Write/MultiEdit/NotebookEdit in crates/, expect make lint"
        );
        for row in log {
            let detail = legionctl_api::chat::log_detail(row);
            assert!(!detail.contains('{'), "{}: {detail}", row.kind);
        }
    });
    let rule = app.read_with(cx, |a, _| {
        a.screen.shown[&1]
            .log
            .iter()
            .find(|r| r.kind == "thread.rule")
            .unwrap()
            .id
    });
    assert!(cx.update(|window, cx| {
        window.render_frame(cx);
        window.find(format!("log-{rule}")).visible()
    }));
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
    // the chat bar's agent (config app.agent) comes with the first read
    wait(cx, &app, "the chat bar's agent", |a| {
        a.screen.agent().is_some()
    });
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
    // an agent's command in a view tab starts a chat: the chat's run has its own folder, never the app's
    wait(cx, &app, "the agent command's chat", |a| {
        a.screen.runs.len() == 2
            && a.screen.runs[1].1.is_some()
            && !said(a, "thread.answer").is_empty()
    });
    app.read_with(cx, |a, _| {
        assert!(matches!(a.tab(), Some(Tab::Thread { .. })), "{:?}", a.tab());
        let answer = said(a, "thread.answer").join("\n");
        assert!(answer.contains("prompt: /plan x"), "{answer}");
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
        // "hello" is the composer's change of Machine now: done, the lab's machine has no sensors for some parts
        assert_eq!(
            runs[1..],
            [
                ("/agent:plan x", Some(true)),
                ("/thread show 1", Some(true))
            ]
        );
        assert_eq!(runs[0].0, "hello");
        assert!(runs[0].1.is_some());
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

/// todo-144, todo-174: the chat bar picks its agent. It starts with config app.agent (`fake`); `/agent ` offers each
/// name of session.acp (the defaults' claude and codex, the lab's fake and other); `/agent nobody` is refused with the
/// names; outside a thread `/agent other` is this screen's pick, and a new chat keeps it: its thread starts with a
/// `thread.agent` row of other, and the answer comes from other.
#[gpui_kit::test]
fn the_app_chat_bar_picks_its_agent_from_session_acp_and_a_new_chat_keeps_the_pick_in_its_thread(
    cx: &mut TestAppContext,
) {
    let (op, home, repo) = thread_lab("pick");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    let cx = &mut vcx;
    wait(cx, &app, "the agents", |a| !a.screen.agents.is_empty());
    app.read_with(cx, |a, _| {
        assert_eq!(a.screen.agent().as_deref(), Some("fake"));
    });
    cx.update(|window, cx| {
        window.click("chat", cx);
        window.input("/agent ", cx);
    });
    app.read_with(cx, |a, _| {
        let entries: Vec<(String, String)> = a
            .screen
            .completions()
            .into_iter()
            .map(|e| (e.text, e.about))
            .collect();
        assert_eq!(
            entries,
            [
                ("/agent claude", "Send your messages to claude"),
                ("/agent codex", "Send your messages to codex"),
                ("/agent fake", "Send your messages to fake (now)"),
                ("/agent other", "Send your messages to other"),
            ]
            .map(|(t, a)| (t.to_string(), a.to_string()))
        );
    });
    cx.update(|window, cx| {
        window.input("nobody", cx);
        window.press("enter", cx);
    });
    app.read_with(cx, |a, _| {
        let why = a.state.message.clone().unwrap_or_default();
        assert!(
            why.contains("No agent named nobody. The agents are: claude, codex, fake, other"),
            "{why}"
        );
        assert_eq!(a.screen.agent().as_deref(), Some("fake"));
    });
    cx.update(|window, cx| {
        window.input("/agent other", cx);
        window.press("enter", cx);
        window.input("/chat", cx);
        window.press("enter", cx);
        window.input("hello", cx);
        window.press("enter", cx);
    });
    wait(cx, &app, "the chat's answer", |a| {
        a.screen
            .thread
            .as_ref()
            .is_some_and(|t| t.log.iter().any(|r| r.kind == "thread.answer"))
    });
    let id = app.read_with(cx, |a, _| {
        let t = a.screen.thread.as_ref().unwrap();
        let answer = t.log.iter().find(|r| r.kind == "thread.answer").unwrap();
        let text = answer.payload["text"].as_str().unwrap_or_default();
        assert!(
            text.contains("prompt: hello") && text.contains("agent: other"),
            "{text}"
        );
        assert_eq!(a.screen.agent().as_deref(), Some("other"));
        t.thread.id
    });
    let log: serde_json::Value = serde_json::from_str(&lab_cli(
        &op,
        &home,
        &repo,
        &["thread", "show", &id.to_string(), "--json"],
        "",
    ))
    .unwrap();
    assert!(
        log["log"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["kind"] == "thread.agent" && r["payload"]["agent"] == "other"),
        "{log:#}"
    );
    drop(runtime);
}

/// The user's text of a chat message of `kind` in the thread that the tab shows.
fn said(a: &LegionApp, kind: &str) -> Vec<String> {
    a.screen
        .thread
        .iter()
        .flat_map(|t| t.log.iter())
        .filter(|r| r.kind == kind)
        .map(|r| {
            r.payload["text"]
                .as_str()
                .or(r.payload["error"].as_str())
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// Type `line` into the chat bar and send it.
fn send(cx: &mut VisualTestContext, line: &str) {
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("chat", cx);
        window.input(line, cx);
        window.press("enter", cx);
    });
}

/// A new chat with no dossier: the picker in place (flags in words), "Hi" says to choose one there with no command
/// in the text, and `/dossier add` is no completion (an agent's work).
fn the_new_chat_has_the_dossier_picker(cx: &mut VisualTestContext, app: &Entity<LegionApp>) {
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("space-work", cx);
        window.render_frame(cx);
        window.click("nav-new-chat", cx);
    });
    wait(cx, app, "the dossier picker", |a| {
        a.screen
            .view_data
            .get("view:dossiers:")
            .is_some_and(|v| v.rows.len() == 2)
    });
    app.read_with(cx, |a, _| {
        let flags: Vec<&str> = a.screen.view_data["view:dossiers:"]
            .rows
            .iter()
            .map(|r| r[1].as_str())
            .collect();
        assert_eq!(flags, ["no", "no"], "a flag in words (r324: false)");
    });
    send(cx, "Hi");
    app.read_with(cx, |a, _| {
        let why = a.state.message.clone().unwrap_or_default();
        assert!(
            why.starts_with("Choose a dossier first") && !why.contains('/'),
            "{why}"
        );
        assert_eq!(a.tab(), Some(&Tab::Chat), "the picker is in the chat");
    });
    // the result belongs to its tab (r324: a tab's error showed in every tab after it)
    app.update(cx, |a, cx| a.open_tab(view("machine-now"), cx));
    app.read_with(cx, |a, _| assert_eq!(a.state.message, None));
    app.update(cx, |a, cx| a.open_tab(Tab::Chat, cx));
    cx.update(|window, cx| {
        window.click("chat", cx);
        window.input("/dossier ", cx);
    });
    app.read_with(cx, |a, _| {
        let offered: Vec<String> = a.screen.completions().into_iter().map(|e| e.text).collect();
        assert!(offered.iter().any(|e| e == "/dossier use"), "{offered:?}");
        assert!(
            !offered
                .iter()
                .any(|e| e == "/dossier add" || e == "/dossier edit"),
            "an agent's work is no command of the app: {offered:?}"
        );
    });
    cx.update(|window, cx| window.press("escape", cx));
    app.update(cx, |a, _| a.screen.chat_mut().clear());
}

/// No dossier (todo-179 1, 4; G-5, G-6, G-7): a new chat shows the dossier picker in place, and "Hi" says to choose
/// one there, with no command in the text. `/dossier add` is no completion (an agent's work). Use on a row makes it
/// the user's dossier (r324: the button sent the relative folder "." and the operator refused it). Release, then New
/// dossier (the form of dossier.create) with no folder: the confirm says what happens in words, with the name as a
/// labelled value, the raw input only behind Details, and no folder (r324: the app's own folder, the home); Apply
/// makes appreview, and the app uses it.
fn make_a_dossier_in_the_app(
    cx: &mut VisualTestContext,
    app: &Entity<LegionApp>,
    op: &Operator,
    home: &Path,
    repo: &Path,
) {
    the_new_chat_has_the_dossier_picker(cx, app);
    let first = app.read_with(cx, |a, _| {
        a.screen.view_data["view:dossiers:"].rows[0][0].clone()
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        // Use is a change of the user's own session: it runs at once, with no confirm
        window.click("view-dossiers-row-0-action-0", cx);
    });
    app.read_with(cx, |a, _| assert!(a.state.pending.is_none(), "no confirm"));
    wait(cx, app, "the row's dossier in use", |a| {
        a.screen.dossier.as_deref() == Some(first.as_str())
    });
    send(cx, "/dossier release");
    wait(cx, app, "no dossier again", |a| a.screen.dossier.is_none());
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("view-dossiers-action-0", cx);
    });
    app.read_with(cx, |a, _| {
        let form = a.state.form.as_ref().expect("the form of dossier.create");
        assert!(form.fields.iter().any(|f| f.name == "name" && f.required));
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("form-field-name", cx);
        window.input("appreview", cx);
        window.click("form-submit", cx);
    });
    app.read_with(cx, |a, _| {
        let pending = a.state.pending.as_ref().expect("an inline confirm");
        assert_eq!(pending.class, legionctl_api::Class::Workspace);
        assert_eq!(pending.button.input["name"], "appreview");
        assert!(
            pending.button.input.get("folder").is_none(),
            "no folder given, none linked: {}",
            pending.button.input
        );
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(
            window.find("pending-values").visible(),
            "the input as labelled values"
        );
        assert!(
            window.try_find("pending-raw").is_none(),
            "the raw input is behind Details"
        );
        window.click("pending-details", cx);
        window.render_frame(cx);
        assert!(window.find("pending-raw").visible());
        window.click("apply", cx);
    });
    wait(cx, app, "the new dossier in use", |a| {
        a.screen.dossier.as_deref() == Some("appreview")
    });
    assert!(home.join("agentWorkspaces/appreview/plan.md").is_file());
    let listed = user_cli(op, home, repo, &["dossier", "list"]);
    assert!(
        listed
            .lines()
            .any(|l| l.starts_with("appreview") && l.ends_with("(your dossier)")),
        "{listed}"
    );
}

/// Wait until the layout fact of the app keeps the tab of thread `id`.
fn wait_for_the_tab_fact(
    cx: &mut VisualTestContext,
    op: &Operator,
    home: &Path,
    repo: &Path,
    id: i64,
) {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        let facts = user_cli(
            op,
            home,
            repo,
            &["facts", "at", "--entity", "app", "--winners", "--json"],
        );
        let facts: serde_json::Value = serde_json::from_str(&facts).unwrap();
        let kept = facts.as_array().unwrap().iter().any(|f| {
            f["attribute"] == "tabs"
                && f["value"]["tabs"]
                    .as_array()
                    .is_some_and(|t| t.contains(&serde_json::json!({"kind": "thread", "id": id})))
        });
        if kept {
            break;
        }
        assert!(Instant::now() < deadline, "the tabs fact: {facts}");
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// "Change where the chat works" in the thread tab: the environment step (thread.repo) (the repository given or `prefilled` with the
/// dossier's), base main, the inline confirm; then the thread has its branch.
fn attach_repository(
    cx: &mut VisualTestContext,
    app: &Entity<LegionApp>,
    repo: &Path,
    prefilled: bool,
) {
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("thread-repo", cx);
    });
    wait(cx, app, "the form of thread.repo", |a| {
        a.state.form.is_some()
    });
    // the second chat's form has the dossier's repository (the first chat's) already
    let given = app.read_with(cx, |a, _| {
        a.state.form.as_ref().unwrap().values.get("repo").cloned()
    });
    assert_eq!(
        given.as_deref(),
        prefilled.then(|| repo.to_str().unwrap()),
        "the dossier's repository first"
    );
    cx.update(|window, cx| {
        window.render_frame(cx);
        if given.is_none() {
            window.click("form-field-repo", cx);
            window.input(repo.to_str().unwrap(), cx);
        }
        window.click("form-field-base", cx);
        window.input("main", cx);
        window.click("form-submit", cx);
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("apply", cx);
    });
    wait(cx, app, "the thread's branch", |a| {
        a.screen
            .thread
            .as_ref()
            .is_some_and(|t| t.thread.branch.is_some())
    });
}

/// Feedback with no dossier asks for one and keeps the note in the chat bar; nothing goes into a dossier
/// (EV-review-12: it went into a dossier named legionctl).
#[gpui_kit::test]
fn feedback_with_no_dossier_asks_for_one_and_keeps_the_note(tcx: &mut TestAppContext) {
    let (op, home, repo) = thread_lab("nofeedback");
    user_cli(&op, &home, &repo, &["dossier", "release"]);
    let (app, mut vcx, runtime) = open_app(tcx, &op, 1400., Some((&home, &repo)));
    let cx = &mut vcx;
    wait(cx, &app, "the threads", |a| !a.screen.threads.is_empty());
    app.read_with(cx, |a, _| assert_eq!(a.screen.dossier, None));
    send(cx, "/feedback Fans row is wrong");
    app.read_with(cx, |a, _| {
        assert!(
            a.state
                .message
                .as_deref()
                .is_some_and(|m| m.starts_with("Choose a dossier for your feedback")),
            "{:?}",
            a.state.message
        );
        assert_eq!(a.screen.chat(), "/feedback Fans row is wrong");
    });
    assert!(!home.join("agentWorkspaces/legionctl").exists());
    drop(runtime);
}

/// todo-174, the user's path from no dossier to a kept conversation (the user's repro on r271: a dossier made, "Hi"
/// answered with "no dossier in use in this repo", and `/dossier add ... --create` did nothing). An agent session's
/// thread (thread 1 of session main) never opens by default and is not in the Work space. With no dossier chosen, the
/// title bar says so; "Hi" in a new chat says what a dossier is for, how to choose or make one, and opens the Dossiers
/// view; `/dossier add` says that an agent does it and names the commands that work. New dossier (the generated form
/// of dossier.create) makes appreview after an inline confirm, and the app uses it at once. "WAIT hi" starts the
/// user's own thread on appreview (no session: owner none), the answer shows in the tab as the agent writes it (the
/// stand-in holds after its first line), then whole; "again" goes on in the same ACP session. A second chat is a
/// second thread. After a restart both chat tabs are back with their conversations, and the title names the
/// dossier and the thread.
#[gpui_kit::test]
fn a_new_user_chooses_no_dossier_makes_one_in_the_app_and_keeps_two_chats_across_a_restart(
    tcx: &mut TestAppContext,
) {
    let (op, home, repo) = thread_lab("path");
    user_cli(&op, &home, &repo, &["dossier", "release"]);
    let (app, mut vcx, runtime) = open_app(tcx, &op, 1400., Some((&home, &repo)));
    let cx = &mut vcx;
    wait(cx, &app, "the threads and agents", |a| {
        !a.screen.threads.is_empty() && !a.screen.agents.is_empty()
    });
    app.read_with(cx, |a, _| {
        assert_eq!(a.screen.dossier, None);
        assert!(a.screen.thread.is_none(), "no thread opens by default");
        assert_eq!(a.screen.tabs, [view("machine-now")]);
        let lead = a.screen.threads.iter().find(|t| t.id == 1).unwrap();
        assert_eq!(
            lead.owner.as_deref(),
            Some("main"),
            "thread 1 is an agent session's"
        );
    });
    make_a_dossier_in_the_app(cx, &app, &op, &home, &repo);

    // the first message makes the user's thread; the answer comes as it is written
    app.update(cx, |a, cx| a.open_tab(Tab::Chat, cx));
    send(cx, "WAIT hi");
    wait(cx, &app, "the first line of the answer", |a| {
        a.screen
            .live
            .values()
            .any(|(_, text)| text.contains("prompt: WAIT hi"))
    });
    let first = app.read_with(cx, |a, _| {
        let Some(Tab::Thread { id }) = a.tab().cloned() else {
            panic!("the chat tab is its thread's tab: {:?}", a.tab());
        };
        let info = a.screen.threads.iter().find(|t| t.id == id).unwrap();
        assert_eq!(
            (info.dossier.as_str(), info.owner.as_deref()),
            ("appreview", None)
        );
        assert!(said(a, "thread.answer").is_empty(), "not whole yet");
        id
    });
    std::fs::write(home.join("release"), "").unwrap();
    wait(cx, &app, "the whole answer", |a| {
        !said(a, "thread.answer").is_empty() && a.screen.live.is_empty()
    });
    send(cx, "again");
    wait(cx, &app, "the second answer", |a| {
        said(a, "thread.answer").len() == 2
    });
    app.read_with(cx, |a, _| {
        assert_eq!(said(a, "thread.say"), ["WAIT hi", "again"]);
        let answers = said(a, "thread.answer");
        assert!(answers[0].contains("acp: new"), "{}", answers[0]);
        assert!(
            answers[1].contains("acp: loaded"),
            "the same conversation: {}",
            answers[1]
        );
    });
    app.update(cx, |a, cx| a.open_tab(Tab::Chat, cx));
    send(cx, "second");
    wait(cx, &app, "the second chat's answer", |a| {
        !said(a, "thread.answer").is_empty()
            && a.screen
                .thread
                .as_ref()
                .is_some_and(|t| t.thread.id != first)
    });
    let second = app.read_with(cx, |a, _| a.screen.thread.as_ref().unwrap().thread.id);
    // the layout fact keeps both chat tabs
    wait_for_the_tab_fact(cx, &op, &home, &repo, second);
    drop(app);
    drop(vcx);
    drop(runtime);

    let (app, mut vcx, runtime) = open_app(tcx, &op, 1400., Some((&home, &repo)));
    let cx = &mut vcx;
    app.read_with(cx, |a, _| {
        let chats: Vec<&Tab> = a
            .screen
            .tabs
            .iter()
            .filter(|t| matches!(t, Tab::Thread { .. }))
            .collect();
        assert_eq!(
            chats,
            [&Tab::Thread { id: first }, &Tab::Thread { id: second }],
            "{:?}",
            a.screen.tabs
        );
    });
    app.update(cx, |a, cx| a.open_tab(Tab::Thread { id: first }, cx));
    wait(cx, &app, "the first chat's conversation", |a| {
        said(a, "thread.answer").len() == 2
    });
    app.read_with(cx, |a, _| {
        assert_eq!(a.screen.dossier.as_deref(), Some("appreview"));
        assert_eq!(said(a, "thread.say"), ["WAIT hi", "again"]);
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.find("place").visible());
    });
    drop(runtime);
}

/// A third chat chooses where it works before its first message (the environment step, todo-180 B): the dossier's
/// repository is offered first; the first run already commits in the chat's own worktree.
fn a_chat_chooses_where_it_works_first(
    cx: &mut VisualTestContext,
    app: &Entity<LegionApp>,
    repo: &Path,
) -> (i64, String) {
    // a third chat chooses where it works before its first message (the environment step, todo-180 B): the dossier's
    // repository is offered first; the first run already commits in the chat's own worktree
    app.update(cx, |a, cx| a.open_tab(Tab::Chat, cx));
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("chat-place-change", cx);
    });
    wait(cx, app, "the environment step", |a| a.state.form.is_some());
    app.read_with(cx, |a, _| {
        let form = a.state.form.as_ref().unwrap();
        assert_eq!(form.values.get("repo").map(String::as_str), repo.to_str());
        let shown: Vec<&str> = form.fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(shown, ["repo", "base"], "no thread number to type");
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("form-field-base", cx);
        window.input("main", cx);
        window.click("form-submit", cx);
    });
    app.read_with(cx, |a, _| {
        assert_eq!(
            a.screen.chat_place,
            Some((repo.to_path_buf(), "main".to_string()))
        );
        assert!(
            a.state.pending.is_none(),
            "the user's choice is the step itself"
        );
    });
    send(cx, "commit");
    wait(cx, app, "the third chat's first answer", |a| {
        !said(a, "thread.answer").is_empty()
    });
    let (id, branch) = app.read_with(cx, |a, _| {
        let answer = &said(a, "thread.answer")[0];
        assert!(answer.contains("COMMIT_OK"), "{answer}");
        let t = &a.screen.thread.as_ref().unwrap().thread;
        (t.id, t.branch.clone().unwrap())
    });
    assert_eq!(branch, format!("legionctl/chat--thread-{id}"));
    (id, branch)
}

/// todo-174, the thread and code (approved 2026-10-17): a chat starts with no repository (its runs work in a scratch
/// folder: the stand-in cannot commit there). "Change where the chat works" opens the environment step; with the lab's
/// repository and base main, the thread gets its own worktree on its own branch, the title names repository@branch,
/// and the next run commits there, not on main (the agent gets the conversation so far: it has no session in the new
/// folder). A second chat's form has that repository first (the dossier's linked one), and it gets a second worktree;
/// a third chat chooses it before its first message.
#[gpui_kit::test]
fn a_chat_gets_a_repository_once_and_each_chat_works_in_its_own_worktree(cx: &mut TestAppContext) {
    let (op, home, repo) = thread_lab("code");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1400., Some((&home, &repo)));
    let cx = &mut vcx;
    wait(cx, &app, "the dossier", |a| a.screen.dossier.is_some());
    let mut threads = Vec::new();
    for first in ["one", "two"] {
        app.update(cx, |a, cx| a.open_tab(Tab::Chat, cx));
        send(cx, first);
        wait(cx, &app, "the scratch answer", |a| {
            !said(a, "thread.answer").is_empty()
        });
        app.read_with(cx, |a, _| {
            let answer = &said(a, "thread.answer")[0];
            assert!(
                answer.contains("COMMIT_FAILED"),
                "no repository yet: {answer}"
            );
        });
        attach_repository(cx, &app, &repo, first != "one");
        send(cx, "commit");
        wait(cx, &app, "the answer in the worktree", |a| {
            said(a, "thread.answer").len() == 2
        });
        let (id, branch) = app.read_with(cx, |a, _| {
            let answer = &said(a, "thread.answer")[1];
            assert!(answer.contains("COMMIT_OK"), "{answer}");
            assert!(
                answer.contains("prompt: The conversation of this thread so far")
                    && answer.contains(&format!("user: {first}")),
                "a new session in the worktree gets the conversation: {answer}"
            );
            let t = &a.screen.thread.as_ref().unwrap().thread;
            (t.id, t.branch.clone().unwrap())
        });
        assert_eq!(branch, format!("legionctl/chat--thread-{id}"));
        // the timeline says where the thread works, as a sentence
        app.read_with(cx, |a, _| {
            let row = a
                .screen
                .thread
                .as_ref()
                .unwrap()
                .log
                .iter()
                .find(|r| r.kind == "thread.repo")
                .cloned();
            assert_eq!(
                legionctl_api::chat::log_detail(&row.unwrap()),
                format!("works on {} at {branch} (from main)", repo.display())
            );
        });
        threads.push((id, branch));
    }
    let (id, branch) = a_chat_chooses_where_it_works_first(cx, &app, &repo);
    threads.push((id, branch));
    cx.update(|window, cx| {
        window.render_frame(cx);
        assert!(window.find("place").visible());
    });
    let git = |args: &[&str]| {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&repo)
            .args(args)
            .env("HOME", &home)
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    };
    assert_eq!(
        git(&["log", "--format=%s", "main"]),
        "start",
        "main has no agent commit"
    );
    for (_, branch) in &threads {
        assert_eq!(
            git(&["log", "--format=%s", branch]),
            "headless\nstart",
            "{branch}"
        );
    }
    let worktrees = git(&["worktree", "list"]);
    for (id, _) in &threads {
        assert!(
            worktrees.contains(&format!("chat--thread-{id}")),
            "{worktrees}"
        );
    }
    drop(runtime);
}

/// todo-174: each failure in the chat names its cause and the command that fixes it, and the conversation keeps the
/// failure: a dossier name in use, a dossier that is not there, an ACP adapter that is not installed (a
/// chat on `missing`: the thread's log has a thread.fail row with that cause), an adapter that ends before it answers
/// (its exit and stderr, as a missing login gives them), and a message into an agent session's thread (refused: a run
/// goes into a thread of the user).
#[gpui_kit::test]
fn each_failure_of_the_chat_names_its_cause(cx: &mut TestAppContext) {
    let (op, home, repo) = thread_lab("fail");
    // the config gets an agent whose adapter is not there
    let config = home.join(".config/legionctl/app.jsonc");
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    value["session"]["acp"]["missing"] = serde_json::json!(["no-such-adapter"]);
    value["session"]["acp"]["broken"] =
        serde_json::json!(["sh", "-c", "echo 'cannot start: no login' >&2; exit 1"]);
    write(&config, &value.to_string());
    let (app, mut vcx, runtime) = open_app(cx, &op, 1400., Some((&home, &repo)));
    let cx = &mut vcx;
    wait(cx, &app, "the agents and the dossier", |a| {
        a.screen.agents.iter().any(|n| n == "missing") && a.screen.dossier.is_some()
    });
    let message = |cx: &mut VisualTestContext, app: &Entity<LegionApp>, what: &str, want: &str| {
        wait(cx, app, what, |a| {
            a.state.message.as_deref().is_some_and(|m| m.contains(want))
        });
    };
    // a change from the chat bar waits in the same confirm as its button, and runs only on Apply (U-13)
    let confirm = |cx: &mut VisualTestContext, app: &Entity<LegionApp>, line: &str, name: &str| {
        send(cx, line);
        wait(cx, app, "the confirm", |a| {
            a.state
                .pending
                .as_ref()
                .is_some_and(|p| p.button.op == name)
        });
        let events: serde_json::Value =
            serde_json::from_str(&op.ok(&["events", "--json"])).unwrap();
        let ran = events["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|e| e["actor"] == "app" && e["kind"] == name);
        assert!(!ran, "{name} ran before Apply");
        cx.update(|window, cx| {
            window.render_frame(cx);
            window.click("apply", cx);
        });
    };
    confirm(cx, &app, "/dossier create chat", "dossier.create");
    message(
        cx,
        &app,
        "the name in use",
        "dossier chat exists already: choose it with dossier use chat",
    );
    // a session change (the user's own dossier choice) runs at once: no confirm
    send(cx, "/dossier use nobody");
    message(
        cx,
        &app,
        "no such dossier",
        // the user's own use of an unknown name lists the dossiers (user decision 2026-10-17)
        "no dossier nobody; the dossiers are: port, chat. Make a new one with dossier create nobody",
    );
    send(cx, "/agent missing");
    send(cx, "/chat");
    send(cx, "x");
    message(
        cx,
        &app,
        "the missing adapter",
        "Install the agent's adapter no-such-adapter, or name its npm package in config session.acp_packages",
    );
    wait(cx, &app, "the failure in the conversation", |a| {
        !said(a, "thread.fail").is_empty()
    });
    app.read_with(cx, |a, _| {
        assert!(said(a, "thread.fail")[0].contains("Install the agent's adapter no-such-adapter"));
        assert_eq!(said(a, "thread.say"), ["x"]);
    });
    // an adapter that ends before it answers: its exit and its stderr, not the JSON-RPC envelope
    send(cx, "/agent broken");
    send(cx, "/chat");
    send(cx, "y");
    message(
        cx,
        &app,
        "the adapter that ended",
        "the agent's ACP adapter ended before it answered: Process exited with exit status: 1: cannot start: no login",
    );
    app.update(cx, |a, cx| a.open_tab(Tab::Thread { id: 1 }, cx));
    wait(cx, &app, "thread 1", |a| {
        a.screen.thread.as_ref().is_some_and(|t| t.thread.id == 1)
    });
    send(cx, "hi");
    message(
        cx,
        &app,
        "the agent's thread",
        "thread 1 is the thread of session main",
    );
    drop(runtime);
}

/// A chat whose adapter is not installed (the move from ~/.cache/.bun to ~/.bun, a stand-in bun): the install fails
/// (the registry does not answer), the chat says why in one line and shows Retry; Retry sends the message again, the
/// chat shows "Installing the Fresh adapter…" while bun installs it, and the agent answers.
#[gpui_kit::test]
fn a_chat_installs_its_missing_adapter_and_retry_tries_again(cx: &mut TestAppContext) {
    let (op, home, repo) = thread_lab("install");
    adapter::stand_in_bun(&home);
    let config = home.join(".config/legionctl/app.jsonc");
    let mut value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&config).unwrap()).unwrap();
    value["session"]["acp"]["fresh"] = serde_json::json!(adapter::COMMAND);
    value["session"]["acp_packages"] = serde_json::json!({"fake-acp": "@test/fake-acp"});
    write(&config, &value.to_string());
    write(&home.join("install-fails"), "");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1400., Some((&home, &repo)));
    let cx = &mut vcx;
    wait(cx, &app, "the agents and the dossier", |a| {
        a.screen.agents.iter().any(|n| n == "fresh") && a.screen.dossier.is_some()
    });
    send(cx, "/agent fresh");
    send(cx, "/chat");
    send(cx, "hi");
    wait(cx, &app, "the failed install, and the run ended", |a| {
        a.screen.live.is_empty() && said(a, "thread.fail").iter().any(|f| {
            f == "Installing the adapter fake-acp failed: error: GET https://registry.npmjs.org/@test/fake-acp - 503"
        })
    });
    std::fs::remove_file(home.join("install-fails")).unwrap();
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("retry", cx);
    });
    wait(cx, &app, "the install note", |a| {
        a.screen
            .live
            .values()
            .any(|(_, text)| text.starts_with("Installing the Fresh adapter…"))
    });
    wait(cx, &app, "the answer", |a| {
        said(a, "thread.answer")
            .iter()
            .any(|t| t.contains("The user's new message:\nhi"))
    });
    assert_eq!(adapter::installs(&home), ["add -g @test/fake-acp"; 2]);
    app.read_with(cx, |a, _| assert_eq!(said(a, "thread.say"), ["hi", "hi"]));
    drop(runtime);
}

/// A chat goes on after the computer started again (user finding r340): the agent's new process does not know the
/// chat's ACP session; the next message gets an answer (a new ACP session with the chat's history), and the chat shows
/// no error.
#[gpui_kit::test]
fn a_chat_goes_on_after_the_agent_lost_its_session(cx: &mut TestAppContext) {
    // in the lab's HOME (thread_lab_on's folder)
    let rebooted = format!(
        "/var/tmp/legionctl-app-lost-{}/rebooted",
        std::process::id()
    );
    let (op, home, repo) = thread_lab_on("lost", &[("FAKE_ACP_FORGET", rebooted.clone().into())]);
    assert_eq!(home.join("rebooted"), Path::new(&rebooted));
    let (app, mut vcx, runtime) = open_app(cx, &op, 1400., Some((&home, &repo)));
    let cx = &mut vcx;
    wait(cx, &app, "the dossier", |a| a.screen.dossier.is_some());
    send(cx, "/chat");
    send(cx, "first");
    wait(cx, &app, "the first answer", |a| {
        said(a, "thread.answer").len() == 1
    });
    write(&home.join("rebooted"), "");
    send(cx, "second");
    wait(cx, &app, "the answer after the restart", |a| {
        said(a, "thread.answer").len() == 2
    });
    app.read_with(cx, |a, _| {
        let answer = &said(a, "thread.answer")[1];
        assert!(
            answer.contains("acp: new") && answer.contains("user: first"),
            "{answer}"
        );
        assert!(said(a, "thread.fail").is_empty() && a.state.message.is_none());
    });
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
    // a placement shows in the app on a thread that it shows: the user opens thread 1
    app.update(cx, |a, cx| a.open_tab(Tab::Thread { id: 1 }, cx));
    wait(cx, &app, "the thread", |a| a.screen.thread.is_some());
    lab_cli(
        &op,
        &home,
        &repo,
        &["views", "place", "1", "members", "thread=1"],
        "",
    );
    wait(cx, &app, "the placed view", |a| {
        a.screen.view().is_some_and(|v| v.rows.len() == 2)
    });
    app.read_with(cx, |a, _| {
        let v = a.screen.view().unwrap();
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
        a.screen.view().is_some_and(|v| v.rows.len() == 3)
    });
    app.read_with(cx, |a, _| {
        let v = a.screen.view().unwrap();
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
        a.screen.view().is_some_and(|v| v.columns == ["name"])
    });
    app.read_with(cx, |a, _| {
        let v = a.screen.view().unwrap();
        assert_eq!(v.rows, [["main0"], ["observer0"], ["observer1"]]);
        assert!(a.screen.events >= 2, "{}", a.screen.events);
    });
    drop(runtime);
}

/// The thread lab with the clauth profiles work (5h 42%), spare (5h 10%), alt (active, the current login) and side,
/// each with a Claude login, the stand-in clauth with their use, and two more team threads of vanilla: 2 with teammates (session main is Claude
/// Code's), 3 with operator members (a session of the agent fake) whose observer0 runs on work.
fn accounts_lab() -> (Operator, PathBuf, PathBuf) {
    let (op, home, repo) = thread_lab("acct");
    // thread 2: teammates (session main is Claude Code's), before observer0's config has an account
    lab_cli(
        &op,
        &home,
        &repo,
        &["thread", "start", "teammates", "--team", "vanilla"],
        "",
    );
    clauth::install(&home);
    write(&home.join("clauth-which"), "alt\n");
    write(
        &home.join(".config/legionctl/accounts.jsonc"),
        &serde_json::json!({
            "session": {"agents": {"fake": ["true"]}},
            "agents": {"configs": {"thread-observer": {"account": "work"}}}})
        .to_string(),
    );
    let mut feed = Vec::new();
    // clauth's layout (0.17.0): alt is active and Claude Code's current login (a link to alt's file)
    for (name, token, five) in [
        ("work", "tok-w", 42.0),
        ("spare", "tok-s", 10.0),
        ("alt", "tok-a", 21.0),
        ("side", "tok-c", 5.0),
    ] {
        write(
            &home.join(format!(".clauth/profiles/{name}/credentials.json")),
            &format!(
                r#"{{"claudeAiOauth": {{"accessToken": "{token}", "refreshToken": "r-{name}"}}}}"#
            ),
        );
        feed.push(clauth::profile(
            name,
            "claude",
            name == "alt",
            &[("5h", five), ("7d", 8.0)],
            None,
        ));
    }
    clauth::status(&home, &feed);
    write(
        &home.join(".clauth/profiles.toml"),
        "active_profile = \"alt\"\nprofiles = [\"work\", \"spare\", \"alt\", \"side\"]\n",
    );
    let live = home.join(".claude/.credentials.json");
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    let _ = std::fs::remove_file(&live);
    std::os::unix::fs::symlink(home.join(".clauth/profiles/alt/credentials.json"), &live).unwrap();
    // thread 3: a team of operator members (the main agent is neither Claude Code nor Codex)
    let launch: serde_json::Value = serde_json::from_str(&lab_cli(
        &op,
        &home,
        &repo,
        &["session", "start", "fake", "--dossier", "team", "--json"],
        "",
    ))
    .unwrap();
    let fake = launch["id"].as_str().unwrap();
    let as_fake = op.run(
        &repo,
        &[
            ("HOME", home.to_str().unwrap()),
            ("LEGIONCTL_SESSION", fake),
        ],
        &["thread", "start", "operators", "--team", "vanilla"],
        "",
    );
    assert!(
        as_fake.status.success(),
        "{}",
        String::from_utf8_lossy(&as_fake.stderr)
    );

    (op, home, repo)
}

/// The member chips show each member's account with its use from the provider, and the account picker sets it with
/// the operation's approval (S3 of planning/features/accounts.jsonc): in a team thread of operator members, a click on
/// observer0's account opens the profiles, a pick asks, Apply runs `thread.account`, and observer0 runs on it from its
/// next wake; the chat bar's account picks the same way. A teammate's account cannot be picked: its chip says why,
/// and the operation refuses it (V1).
#[gpui_kit::test]
fn the_member_chip_shows_its_account_usage_and_the_picker_sets_it(cx: &mut TestAppContext) {
    let (op, home, repo) = accounts_lab();
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    let cx = &mut vcx;
    app.update(cx, |a, cx| a.open_tab(Tab::Thread { id: 3 }, cx));
    wait(cx, &app, "thread 3 and the usage", |a| {
        a.screen.thread.as_ref().is_some_and(|t| t.thread.id == 3) && !a.screen.usage.is_empty()
    });
    app.read_with(cx, |a, _| {
        let observer = a.screen.shown[&3]
            .members
            .iter()
            .find(|m| m.name == "observer0")
            .unwrap();
        assert_eq!(observer.account.as_deref(), Some("work"));
        assert_eq!(a.screen.usage_of(Some("work")), "claude 5h 42% 7d 8%");
        // the chip's tooltip: clauth's state of the account in one plain sentence
        let state = a.screen.account_state(Some("work"));
        plain::assert_plain("the accounts", &state);
        assert_eq!(
            state,
            "Claude Code: 5h 42% used, 7d 8% used. Read just now."
        );
        assert_eq!(
            a.screen.usage_of(None),
            "claude 5h 21% 7d 8%",
            "the current login is alt"
        );
    });
    reveal(cx, "account-observer0");
    cx.update(|window, cx| window.click("account-observer0", cx));
    reveal(cx, "pick-spare");
    cx.update(|window, cx| window.click("pick-spare", cx));
    wait(cx, &app, "the pick's approval", |a| {
        a.state.pending.is_some()
    });
    app.read_with(cx, |a, _| {
        let pending = a.state.pending.as_ref().unwrap();
        assert_eq!(pending.class, legionctl_api::Class::Workspace);
        assert_eq!(
            pending.button.input,
            serde_json::json!({"thread": 3, "member": "observer0", "account": "spare"})
        );
    });
    cx.update(|window, cx| window.click("apply", cx));
    wait(cx, &app, "observer0 on spare", |a| {
        a.screen.shown.get(&3).is_some_and(|t| {
            t.members
                .iter()
                .any(|m| m.name == "observer0" && m.account.as_deref() == Some("spare"))
        })
    });

    // the chat bar's account
    reveal(cx, "account-");
    cx.update(|window, cx| window.click("account-", cx));
    app.read_with(cx, |a, _| assert_eq!(a.screen.picking.as_deref(), Some("")));
    reveal(cx, "pick-work");
    cx.update(|window, cx| window.click("pick-work", cx));
    wait(cx, &app, "the chat pick's approval", |a| {
        a.state.pending.is_some()
    });
    cx.update(|window, cx| window.click("apply", cx));
    wait(cx, &app, "the chat bar on work", |a| {
        a.screen.chat_account().as_deref() == Some("work")
    });

    // the current login (clauth's alt) is an account; clauth's side is not the current login and picks the same way
    reveal(cx, "account-");
    cx.update(|window, cx| window.click("account-", cx));
    reveal(cx, "pick-side");
    cx.update(|window, cx| window.click("pick-side", cx));
    wait(cx, &app, "side's approval", |a| a.state.pending.is_some());
    cx.update(|window, cx| window.click("apply", cx));
    wait(cx, &app, "the chat bar on side", |a| {
        a.screen.chat_account().as_deref() == Some("side")
    });
    reveal(cx, "account-");
    cx.update(|window, cx| window.click("account-", cx));
    reveal(cx, "pick-current");
    cx.update(|window, cx| window.click("pick-current", cx));
    wait(cx, &app, "the current login's approval", |a| {
        a.state.pending.is_some()
    });
    app.read_with(cx, |a, _| {
        assert_eq!(
            a.state.pending.as_ref().unwrap().button.input,
            serde_json::json!({"thread": 3, "member": null, "account": null})
        );
    });
    cx.update(|window, cx| window.click("apply", cx));
    wait(cx, &app, "the chat bar on the current login", |a| {
        a.screen.chat_account().is_none()
    });

    a_teammates_account_cannot_be_picked(cx, &app, (&op, &home, &repo));
    drop(runtime);
}

/// V1: a teammate runs on its lead's account: its chip opens no picker, and the operation refuses it.
fn a_teammates_account_cannot_be_picked(
    cx: &mut VisualTestContext,
    app: &Entity<LegionApp>,
    (op, home, repo): (&Operator, &Path, &Path),
) {
    app.update(cx, |a, cx| a.open_tab(Tab::Thread { id: 2 }, cx));
    wait(cx, app, "thread 2", |a| {
        a.screen.thread.as_ref().is_some_and(|t| t.thread.id == 2)
    });
    reveal(cx, "account-observer0");
    cx.update(|window, cx| window.click("account-observer0", cx));
    cx.run_until_parked();
    app.read_with(cx, |a, _| {
        assert_eq!(a.screen.picking, None, "no picker for a teammate");
    });
    let refused = op.run(
        repo,
        &[
            ("HOME", home.to_str().unwrap()),
            ("LEGIONCTL_SESSION", "main"),
        ],
        &[
            "thread",
            "account",
            "2",
            "--account",
            "spare",
            "--member",
            "observer0",
        ],
        "",
    );
    assert!(
        !refused.status.success()
            && String::from_utf8_lossy(&refused.stderr)
                .contains("observer0 is a teammate: it runs on the account of its lead"),
        "{}",
        String::from_utf8_lossy(&refused.stderr)
    );
}
