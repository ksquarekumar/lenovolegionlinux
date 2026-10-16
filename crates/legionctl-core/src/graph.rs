//! The work graph of a dossier (note 18, W4), built from its files on each call (a dossier is tens of files, and a
//! graph made on demand is never out of date): every `plan.md` table row and every artifact is a node; front matter
//! (`supports`, `supersedes`, `code`) and artifact paths named in plan rows are edges. `dossier check` reports what
//! the practice asks for from it: dangling links, acceptance rows that no EVIDENCE supports, and EVIDENCE whose code
//! changed after it was written.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Node {
    /// `plan#todo-3`, an artifact's front-matter id, or its path without `.md` (`MEMORY/03-workspace`).
    pub id: String,
    /// The plan section of a row (`ToDo`), or the artifact's class (`EVIDENCE/validation`).
    pub kind: String,
    /// The file it comes from, relative to the dossier.
    pub file: String,
    pub status: Option<String>,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct Edge {
    pub from: String,
    pub to: String,
    /// supports, supersedes, code, refers-to.
    pub kind: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct Graph {
    pub nodes: Vec<Node>,
    pub edges: Vec<Edge>,
    /// The status changes of the node asked for (`graph --node`), oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history: Vec<StatusChange>,
}

/// A node's status changed in a write through legionctl (null: the node did not exist or had no status).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StatusChange {
    /// Seconds since the epoch.
    pub ts: i64,
    pub session: String,
    pub node: String,
    pub from: Option<String>,
    pub to: Option<String>,
}

/// The status of each node that `file` (relative to the dossier) holds in `text`: the plan rows, or the artifact.
fn statuses(file: &str, text: &str) -> BTreeMap<String, Option<String>> {
    if file == "plan.md" {
        let mut graph = Graph::default();
        plan_rows(text, &BTreeMap::new(), &mut graph);
        return graph.nodes.into_iter().map(|n| (n.id, n.status)).collect();
    }
    if Path::new(file).extension().is_none_or(|e| e != "md") {
        return BTreeMap::new();
    }
    let fm = front_matter(text);
    let id = fm
        .get("id")
        .and_then(|v| v.first())
        .cloned()
        .unwrap_or_else(|| file.trim_end_matches(".md").to_string());
    BTreeMap::from([(id, fm.get("status").and_then(|v| v.first()).cloned())])
}

/// The nodes of `file` whose status differs between `old` and `new`: node, from, to.
#[must_use]
pub fn status_changes(
    file: &str,
    old: &str,
    new: &str,
) -> Vec<(String, Option<String>, Option<String>)> {
    let (before, after) = (statuses(file, old), statuses(file, new));
    let ids: BTreeSet<&String> = before.keys().chain(after.keys()).collect();
    ids.into_iter()
        .filter_map(|id| {
            let from = before.get(id).cloned().flatten();
            let to = after.get(id).cloned().flatten();
            (from != to).then(|| (id.clone(), from, to))
        })
        .collect()
}

/// The front-matter links of an artifact's text: `supports` and `supersedes` targets.
fn links(text: &str) -> BTreeSet<String> {
    let fm = front_matter(text);
    ["supports", "supersedes"]
        .iter()
        .flat_map(|k| fm.get(*k).into_iter().flatten().cloned())
        .collect()
}

/// The plan row ids that `plan` has more than once.
fn duplicate_rows(plan: &str) -> BTreeSet<String> {
    let mut graph = Graph::default();
    plan_rows(plan, &BTreeMap::new(), &mut graph);
    let mut seen = BTreeSet::new();
    graph
        .nodes
        .into_iter()
        .filter(|n| !seen.insert(n.id.clone()))
        .map(|n| n.id)
        .collect()
}

