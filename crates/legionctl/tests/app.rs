//! The desktop app against the real operator on the recorded machine (unplugged, AC policy still applied, no
//! daemon): it shows the machine, previews the policy change, reports a refused apply, and shows doctor's findings.
//! Real clicks through GPUI's headless window; the operator answers in real time.

mod common;
#[path = "common/keyring.rs"]
mod keyring;
#[path = "common/usage.rs"]
mod usage;

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

/// Scroll the tab's body until the element `id` shows.
fn reveal(cx: &mut VisualTestContext, id: &str) {
    for _ in 0..60 {
        let shown = cx.update(|window, cx| {
            window.render_frame(cx);
            window
                .try_find(SharedString::from(id.to_string()))
                .is_some_and(|e| e.visible())
        });
        if shown {
            return;
        }
        cx.update(|window, cx| {
            window.scroll(
                "body",
                gpui_kit::ScrollDelta::Pixels(point(px(0.), px(-200.))),
                cx,
            );
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
    let before = op.ok(&["events"]);
    assert!(
        !before
            .lines()
            .any(|l| l.contains(" app ") && l.contains("power.apply") && !l.contains("dry")),
        "{before}"
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
    let events = op.ok(&["events"]);
    assert!(
        events
            .lines()
            .any(|l| l.contains(" app ") && l.contains("power.apply")),
        "{events}"
    );

    health_and_bench(cx, &app);
    drop(runtime);
}

/// Health groups doctor's findings; Bench reads every page of the runs, the newest first, one chart per kind.
fn health_and_bench(cx: &mut VisualTestContext, app: &Entity<LegionApp>) {
    // Health: doctor's findings in groups with their counts
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
    });

    // Bench: every page of the runs, the newest first, one chart per kind
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
/// ones on demand, a choice as buttons). Continue without a required field says which; with them, the session class
/// asks inline (no dry run), and Apply stores the fact with the user as its source.
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
    click(cx, "form-submit".into());
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
    click(cx, "form-field-entity".into());
    cx.update(|window, cx| window.input("color", cx));
    click(cx, "form-field-value".into());
    cx.update(|window, cx| window.input("\"teal\"", cx));
    click(cx, "form-more".into());
    click(cx, "form-scope-project".into());
    click(cx, "form-submit".into());
    app.read_with(cx, |a, _| {
        assert!(a.state.form.is_none());
        let pending = a.state.pending.as_ref().unwrap();
        assert_eq!(pending.class, legionctl_api::Class::Session);
        assert!(pending.plan.is_none(), "no dry run: an inline confirm");
        assert_eq!(pending.button.input["entity"], "color");
        assert_eq!(pending.button.input["scope"], "project");
    });
    click(cx, "apply".into());
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

/// The code graph seed (DESIGN-09 G4, todo-141) on a repository with the ctx stand-in: on branch work, the commit
/// after main changes `two`. A ctx that cannot index gives no graph: the tree part shows why, not an empty tab. With
/// the stand-in, the tree's top has the folder src with its symbol count, marked as changed since main (tools is
/// not); a click on src reads its level (src/lib.rs, marked), a click on the file its symbols (two is marked, add is
/// not); a click on add selects it, and the stack's other part reads what uses it: two.
#[gpui_kit::test]
fn the_code_graph_seed_opens_levels_on_click_marks_changes_and_shows_what_uses_a_selected_symbol(
    cx: &mut TestAppContext,
) {
    let (op, home, repo) = thread_lab("graph");
    graph_repo(&home, &repo);
    broken_ctx(&home);
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
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
            "Click a symbol in the tree to see what uses it"
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

/// The chat bar's text is an intent (DESIGN-09 G5). The intent of a seed opens it from the store: no model runs (no
/// session.run). A new intent goes to the ui-composer (the chat bar's agent with its model from `views.composer_models`):
/// its view opens as a tab under a name no layer has, as version 1, and the intent is a fact of the user; the same
/// intent again opens the stored view with no model.
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
            ("health-2", 1),
            "a name no layer has, version 1"
        );
        let said = a.state.message.clone().unwrap_or_default();
        assert!(
            said.contains("model: fake-small") && said.contains("Here is the view"),
            "{said}"
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
        (Some("health-2"), Some("user"))
    );

    cx.update(|window, cx| window.click("nav-machine-now", cx));
    say(cx, "Fan speeds of the machine?");
    wait(cx, &app, "the stored answer", |a| {
        a.tab()
            .is_some_and(|t| t.key().starts_with("view:health-2:"))
    });
    assert_eq!(
        runs(),
        1,
        "the same intent again: the stored view, no model"
    );
    drop(runtime);
}

/// `/change <words>` (DESIGN-09 G5): the composer's answer is the next version of the tab's view (version 2, the same
/// name); Undo goes back to version 1. A composed view that fails its check, or has a part with no title, is shown as an
/// error with its reason, and nothing is saved. A composed view's space follows its source, not the model's word.
#[gpui_kit::test]
fn a_change_is_the_next_version_of_the_tabs_view_and_a_bad_view_shows_its_error(
    cx: &mut TestAppContext,
) {
    let (op, home, repo) = thread_lab("change");
    let views = home.join("fake-views");
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
    say(cx, "/change add the unit");
    wait(cx, &app, "version 2", |a| {
        a.screen
            .view()
            .is_some_and(|v| v.title == "Sensors, with more")
    });
    app.read_with(cx, |a, _| {
        let v = a.screen.view().unwrap();
        assert_eq!(
            (v.name.as_str(), v.version, v.versions.clone()),
            (name.as_str(), 2, vec![1, 2])
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

    say(cx, "/change bad view");
    wait(cx, &app, "the check's error", |a| {
        a.state
            .message
            .as_deref()
            .is_some_and(|m| m.contains("the view failed its check"))
    });
    app.read_with(cx, |a, _| {
        let said = a.state.message.clone().unwrap();
        assert!(said.contains("piechart"), "{said}");
        assert_eq!(a.screen.view().unwrap().versions, [1, 2], "nothing saved");
    });
    say(cx, "/change untitled part");
    wait(cx, &app, "the title's error", |a| {
        a.state.message.as_deref().is_some_and(|m| {
            m.contains("each view and each part needs a title in the user's words: temp-stat")
        })
    });
    app.read_with(cx, |a, _| {
        assert_eq!(a.screen.view().unwrap().versions, [1, 2], "nothing saved");
    });

    say(cx, "what changed on this branch");
    wait(cx, &app, "the composed code view in the list", |a| {
        a.screen.views.iter().any(|v| v.name == "branch-changes")
    });
    app.read_with(cx, |a, _| {
        let listed = a.screen.views.iter().find(|v| v.name == "branch-changes");
        assert_eq!(listed.and_then(|v| v.space.as_deref()), Some("work"));
    });
    drop(runtime);
}

/// The Resources seed (note 22) against the real operator on the recorded machine, without a daemon: it lists the
/// configset layers in their group with their actions, which come with the data (each action names its operation);
/// a click shows the route first (rules.disable is a machine operation with no dry run, so the tab asks inline and
/// nothing runs), and Apply runs it as a call of its own: the operator refuses it without the system bus, the tab
/// says why, and the event log names `rules.disable` with the app as its actor.
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
        window.click(SharedString::from(id), cx);
    });
    app.read_with(cx, |a, _| {
        let pending = a.state.pending.as_ref().unwrap();
        assert_eq!(pending.button.op, "rules.disable");
        assert!(
            pending.plan.is_none(),
            "no dry run: an inline confirm of the operation and its input"
        );
    });
    let before = op.ok(&["events"]);
    assert!(!before.contains("rules.disable"), "{before}");

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
    let events = op.ok(&["events"]);
    assert!(
        events
            .lines()
            .any(|l| l.contains(" app ") && l.contains("rules.disable")),
        "{events}"
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
                // the composer's runs are bound to the view tools; the stand-in takes the binding as claude-code-acp
                "acp_bind": ["fake", "other"],
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
    let mut op = Operator::start(
        &format!("ap-{tag}"),
        &[
            ("LEGIONCTL_SYSROOT", home.join("root").into()),
            ("HOME", home.as_os_str().to_owned()),
            ("PATH", path),
            // the stand-in composer answers an intent with the view of <first word>.jsonc there
            ("FAKE_ACP_VIEWS", home.join("fake-views").into()),
            // a WAIT prompt holds until the test makes this file (a chat's folder is an overlay: its lower
            // folder's new files need not show)
            ("FAKE_ACP_RELEASE", home.join("release").into()),
        ],
    );
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
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = Client::at("app", &op.runtime);
    let handle = runtime.handle().clone();
    let repo = runs.map_or_else(here, |(_, repo)| repo.to_path_buf());
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
            cx.new(|cx| LegionApp::new(client, handle, Some(view("machine-now")), repo, cx))
        })
        .unwrap();
        window.downcast().unwrap()
    });
    let app: Entity<LegionApp> = window
        .update(cx, |root, _, _| root.view().clone().downcast().unwrap())
        .unwrap();
    let mut vcx = VisualTestContext::from_window(window.into(), cx);
    // the layout facts first: they set the tabs
    wait(&mut vcx, &app, "the layout", |a| a.screen.restored);
    (app, vcx, runtime)
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
/// centred, 60% of the tab's width; three lines make it taller, many lines stop at 40% of the tab; Ctrl+Right makes
/// it 70%. A drag of the navigator's edge sets its width; a click on the space that shows folds the navigator.
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
    cx.update(|window, cx| window.press("ctrl-right", cx));
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
                ("/agent claude", "Send the prompts to claude (session.acp)"),
                ("/agent codex", "Send the prompts to codex (session.acp)"),
                ("/agent fake", "Send the prompts to fake (session.acp, now)"),
                ("/agent other", "Send the prompts to other (session.acp)"),
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
            why.contains("no agent nobody in session.acp; it has claude, codex, fake, other"),
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

