//! The planned-code graph (design 23, A4-A5, P3): `planning/features/<id>.jsonc` (features, objectives, stories, work
//! items with planned unit keys) and `planning/units.jsonc` (run, build and deploy units), versioned with the code.
//! `check` validates the plan of a tree against its code graph; `status` joins the plan with the code graph (units
//! done or missing), tokei (LOC of files) and, since a ref, the change graph.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::codegraph::{self, Node};
use crate::codetests::TestRow;
use crate::dossier::Dossiers;
use crate::error::{Code, coded};

const FEATURES: &str = "planning/features/";
const UNITS: &str = "planning/units.jsonc";
/// The unit key namespaces (design 23, A4). `rust:` and `symbol:` are stable keys of the code graph (`symbol:` for
/// methods, which ctx gives no `rust:` key).
const PREFIXES: [&str; 7] = [
    "file:", "rust:", "symbol:", "config:", "op:", "test:", "unit:",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum Status {
    Planned,
    InProgress,
    Done,
    Abandoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum StoryKind {
    #[default]
    Story,
    Lifecycle,
    DataFlow,
    ControlFlow,
    Requirement,
    Issue,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Change {
    Add,
    Edit,
    Remove,
}

/// One feature: `planning/features/<id>.jsonc`.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Feature {
    #[serde(rename = "$schema", default)]
    pub schema: Option<String>,
    /// The file name without `.jsonc`.
    pub id: String,
    pub title: String,
    pub status: Status,
    /// Dossier plan rows as `<dossier>#<row>`, such as `legionctl#todo-90`.
    #[serde(default)]
    pub plan_rows: Vec<String>,
    #[serde(default)]
    pub objectives: Vec<Objective>,
    #[serde(default)]
    pub stories: Vec<Story>,
    pub work_items: Vec<WorkItem>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Objective {
    pub id: String,
    pub text: String,
}

/// A story, lifecycle, data flow, control flow, requirement or issue, with the tests that prove it.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Story {
    pub id: String,
    #[serde(default)]
    pub kind: StoryKind,
    pub text: String,
    #[serde(default)]
    pub objectives: Vec<String>,
    /// `test:<file>::<fn>` keys.
    #[serde(default)]
    pub tests: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct WorkItem {
    pub id: String,
    pub text: String,
    pub status: Status,
    /// Its tests are the tests of these stories.
    #[serde(default)]
    pub stories: Vec<String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub units: Vec<PlannedUnit>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PlannedUnit {
    /// `file:<path>`, `rust:<crate dir>:<module path>::<name>`, `symbol:<file>::impl <type>.<method>`,
    /// `config:<dotted key>`, `op:<operation>`, `test:<file>::<fn>` or `unit:<name>`.
    pub key: String,
    pub change: Change,
    /// Estimated lines of code when done.
    #[serde(default)]
    pub loc: Option<u64>,
}

/// `planning/units.jsonc`: the run, build and deploy units that ctx does not index.
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Units {
    #[serde(rename = "$schema", default)]
    pub schema: Option<String>,
    pub units: Vec<Unit>,
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Unit {
    /// The `unit:<name>` key.
    pub name: String,
    /// systemd, package, image, hook, polkit or another kind.
    pub kind: String,
    /// The unit is present when each of its files is; its LOC is theirs.
    pub files: Vec<String>,
}

/// The JSON schemas of the planning files, with the repository path of each.
///
/// # Errors
/// A schema does not serialize.
pub fn schemas() -> Result<[(&'static str, Value); 2]> {
    Ok([
        (
            "planning/feature.schema.json",
            serde_json::to_value(schemars::schema_for!(Feature))?,
        ),
        (
            "planning/units.schema.json",
            serde_json::to_value(schemars::schema_for!(Units))?,
        ),
    ])
}

/// The plan of one tree: its features (with their files), its units, and the files that did not parse.
struct Plan {
    features: Vec<(String, Feature)>,
    units: Vec<Unit>,
    problems: Vec<String>,
}

fn parse<T: DeserializeOwned>(dir: &Path, tree: &str, path: &str) -> Result<T, String> {
    let bytes = codegraph::run(dir, &["cat-file", "blob", &format!("{tree}:{path}")])
        .map_err(|e| format!("{path}: {e:#}"))?;
    serde_json::from_reader(json_comments::StripComments::new(&bytes[..]))
        .map_err(|e| format!("{path}: {e}"))
}

fn load(dir: &Path, tree: &str) -> Result<Plan> {
    let mut plan = Plan {
        features: Vec::new(),
        units: Vec::new(),
        problems: Vec::new(),
    };
    for path in codegraph::git(dir, &["ls-tree", "-r", "--name-only", tree, "planning"])?.lines() {
        if path == UNITS {
            match parse::<Units>(dir, tree, path) {
                Ok(units) => plan.units = units.units,
                Err(e) => plan.problems.push(e),
            }
        } else if path.starts_with(FEATURES)
            && Path::new(path).extension().is_some_and(|e| e == "jsonc")
        {
            match parse::<Feature>(dir, tree, path) {
                Ok(feature) => plan.features.push((path.to_string(), feature)),
                Err(e) => plan.problems.push(e),
            }
        }
    }
    Ok(plan)
}

/// What the keys of a plan resolve against at one tree.
struct Facts {
    tree: String,
    graph: BTreeMap<String, Node>,
    files: BTreeSet<String>,
    ops: BTreeSet<String>,
    config: Value,
    units: BTreeMap<String, Vec<String>>,
}

/// A unit key resolved at a tree.
struct Found {
    present: bool,
    /// Stable keys of the code graph (to match the change graph).
    keys: Vec<String>,
    /// Files (to count LOC and to match changed files).
    files: Vec<String>,
    /// The span in lines of a symbol.
    span: Option<u64>,
}

impl Facts {
    fn at(dir: &Path, rev: &str, build: bool, plan_units: &[Unit]) -> Result<Facts> {
        let (tree, db) = codegraph::snapshot_for(dir, rev, build)?;
        let blob =
            |path: &str| codegraph::run(dir, &["cat-file", "blob", &format!("{tree}:{path}")]).ok();
        let ops = blob("docs/api/schema.json")
            .and_then(|b| serde_json::from_slice::<serde_json::Map<String, Value>>(&b).ok())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default();
        let config = blob("config/defaults.jsonc")
            .and_then(|b| serde_json::from_reader(json_comments::StripComments::new(&b[..])).ok())
            .unwrap_or_default();
        Ok(Facts {
            graph: codegraph::nodes(&db)?,
            files: codegraph::git(dir, &["ls-tree", "-r", "--name-only", &tree])?
                .lines()
                .map(String::from)
                .collect(),
            units: plan_units
                .iter()
                .map(|u| (u.name.clone(), u.files.clone()))
                .collect(),
            ops,
            config,
            tree,
        })
    }

    /// None: the key has no known prefix.
    fn find(&self, key: &str) -> Option<Found> {
        let symbol = |found: Option<(&String, &Node)>| match found {
            Some((k, n)) => Found {
                present: true,
                keys: vec![k.clone()],
                files: vec![n.file.clone()],
                span: u64::try_from(n.span_lines()).ok(),
            },
            None => Found {
                present: false,
                keys: Vec::new(),
                files: Vec::new(),
                span: None,
            },
        };
        let plain = |present: bool, files: Vec<String>| Found {
            present,
            keys: Vec::new(),
            files,
            span: None,
        };
        let (prefix, rest) = key.split_once(':')?;
        Some(match prefix {
            "file" => plain(self.files.contains(rest), vec![rest.into()]),
            "rust" | "symbol" => symbol(self.graph.get_key_value(key)),
            "test" => {
                let (file, name) = rest.rsplit_once("::")?;
                symbol(
                    self.graph
                        .iter()
                        .find(|(_, n)| n.file == file && n.label == name && n.kind == "function"),
                )
            }
            "op" => plain(self.ops.contains(rest), Vec::new()),
            "config" => plain(
                self.config
                    .pointer(&format!("/{}", rest.replace('.', "/")))
                    .is_some(),
                Vec::new(),
            ),
            "unit" => {
                let files = self.units.get(rest)?;
                plain(files.iter().all(|f| self.files.contains(f)), files.clone())
            }
            _ => return None,
        })
    }
}

/// Whether a unit is where its change puts it: there after add or edit, gone after remove.
fn reached(found: &Found, change: Change) -> bool {
    found.present != (change == Change::Remove)
}

/// Whether a recorded run (the source of its test binary relative to the repository, its libtest name) is of the
/// test `test:<file>::<fn>`.
fn ran(run: &TestRow, key: &str) -> bool {
    key.strip_prefix("test:")
        .and_then(|k| k.rsplit_once("::"))
        .is_some_and(|(file, name)| {
            file == run.file && (run.name == name || run.name.ends_with(&format!("::{name}")))
        })
}

/// The tests of a work item: the tests of its stories.
fn tests_of<'a>(feature: &'a Feature, item: &WorkItem) -> Vec<&'a String> {
    feature
        .stories
        .iter()
        .filter(|s| item.stories.contains(&s.id))
        .flat_map(|s| &s.tests)
        .collect()
}

/// The result of `code plan check`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanCheck {
    pub tree: String,
    pub features: usize,
    pub problems: Vec<String>,
}

/// Validate the plan of `rev`: files that do not parse (unknown keys among them), duplicate ids, links to unknown
/// ids, unit keys with an unknown prefix, units that are not where their change puts them (an `edit` unit, or any
/// unit of a done work item or feature), tests of a done item that are not there, and work items with no tests.
///
/// # Errors
/// The ref does not resolve or has no code graph (without `build`).
pub fn check(dir: &Path, rev: &str, build: bool) -> Result<PlanCheck> {
    let (_, tree) = codegraph::resolve(dir, rev)?;
    let plan = load(dir, &tree)?;
    let facts = Facts::at(dir, rev, build, &plan.units)?;
    let mut problems = plan.problems;
    let mut once = BTreeSet::new();
    for unit in &plan.units {
        if !once.insert(&unit.name) {
            problems.push(format!("{UNITS}: unit {} twice", unit.name));
        }
    }
    let mut ids = BTreeSet::new();
    for (path, f) in &plan.features {
        if !ids.insert(&f.id) {
            problems.push(format!("{path}: feature id {} twice", f.id));
        }
        if *path != format!("{FEATURES}{}.jsonc", f.id) {
            problems.push(format!("{path}: the file name is not the id {}", f.id));
        }
        let mut seen = BTreeSet::new();
        let objectives = f.objectives.iter().map(|o| &o.id);
        let stories = f.stories.iter().map(|s| &s.id);
        let items = f.work_items.iter().map(|w| &w.id);
        for id in objectives.chain(stories).chain(items) {
            if !seen.insert(id) {
                problems.push(format!("{path}: id {id} twice"));
            }
        }
        let unknown = |list: &[String], of: &[&String], what: &str, problems: &mut Vec<String>| {
            for id in list.iter().filter(|id| !of.contains(id)) {
                problems.push(format!("{path}: {what} names {id}, which does not exist"));
            }
        };
        let objective_ids: Vec<&String> = f.objectives.iter().map(|o| &o.id).collect();
        let story_ids: Vec<&String> = f.stories.iter().map(|s| &s.id).collect();
        let item_ids: Vec<&String> = f.work_items.iter().map(|w| &w.id).collect();
        for row in &f.plan_rows {
            if !row
                .split_once('#')
                .is_some_and(|(d, r)| !d.is_empty() && !r.is_empty())
            {
                problems.push(format!("{path}: plan row {row} is not <dossier>#<row>"));
            }
        }
        for story in &f.stories {
            unknown(&story.objectives, &objective_ids, &story.id, &mut problems);
            for test in story.tests.iter().filter(|t| !t.starts_with("test:")) {
                problems.push(format!("{path}: {}: {test} is not a test: key", story.id));
            }
        }
        for item in &f.work_items {
            let at = format!("{path}: {}", item.id);
            unknown(&item.stories, &story_ids, &item.id, &mut problems);
            unknown(&item.depends_on, &item_ids, &item.id, &mut problems);
            let done = item.status == Status::Done || f.status == Status::Done;
            let tests = tests_of(f, item);
            if tests.is_empty() {
                problems.push(format!("{at}: no tests (its stories link none)"));
            }
            for unit in &item.units {
                let Some(found) = facts.find(&unit.key) else {
                    problems.push(format!(
                        "{at}: {} has no known prefix ({})",
                        unit.key,
                        PREFIXES.join(", ")
                    ));
                    continue;
                };
                if !reached(&found, unit.change) && (done || unit.change == Change::Edit) {
                    let state = if found.present {
                        "is still there"
                    } else {
                        "resolves to nothing"
                    };
                    problems.push(format!("{at}: {} {state} at {rev}", unit.key));
                }
            }
            for test in tests.into_iter().filter(|_| done) {
                if !facts.find(test).is_some_and(|f| f.present) {
                    problems.push(format!("{at}: test {test} resolves to nothing at {rev}"));
                }
            }
        }
    }
    Ok(PlanCheck {
        tree: facts.tree,
        features: plan.features.len(),
        problems,
    })
}

/// One planned unit at a tree.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanUnitState {
    pub key: String,
    /// add, edit or remove.
    pub change: String,
    /// done (there after add or edit, gone after remove), missing, or still there (to remove).
    pub state: String,
    pub loc_planned: Option<u64>,
    pub loc_actual: Option<u64>,
    /// tokei (code lines of files) or span (the lines of a symbol, with blanks and comments).
    pub loc_by: Option<String>,
}

/// A test that a work item's stories link.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanTestState {
    pub key: String,
    pub present: bool,
    /// Its last recorded run (`code tests run`): ok, failed or ignored.
    pub result: Option<String>,
    /// The recorded mutants it killed.
    pub kills: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanItemState {
    pub id: String,
    pub text: String,
    pub status: String,
    /// Each unit done, and each linked test there and not failed at its last run.
    pub complete: bool,
    pub loc_planned: u64,
    pub loc_actual: u64,
    pub units: Vec<PlanUnitState>,
    pub tests: Vec<PlanTestState>,
    /// The entities changed since the `since` ref that touch its units: `<change> <key>`.
    pub changed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanFeatureState {
    pub id: String,
    pub title: String,
    pub status: String,
    pub plan_rows: Vec<String>,
    pub loc_planned: u64,
    pub loc_actual: u64,
    pub work_items: Vec<PlanItemState>,
}

/// The result of `code plan status`.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PlanStatus {
    #[serde(rename = "ref")]
    pub rev: String,
    pub tree: String,
    pub since: Option<String>,
    pub features: Vec<PlanFeatureState>,
    /// Plan files that do not parse, and LOC that could not be counted.
    pub notes: Vec<String>,
}

/// tokei's code lines of each file at the tree: `tokei -o json` on the files' blobs in a temporary folder (the paths
/// stay, so tokei finds each language). None when tokei is not installed.
fn tokei(
    dir: &Path,
    tree: &str,
    files: &BTreeSet<&String>,
) -> Result<Option<BTreeMap<String, u64>>> {
    let Some(bin) = crate::setup::tokei_bin() else {
        return Ok(None);
    };
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let tmp = std::env::temp_dir().join(format!("legionctl-tokei-{}.{nanos}", std::process::id()));
    let counted = (|| {
        let mut written = Vec::new();
        for file in files {
            if let Ok(blob) = codegraph::run(dir, &["cat-file", "blob", &format!("{tree}:{file}")])
            {
                let path = tmp.join(file.as_str());
                std::fs::create_dir_all(path.parent().context("a file path has no folder")?)?;
                std::fs::write(path, blob)?;
                written.push(file.as_str());
            }
        }
        let mut loc = BTreeMap::new();
        if written.is_empty() {
            return Ok(loc);
        }
        let out = Command::new(&bin)
            .current_dir(&tmp)
            .args(["-o", "json"])
            .args(&written)
            .output()
            .context("running tokei")?;
        ensure!(
            out.status.success(),
            "tokei: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        );
        let json: serde_json::Map<String, Value> = serde_json::from_slice(&out.stdout)?;
        for (language, stats) in json.iter().filter(|(l, _)| *l != "Total") {
            for report in stats["reports"].as_array().into_iter().flatten() {
                let name = report["name"].as_str().unwrap_or_default();
                let code = report["stats"]["code"].as_u64().unwrap_or(0);
                ensure!(
                    !name.is_empty(),
                    "tokei gave a {language} report with no name"
                );
                *loc.entry(name.trim_start_matches("./").to_string())
                    .or_insert(0) += code;
            }
        }
        Ok(loc)
    })();
    let _ = std::fs::remove_dir_all(&tmp);
    counted.map(Some)
}

/// Each feature (or one) of the plan at `rev` against its code graph: units done or missing, LOC planned and actual
/// (tokei for files and units, the span for symbols), the linked tests, and with `since` the changed entities that
/// touch each work item (`code graph changes <since> <rev>`).
///
/// # Errors
/// A ref does not resolve or has no code graph (without `build`), no such feature, or tokei fails.
pub fn status(
    dossiers: &Dossiers,
    dir: &Path,
    feature: Option<&str>,
    rev: &str,
    since: Option<&str>,
    build: bool,
) -> Result<PlanStatus> {
    let (_, tree) = codegraph::resolve(dir, rev)?;
    let plan = load(dir, &tree)?;
    let facts = Facts::at(dir, rev, build, &plan.units)?;
    let mut notes = plan.problems;
    let features: Vec<&Feature> = plan
        .features
        .iter()
        .map(|(_, f)| f)
        .filter(|f| feature.is_none_or(|id| f.id == id))
        .collect();
    if let Some(id) = feature
        && features.is_empty()
    {
        return Err(coded(Code::NotFound, format!("no feature {id} at {rev}")));
    }
    let changes = match since {
        Some(from) => codegraph::changes(dir, &[from.into(), rev.into()], build)?
            .into_iter()
            .flat_map(|set| set.entities)
            .collect(),
        None => Vec::new(),
    };
    let found: BTreeMap<&String, Option<Found>> = features
        .iter()
        .flat_map(|f| &f.work_items)
        .flat_map(|w| &w.units)
        .map(|u| (&u.key, facts.find(&u.key)))
        .collect();
    let counted: BTreeSet<&String> = found
        .iter()
        .filter(|(k, _)| k.starts_with("file:") || k.starts_with("unit:"))
        .filter_map(|(_, f)| f.as_ref())
        .flat_map(|f| &f.files)
        .collect();
    let loc = tokei(dir, &facts.tree, &counted)?;
    let runs = dossiers.tests_list(dir)?;
    if loc.is_none() && !counted.is_empty() {
        notes.push("tokei is not installed: no LOC for file: and unit: units".into());
    }
    let mut out = Vec::new();
    for f in features {
        let mut items = Vec::new();
        for item in &f.work_items {
            let mut units = Vec::new();
            let mut touched = Vec::new();
            for unit in &item.units {
                let found = found.get(&unit.key).and_then(Option::as_ref);
                units.push(unit_state(unit, found, loc.as_ref()));
                touched.extend(touching(unit, found, &changes));
            }
            touched.sort();
            touched.dedup();
            let tests: Vec<PlanTestState> = tests_of(f, item)
                .into_iter()
                .map(|t| {
                    let run = runs.iter().find(|r| ran(r, t));
                    PlanTestState {
                        key: t.clone(),
                        present: facts.find(t).is_some_and(|f| f.present),
                        result: run.map(|r| r.result.clone()),
                        kills: run.map_or(0, |r| r.kills),
                    }
                })
                .collect();
            items.push(PlanItemState {
                id: item.id.clone(),
                text: item.text.clone(),
                status: status_name(item.status).into(),
                complete: units.iter().all(|u| u.state == "done")
                    && tests
                        .iter()
                        .all(|t| t.present && t.result.as_deref() != Some("failed")),
                loc_planned: units.iter().filter_map(|u| u.loc_planned).sum(),
                loc_actual: units.iter().filter_map(|u| u.loc_actual).sum(),
                units,
                tests,
                changed: touched,
            });
        }
        out.push(PlanFeatureState {
            id: f.id.clone(),
            title: f.title.clone(),
            status: status_name(f.status).into(),
            plan_rows: f.plan_rows.clone(),
            loc_planned: items.iter().map(|i| i.loc_planned).sum(),
            loc_actual: items.iter().map(|i| i.loc_actual).sum(),
            work_items: items,
        });
    }
    Ok(PlanStatus {
        rev: rev.into(),
        tree: facts.tree,
        since: since.map(String::from),
        features: out,
        notes,
    })
}

/// A planned unit at the tree: its state, and its LOC (the span of a symbol, tokei for files and units).
fn unit_state(
    unit: &PlannedUnit,
    found: Option<&Found>,
    loc: Option<&BTreeMap<String, u64>>,
) -> PlanUnitState {
    let (state, loc_actual, loc_by) = match found {
        None => ("unknown prefix", None, None),
        Some(found) => {
            let state = match (reached(found, unit.change), found.present) {
                (true, _) => "done",
                (false, true) => "still there",
                (false, false) => "missing",
            };
            match (found.span, loc) {
                (Some(span), _) => (state, Some(span), Some("span")),
                (None, Some(loc)) if !found.files.is_empty() && found.present => (
                    state,
                    Some(found.files.iter().filter_map(|f| loc.get(f)).sum()),
                    Some("tokei"),
                ),
                _ => (state, None, None),
            }
        }
    };
    PlanUnitState {
        key: unit.key.clone(),
        change: change_name(unit.change).into(),
        state: state.into(),
        loc_planned: unit.loc,
        loc_actual,
        loc_by: loc_by.map(String::from),
    }
}

/// The changed entities that touch a planned unit, as `<change> <key>`: its key (also a removed one), its symbol's
/// key at the tree, and for a file or unit each entity in its files.
fn touching(
    unit: &PlannedUnit,
    found: Option<&Found>,
    changes: &[codegraph::EntityChange],
) -> Vec<String> {
    let by_file = unit.key.starts_with("file:") || unit.key.starts_with("unit:");
    changes
        .iter()
        .filter(|e| {
            e.key == unit.key
                || found.is_some_and(|f| {
                    f.keys.contains(&e.key) || (by_file && f.files.contains(&e.file))
                })
        })
        .map(|e| format!("{} {}", e.change, e.key))
        .collect()
}

const fn change_name(change: Change) -> &'static str {
    match change {
        Change::Add => "add",
        Change::Edit => "edit",
        Change::Remove => "remove",
    }
}

const fn status_name(status: Status) -> &'static str {
    match status {
        Status::Planned => "planned",
        Status::InProgress => "in-progress",
        Status::Done => "done",
        Status::Abandoned => "abandoned",
    }
}

/// `dossier check` findings of the plan at the HEAD of the dossier's workspace `dir`: a feature that names a plan row
/// of this dossier that does not exist, and a plan row whose status says done or built while a feature that names it
/// has work items that are not complete. Nothing when the repository keeps no code graphs or HEAD has none.
#[must_use]
pub fn row_findings(
    dossiers: &Dossiers,
    dossier: &str,
    graph: &crate::graph::Graph,
    dir: &Path,
) -> Vec<String> {
    if !codegraph::enabled(dir) {
        return Vec::new();
    }
    let Ok(state) = status(dossiers, dir, None, "HEAD", None, false) else {
        return Vec::new();
    };
    // a link keeps the name the dossier had when it was written (the registry keeps old names)
    let names = dossiers
        .names_of(dossier)
        .unwrap_or_else(|_| vec![dossier.to_string()]);
    let mut out = Vec::new();
    for f in &state.features {
        for row in f.plan_rows.iter().filter_map(|r| {
            r.split_once('#')
                .filter(|(d, _)| names.iter().any(|n| n == d))
                .map(|(_, row)| row)
        }) {
            let Some(node) = graph.nodes.iter().find(|n| n.id == format!("plan#{row}")) else {
                out.push(format!(
                    "feature {} names plan#{row}, which does not exist",
                    f.id
                ));
                continue;
            };
            let claims_done = node.status.as_deref().is_some_and(|s| {
                s.split_whitespace().next().is_some_and(|w| {
                    matches!(
                        w.trim_end_matches(':').to_lowercase().as_str(),
                        "done" | "built"
                    )
                })
            });
            let open: Vec<String> = f
                .work_items
                .iter()
                .filter(|w| !w.complete)
                .map(|w| {
                    let missing: Vec<&str> = w
                        .units
                        .iter()
                        .filter(|u| u.state != "done")
                        .map(|u| u.key.as_str())
                        .chain(
                            w.tests
                                .iter()
                                .filter(|t| !t.present)
                                .map(|t| t.key.as_str()),
                        )
                        .collect();
                    format!("{} ({})", w.id, missing.join(", "))
                })
                .collect();
            if claims_done && !open.is_empty() {
                out.push(format!(
                    "plan#{row} says done, but feature {} is not complete at HEAD: {}",
                    f.id,
                    open.join("; ")
                ));
            }
        }
    }
    out
}
