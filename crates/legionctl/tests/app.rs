//! The desktop app against the real operator on the recorded machine (unplugged, AC policy still applied, no
//! daemon): it shows the machine, previews the policy change, reports a refused apply, and shows doctor's findings.
//! Real clicks through GPUI's headless window; the operator answers in real time.

mod common;

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
/// name); Undo goes back to version 1. A composed view that fails its check is shown as an error with its reason, and
/// nothing is saved.
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

/// The app in a headless window of `width` x 700 against `op` with the seed Machine now as its tab, and its entity; `runs`
/// is the repository of the chat bar's runs (dossier `chat`).
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
    let dossier = runs.map(|_| "chat".to_string());
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
                LegionApp::new(client, handle, Some(view("machine-now")), repo, cx)
                    .with_dossier(dossier)
            })
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
        // the Work space, then its first thread as a tab
        cx.update(|window, cx| window.click("space-work", cx));
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
        assert_eq!(a.screen.space, Space::Work);
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
/// the app through the event stream and reads as a sentence, in the app and in the TUI's line; no row is raw JSON.
#[gpui_kit::test]
fn the_app_thread_pane_shows_a_watch_rule_as_a_sentence(cx: &mut TestAppContext) {
    let (op, home, repo) = thread_lab("rule");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1000., None);
    let cx = &mut vcx;
    wait(cx, &app, "the thread", |a| a.screen.thread.is_some());
    // the thread is a tab (DESIGN-09): the Work space lists it
    cx.update(|window, cx| window.click("space-work", cx));
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
        assert!(
            legionctl_api::chat::log_line(rule)
                .ends_with("observer0 rule reminder: after 4 Edit/Write/MultiEdit/NotebookEdit in crates/, expect make lint"),
            "{}",
            legionctl_api::chat::log_line(rule)
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

/// todo-144: the chat bar picks its agent. It starts with config app.agent (`fake`); `/agent ` offers each name of
/// session.acp (the defaults' claude and codex, the lab's fake and other); `/agent nobody` is refused with the names;
/// `/agent other` is a row of the thread's log, so the pick is the thread's (every screen reads it); the next prompt
/// runs on `other`, whose answer names it.
#[gpui_kit::test]
fn the_app_chat_bar_picks_its_agent_from_session_acp_and_keeps_the_pick_in_the_thread(
    cx: &mut TestAppContext,
) {
    let (op, home, repo) = thread_lab("pick");
    let (app, mut vcx, runtime) = open_app(cx, &op, 1200., Some((&home, &repo)));
    let cx = &mut vcx;
    wait(cx, &app, "the thread and the agents", |a| {
        a.screen.thread.is_some() && !a.screen.agents.is_empty()
    });
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
    wait(cx, &app, "the refused pick", |a| {
        a.screen.runs.first().is_some_and(|r| r.1.is_some())
    });
    app.read_with(cx, |a, _| {
        assert_eq!(a.screen.runs[0], ("/agent nobody".to_string(), Some(false)));
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
    });
    wait(cx, &app, "the pick in the thread's log", |a| {
        a.screen.agent().as_deref() == Some("other")
    });
    let log: serde_json::Value = serde_json::from_str(&lab_cli(
        &op,
        &home,
        &repo,
        &["thread", "show", "1", "--json"],
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
    cx.update(|window, cx| {
        window.input("hello", cx);
        window.press("enter", cx);
    });
    wait(cx, &app, "the prompt's run", |a| {
        a.screen.runs.len() == 3 && a.screen.runs[2].1.is_some()
    });
    app.read_with(cx, |a, _| {
        assert_eq!(a.screen.runs[2], ("hello".to_string(), Some(true)));
        let answer = a.state.message.clone().unwrap_or_default();
        // text is an intent (G5): the picked agent composes, with its model
        assert!(
            answer.contains("Intent: hello")
                && answer.contains("agent: other")
                && answer.contains("model: other-small"),
            "{answer}"
        );
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