/// No dossier: "Hi" in a new chat says what a dossier is for and opens the Dossiers view; `/dossier add` names the
/// commands that work; New dossier (the form of dossier.create, an inline confirm) makes appreview, and the app uses it.
fn make_a_dossier_in_the_app(
    cx: &mut VisualTestContext,
    app: &Entity<LegionApp>,
    op: &Operator,
    home: &Path,
    repo: &Path,
) {
    cx.update(|window, cx| {
        window.render_frame(cx);
        window.click("space-work", cx);
        window.render_frame(cx);
        window.click("nav-new-chat", cx);
    });
    send(cx, "Hi");
    app.read_with(cx, |a, _| {
        let why = a.state.message.clone().unwrap_or_default();
        assert!(
            why.starts_with("No dossier: a chat and an agent run keep their record in a dossier")
                && why.contains("/dossier use <name>")
                && why.contains("/dossier create <name>"),
            "{why}"
        );
        assert_eq!(a.tab(), Some(&view("dossiers")), "the choice opens");
    });
    send(cx, "/dossier add MEMORY notes");
    app.read_with(cx, |a, _| {
        let why = a.state.message.clone().unwrap_or_default();
        assert!(
            why.contains("an agent does it in its session")
                && why.contains("choose it: /dossier use <name>"),
            "{why}"
        );
    });
    wait(cx, app, "the dossiers", |a| {
        shown(a, "dossiers").is_some_and(|v| v.rows.len() == 2)
    });
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
    });
    cx.update(|window, cx| {
        window.render_frame(cx);
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
            .any(|l| l.starts_with("appreview") && l.ends_with("(the user's dossier)")),
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

/// "Work on a repository" in the thread tab: the form of thread.repo (the repository given or `prefilled` with the
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
        window.click("form-more", cx);
        window.render_frame(cx);
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

/// todo-174, the thread and code (approved 2026-10-17): a chat starts with no repository (its runs work in a scratch
/// folder: the stand-in cannot commit there). "Work on a repository" opens the form of thread.repo; with the lab's
/// repository and base main, the thread gets its own worktree on its own branch, the title names repository@branch,
/// and the next run commits there, not on main (the agent gets the conversation so far: it has no session in the new
/// folder). A second chat's form has that repository first (the dossier's linked one), and it gets a second worktree.
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
/// failure: a dossier name in use, a dossier that is not there, an ACP adapter that is not on the operator's PATH (a
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
    send(cx, "/dossier create chat");
    message(
        cx,
        &app,
        "the name in use",
        "dossier chat exists already: choose it with dossier use chat",
    );
    send(cx, "/dossier use nobody");
    message(
        cx,
        &app,
        "no such dossier",
        "no dossier nobody (dossier list); make it with dossier create nobody",
    );
    send(cx, "/agent missing");
    send(cx, "/chat");
    send(cx, "x");
    message(
        cx,
        &app,
        "the missing adapter",
        "the ACP adapter no-such-adapter is not on the operator's PATH",
    );
    wait(cx, &app, "the failure in the conversation", |a| {
        !said(a, "thread.fail").is_empty()
    });
    app.read_with(cx, |a, _| {
        assert!(
            said(a, "thread.fail")[0].contains("no-such-adapter is not on the operator's PATH")
        );
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

/// The thread lab with account profiles work (5h 42%) and spare (5h 10%), each with a Claude login, the stand-in
/// usage endpoint and keyring, and two more team threads of vanilla: 2 with teammates (session main is Claude
/// Code's), 3 with operator members (a session of the agent fake) whose observer0 runs on work.
fn accounts_lab() -> (Operator, PathBuf, PathBuf, common::process::Kill) {
    let (op, home, repo) = thread_lab("acct");
    // a stand-in keyring on the operator's PATH (the home's own tools come first): never the user's
    keyring::keyring_path(&home, &std::ffi::OsString::new());
    std::fs::copy(
        home.join("keyring-bin/secret-tool"),
        home.join(".local/bin/secret-tool"),
    )
    .unwrap();
    // thread 2: teammates (session main is Claude Code's), before observer0's config has an account
    lab_cli(
        &op,
        &home,
        &repo,
        &["thread", "start", "teammates", "--team", "vanilla"],
        "",
    );
    let answers = home.join("usage");
    let (server, port) = usage::usage_server(&answers);
    write(
        &home.join(".config/legionctl/accounts.jsonc"),
        &serde_json::json!({
            "session": {"agents": {"fake": ["true"]}},
            "agents": {"configs": {"thread-observer": {"account": "work"}},
                       "accounts": {"usage_ttl_s": 3600, "endpoints": {
                           "claude": format!("http://127.0.0.1:{port}/claude"), "codex": "http://127.0.0.1:1/none"}}}})
        .to_string(),
    );
    for (name, token, five) in [("work", "tok-w", 42.0), ("spare", "tok-s", 10.0)] {
        lab_cli(&op, &home, &repo, &["agents", "accounts", "add", name], "");
        write(
            &home.join(format!(
                ".local/share/legionctl/accounts/{name}/claude/.credentials.json"
            )),
            &format!(
                r#"{{"claudeAiOauth": {{"accessToken": "{token}", "expiresAt": 4102444800000}}}}"#
            ),
        );
        write(
            &answers.join(format!("{token}.json")),
            &serde_json::json!({"body": {"limits": [{"kind": "session", "percent": five}, {"kind": "weekly_all", "percent": 8.0}]}})
                .to_string(),
        );
    }
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

    (op, home, repo, server)
}

/// The member chips show each member's account with its use from the provider, and the account picker sets it with
/// the operation's approval (S3 of planning/features/accounts.jsonc): in a team thread of operator members, a click on
/// observer0's account opens the profiles, a pick asks, Apply runs `thread.account`, and observer0 runs on it from its
/// next wake; the chat bar's account picks the same way. A teammate's account cannot be picked: its chip says why,
/// and the operation refuses it (V1).
#[gpui_kit::test]
fn the_member_chip_shows_its_account_usage_and_the_picker_sets_it(cx: &mut TestAppContext) {
    let (op, home, repo, _server) = accounts_lab();
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
        assert_eq!(a.screen.usage_of("work"), "claude 5h 42% 7d 8%");
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
        &["thread", "account", "2", "spare", "--member", "observer0"],
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