/// Refuse a write of `file` (relative to the dossier at `dir`; `plan.md`, an artifact, or empty for a new numbered
/// artifact) from `old` to `new` that adds a plan row id that is there already, or a front-matter link to no node; a
/// bare plan row id (`todo-12`) gets its form (`plan#todo-12`) in the refusal (todo-121). Links and duplicate rows that
/// `old` has already do not refuse a write.
///
/// # Errors
/// The write adds a duplicate row id or a link to no node.
pub fn check_write(dir: &Path, file: &str, old: &str, new: &str) -> anyhow::Result<()> {
    if file == "plan.md" {
        let before = duplicate_rows(old);
        let added: Vec<String> = duplicate_rows(new)
            .into_iter()
            .filter(|id| !before.contains(id))
            .collect();
        anyhow::ensure!(
            added.is_empty(),
            "plan.md would have the row {} twice: give the new row the next free id",
            added.join(", ")
        );
        return Ok(());
    }
    let before = links(old);
    let added: Vec<String> = links(new)
        .into_iter()
        .filter(|l| !before.contains(l))
        .collect();
    if added.is_empty() {
        return Ok(());
    }
    let graph = build(dir);
    let own = front_matter(new).get("id").and_then(|v| v.first()).cloned();
    let ids: BTreeSet<&str> = graph
        .nodes
        .iter()
        .map(|n| n.id.as_str())
        .chain(own.as_deref())
        .collect();
    for link in added {
        if ids.contains(link.as_str()) {
            continue;
        }
        let row = format!("plan#{link}");
        anyhow::ensure!(
            !ids.contains(row.as_str()),
            "the front matter links to {link}, which is no node: a plan row is {row}"
        );
        anyhow::bail!(
            "the front matter links to {link}, which does not exist: link a plan row as plan#<id>, an artifact by its \
             id (legionctl dossier graph lists the nodes)"
        );
    }
    Ok(())
}

/// `key: value` and `key: [a, b]` lines between the leading `---` lines.
pub(crate) fn front_matter(text: &str) -> BTreeMap<String, Vec<String>> {
    let mut out = BTreeMap::new();
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("---") {
        return out;
    }
    for line in lines.take_while(|l| l.trim() != "---") {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim();
        let values = match value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
            Some(list) => list
                .split(',')
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty())
                .collect(),
            None => vec![value.to_string()],
        };
        out.insert(key.trim().to_string(), values);
    }
    out
}

fn cells(line: &str) -> Option<Vec<String>> {
    let inner = line.trim().strip_prefix('|')?.strip_suffix('|')?;
    Some(inner.split('|').map(|c| c.trim().to_string()).collect())
}

/// The node ids of the artifacts whose paths (`MEMORY/03-x.md`) a text names.
fn paths_in(text: &str, artifacts: &BTreeMap<String, String>) -> Vec<String> {
    artifacts
        .iter()
        .filter(|(path, _)| text.contains(&format!("{path}.md")))
        .map(|(_, id)| id.clone())
        .collect()
}

fn plan_rows(plan: &str, artifacts: &BTreeMap<String, String>, graph: &mut Graph) {
    let mut section = String::new();
    let mut header: Option<Vec<String>> = None;
    for line in plan.lines() {
        if let Some(title) = line.strip_prefix("## ") {
            section = title.trim().to_string();
            header = None;
            continue;
        }
        let Some(row) = cells(line) else {
            header = None;
            continue;
        };
        if row
            .iter()
            .all(|c| c.chars().all(|ch| ch == '-' || ch == ':' || ch == ' '))
        {
            continue;
        }
        let Some(head) = &header else {
            header = Some(row.iter().map(|c| c.to_lowercase()).collect());
            continue;
        };
        let Some(id) = row.first().filter(|id| !id.is_empty() && !id.contains(' ')) else {
            continue;
        };
        let id = format!("plan#{id}");
        let status = head
            .iter()
            .position(|h| h == "status")
            .and_then(|i| row.get(i))
            .cloned();
        let text = row.join(" | ");
        for to in paths_in(&text, artifacts) {
            graph.edges.push(Edge {
                from: id.clone(),
                to,
                kind: "refers-to".into(),
            });
        }
        graph.nodes.push(Node {
            id,
            kind: section.clone(),
            file: "plan.md".into(),
            status,
            title: row.get(1).cloned().unwrap_or_default(),
        });
    }
}

fn markdown(dir: &Path, base: &Path, out: &mut Vec<String>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        if path.is_dir() {
            markdown(&path, base, out);
        } else if path.extension().is_some_and(|e| e == "md")
            && let Ok(rel) = path.strip_prefix(base)
            && rel != Path::new("plan.md")
        {
            out.push(rel.to_string_lossy().into_owned());
        }
    }
}

/// The graph of the dossier at `dir`.
#[must_use]
pub fn build(dir: &Path) -> Graph {
    let mut files = Vec::new();
    markdown(dir, dir, &mut files);
    files.sort();
    let mut graph = Graph::default();
    // path without .md -> node id (the front-matter id when there is one)
    let mut artifacts = BTreeMap::new();
    for file in &files {
        let text = std::fs::read_to_string(dir.join(file)).unwrap_or_default();
        let fm = front_matter(&text);
        let path_id = file.trim_end_matches(".md").to_string();
        let id = fm
            .get("id")
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_else(|| path_id.clone());
        artifacts.insert(path_id, id.clone());
        let class = fm
            .get("class")
            .and_then(|v| v.first())
            .cloned()
            .unwrap_or_else(|| {
                let parts: Vec<&str> = file.split('/').collect();
                parts[..parts.len().saturating_sub(1)].join("/")
            });
        for (key, kind) in [("supports", "supports"), ("supersedes", "supersedes")] {
            for to in fm.get(key).into_iter().flatten() {
                graph.edges.push(Edge {
                    from: id.clone(),
                    to: to.clone(),
                    kind: kind.into(),
                });
            }
        }
        for code in fm.get("code").into_iter().flatten() {
            graph.edges.push(Edge {
                from: id.clone(),
                to: format!("code:{code}"),
                kind: "code".into(),
            });
        }
        let title = text
            .lines()
            .find_map(|l| l.strip_prefix("# "))
            .unwrap_or(file)
            .trim()
            .to_string();
        graph.nodes.push(Node {
            id,
            kind: class,
            file: file.clone(),
            status: fm.get("status").and_then(|v| v.first()).cloned(),
            title,
        });
    }
    if let Ok(plan) = std::fs::read_to_string(dir.join("plan.md")) {
        plan_rows(&plan, &artifacts, &mut graph);
    }
    graph.edges.sort();
    graph.edges.dedup();
    graph
}

impl Graph {
    /// `id` and the nodes one edge away, with those edges.
    #[must_use]
    pub fn around(&self, id: &str) -> Graph {
        let edges: Vec<Edge> = self
            .edges
            .iter()
            .filter(|e| e.from == id || e.to == id)
            .cloned()
            .collect();
        let near: BTreeSet<&str> = edges
            .iter()
            .flat_map(|e| [e.from.as_str(), e.to.as_str()])
            .chain([id])
            .collect();
        Graph {
            nodes: self
                .nodes
                .iter()
                .filter(|n| near.contains(n.id.as_str()))
                .cloned()
                .collect(),
            edges,
            history: Vec::new(),
        }
    }

    /// What the practice asks for: links to nodes that do not exist, acceptance rows that no EVIDENCE supports, and
    /// EVIDENCE whose code changed after it was written (`changed_after(path, written)` asks the repository).
    #[must_use]
    pub fn findings(
        &self,
        dir: &Path,
        changed_after: &dyn Fn(&str, std::time::SystemTime) -> anyhow::Result<bool>,
    ) -> Vec<String> {
        let ids: BTreeSet<&str> = self.nodes.iter().map(|n| n.id.as_str()).collect();
        let mut out: Vec<String> = self
            .edges
            .iter()
            .filter(|e| e.kind != "code" && !ids.contains(e.to.as_str()))
            .map(|e| format!("{} {} {}, which does not exist", e.from, e.kind, e.to))
            .collect();
        for row in self
            .nodes
            .iter()
            .filter(|n| n.kind == "Acceptance Criteria")
        {
            let verified = self.edges.iter().any(|e| {
                e.to == row.id
                    && e.kind == "supports"
                    && self
                        .nodes
                        .iter()
                        .any(|n| n.id == e.from && n.kind.starts_with("EVIDENCE"))
            });
            if !verified {
                out.push(format!("{} has no EVIDENCE that supports it", row.id));
            }
        }
        for evidence in self.nodes.iter().filter(|n| n.kind.starts_with("EVIDENCE")) {
            let Ok(written) =
                std::fs::metadata(dir.join(&evidence.file)).and_then(|m| m.modified())
            else {
                continue;
            };
            for edge in self
                .edges
                .iter()
                .filter(|e| e.from == evidence.id && e.kind == "code")
            {
                let path = edge.to.trim_start_matches("code:");
                let file = path.split('#').next().unwrap_or(path);
                match changed_after(file, written) {
                    Ok(true) => out.push(format!(
                        "{} is stale: {file} changed after it was written",
                        evidence.id
                    )),
                    Ok(false) => {}
                    Err(e) => out.push(format!(
                        "{}: cannot tell if {file} changed after it was written: {e:#}",
                        evidence.id
                    )),
                }
            }
        }
        out
    }
}
