//! Dossiers (note 18): an agent's own space for one scope, outside the product's git tree, in
//! `<dossier.root>/<dossier>/`. A session binds to one dossier at a time and writes only there; reads work on every
//! dossier. The pattern follows the scope-dossier practice of `safecoder dossier`: hash-anchored line edits, numbered
//! artifacts per class folder, SQLite bindings with a stale rule, and a plan ledger with fixed sections.

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use blake2::digest::consts::U2;
use blake2::{Blake2b, Digest};
use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::DossierConfig;
use crate::db::Db;

const SCHEMA: &[&str] = &[
    // v1: one open binding per dossier and per session; released rows stay as history
    "CREATE TABLE bindings (
        session  TEXT NOT NULL,
        tool     TEXT NOT NULL,
        dossier  TEXT NOT NULL,
        acquired INTEGER NOT NULL,
        updated  INTEGER NOT NULL,
        released INTEGER
     );
     CREATE UNIQUE INDEX open_dossier ON bindings (dossier) WHERE released IS NULL;
     CREATE UNIQUE INDEX open_session ON bindings (session) WHERE released IS NULL;
     CREATE TABLE writes (
        ts      INTEGER NOT NULL,
        session TEXT NOT NULL,
        kind    TEXT NOT NULL,
        path    TEXT NOT NULL
     );",
    // v2: where the session worked (repository root, else its folder), for binding the next session there
    "ALTER TABLE bindings ADD COLUMN place TEXT;",
];

/// Where a session works, for binding the next session there: the repository that contains `dir`, else `dir`.
#[must_use]
pub fn place_of(dir: &Path) -> PathBuf {
    crate::config::repo_root(dir).unwrap_or_else(|| dir.to_path_buf())
}
const DB_FILE: &str = "workspace.db";

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// The 4-hex-digit anchor of a line: `BLAKE2b` with a 2-byte digest over the line's UTF-8 text (as `safecoder`).
#[must_use]
pub fn line_hash(text: &str) -> String {
    let out = Blake2b::<U2>::digest(text.as_bytes());
    format!("{:02x}{:02x}", out[0], out[1])
}

/// Lines of a file: `\r\n` and `\r` count as line ends; there is no empty last line.
fn lines(text: &str) -> Vec<String> {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .lines()
        .map(String::from)
        .collect()
}

fn hashline(number: usize, text: &str) -> String {
    format!("{number}:{}|{text}", line_hash(text))
}

/// `12:a3f0` -> (12, "a3f0")
fn parse_anchor(anchor: &str) -> Result<(usize, String)> {
    let parsed = anchor.split_once(':').and_then(|(n, h)| {
        let valid = !n.starts_with('0')
            && h.len() == 4
            && h.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase());
        Some((
            n.parse::<usize>().ok().filter(|n| *n > 0 && valid)?,
            h.to_string(),
        ))
    });
    parsed.with_context(|| format!("anchor must look like 12:a3f0, got '{anchor}'"))
}

fn valid_name(name: &str) -> bool {
    name.chars()
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum EditMode {
    /// Replace the lines from start to end (inclusive) with the content; empty content deletes them.
    Replace,
    /// Insert the content after the start line.
    InsertAfter,
    /// Delete the lines from start to end.
    Delete,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Lock {
    pub dossier: String,
    pub session: String,
    pub tool: String,
    /// Seconds since the epoch.
    pub updated: i64,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct DossierInfo {
    pub name: String,
    pub has_plan: bool,
}

pub struct Workspace {
    pub root: PathBuf,
    db: Db,
    config: DossierConfig,
}

impl Workspace {
    /// The workspace of `config.root` (`~/` is the home folder).
    ///
    /// # Errors
    /// The root or its database cannot be created.
    pub fn open(config: &DossierConfig) -> Result<Workspace> {
        let root = match config.root.strip_prefix("~/") {
            Some(rest) => {
                PathBuf::from(std::env::var_os("HOME").context("HOME is not set")?).join(rest)
            }
            None => PathBuf::from(&config.root),
        };
        std::fs::create_dir_all(&root).with_context(|| format!("creating {}", root.display()))?;
        Ok(Workspace {
            db: Db::open(&root.join(DB_FILE), SCHEMA)?,
            root,
            config: config.clone(),
        })
    }

    fn stale_after(&self) -> i64 {
        i64::try_from(self.config.stale_hours.saturating_mul(3600)).unwrap_or(i64::MAX)
    }

    /// The dossier this session may write, if any.
    ///
    /// # Errors
    /// The database read fails.
    pub fn active(&self, session: &str) -> Result<Option<String>> {
        Ok(self
            .db
            .conn()
            .query_row(
                "SELECT dossier FROM bindings WHERE session = ?1 AND released IS NULL",
                [session],
                |r| r.get(0),
            )
            .optional()?)
    }

    /// Bind `session` to `name`, releasing its previous dossier. A dossier held by another live session is refused
    /// unless `steal`; a binding idle for `stale_hours` is taken over.
    ///
    /// # Errors
    /// `name` is not valid, does not exist without `create`, or is held by another session.
    pub fn bind(
        &self,
        session: &str,
        tool: &str,
        name: &str,
        create: bool,
        steal: bool,
        place: Option<&Path>,
    ) -> Result<Vec<String>> {
        ensure!(
            valid_name(name),
            "dossier name must be a-z, 0-9 and '-', starting with a letter or digit"
        );
        let mut out = Vec::new();
        let dir = self.root.join(name);
        if !dir.is_dir() {
            ensure!(
                create,
                "dossier '{name}' does not exist; pass create to start it"
            );
            out.extend(self.create(name)?);
        }
        let stale_after = self.stale_after();
        // IMMEDIATE: two sessions binding at once see each other's row
        let tx = Transaction::new_unchecked(self.db.conn(), TransactionBehavior::Immediate)?;
        let holder: Option<(String, String, i64)> = tx
            .query_row(
                "SELECT session, tool, updated FROM bindings WHERE dossier = ?1 AND released IS NULL",
                [name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let now = now();
        match holder {
            Some((holder, _, _)) if holder == session => {
                tx.execute(
                    "UPDATE bindings SET updated = ?1 WHERE session = ?2 AND released IS NULL",
                    params![now, session],
                )?;
                tx.commit()?;
                out.push(format!("active dossier: {name} ({})", dir.display()));
                return Ok(out);
            }
            Some((holder, holder_tool, updated)) => {
                let stale = now - updated > stale_after;
                ensure!(
                    stale || steal,
                    "dossier '{name}' is held by session {holder} ({holder_tool}), last active {} min ago; use steal to take it over",
                    (now - updated) / 60
                );
                tx.execute(
                    "UPDATE bindings SET released = ?1 WHERE dossier = ?2 AND released IS NULL",
                    params![now, name],
                )?;
                out.push(format!(
                    "{name} was held by session {holder} ({})",
                    if stale {
                        "stale"
                    } else {
                        "taken over with steal"
                    }
                ));
            }
            None => {}
        }
        let previous: Option<String> = tx
            .query_row(
                "SELECT dossier FROM bindings WHERE session = ?1 AND released IS NULL",
                [session],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(previous) = previous {
            tx.execute(
                "UPDATE bindings SET released = ?1 WHERE session = ?2 AND released IS NULL",
                params![now, session],
            )?;
            out.insert(0, format!("released {previous}"));
        }
        tx.execute(
            "INSERT INTO bindings (session, tool, dossier, acquired, updated, place) VALUES (?1, ?2, ?3, ?4, ?4, ?5)",
            params![session, tool, name, now, place.map(|p| p.to_string_lossy().into_owned())],
        )?;
        tx.commit()?;
        out.push(format!("active dossier: {name} ({})", dir.display()));
        let memory = self.recent_memory(name, 3);
        if !memory.is_empty() {
            out.push("newest memory:".into());
            out.extend(memory.into_iter().map(|m| format!("  {m}")));
        }
        Ok(out)
    }

    /// The dossier the last session in `place` bound, if it still exists.
    ///
    /// # Errors
    /// The database read fails.
    pub fn last_in(&self, place: &Path) -> Result<Option<String>> {
        let last: Option<String> = self
            .db
            .conn()
            .query_row(
                "SELECT dossier FROM bindings WHERE place = ?1 ORDER BY acquired DESC LIMIT 1",
                [place.to_string_lossy()],
                |r| r.get(0),
            )
            .optional()?;
        Ok(last.filter(|d| self.root.join(d).is_dir()))
    }

    /// # Errors
    /// The database write fails.
    pub fn release(&self, session: &str) -> Result<Option<String>> {
        let active = self.active(session)?;
        self.db.conn().execute(
            "UPDATE bindings SET released = ?1 WHERE session = ?2 AND released IS NULL",
            params![now(), session],
        )?;
        Ok(active)
    }

    /// # Errors
    /// The database read fails.
    pub fn locks(&self) -> Result<Vec<Lock>> {
        let stale_after = self.stale_after();
        let now = now();
        let mut stmt = self
            .db
            .conn()
            .prepare("SELECT dossier, session, tool, updated FROM bindings WHERE released IS NULL ORDER BY dossier")?;
        let rows = stmt.query_map([], |r| {
            let updated: i64 = r.get(3)?;
            Ok(Lock {
                dossier: r.get(0)?,
                session: r.get(1)?,
                tool: r.get(2)?,
                updated,
                stale: now - updated > stale_after,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// The class folders, `plan.md` with every ledger section, and `dossier.jsonc` with the title.
    fn create(&self, name: &str) -> Result<Vec<String>> {
        let dir = self.root.join(name);
        let mut made = vec![format!("created {}", dir.display())];
        for class in self.config.classes.keys() {
            std::fs::create_dir_all(dir.join(class))?;
        }
        let mut plan = format!("# {name}\n");
        for section in &self.config.plan_sections {
            let _ = write!(plan, "\n## {section}\n");
        }
        atomic_write(&dir.join("plan.md"), &plan)?;
        atomic_write(
            &dir.join("dossier.jsonc"),
            &format!(
                "// This dossier's data: title, entities (repos, folders) and objectives (Y1, Y2, ...).\n{}\n",
                serde_json::to_string_pretty(
                    &serde_json::json!({"title": name, "entities": [], "objectives": []})
                )?
            ),
        )?;
        made.push("plan.md, dossier.jsonc and the class folders".into());
        Ok(made)
    }

    /// Dossiers, most recently changed first.
    #[must_use]
    pub fn list(&self) -> Vec<DossierInfo> {
        let mut dirs: Vec<(SystemTime, DossierInfo)> = std::fs::read_dir(&self.root)
            .into_iter()
            .flatten()
            .flatten()
            .filter(|e| e.path().is_dir() && valid_name(&e.file_name().to_string_lossy()))
            .map(|e| {
                let modified = e
                    .metadata()
                    .and_then(|m| m.modified())
                    .unwrap_or(UNIX_EPOCH);
                let info = DossierInfo {
                    name: e.file_name().to_string_lossy().into_owned(),
                    has_plan: e.path().join("plan.md").is_file(),
                };
                (modified, info)
            })
            .collect();
        dirs.sort_by_key(|d| std::cmp::Reverse(d.0));
        dirs.into_iter().map(|(_, d)| d).collect()
    }

    /// `MEMORY/NN-<topic>.md  <title>`, newest first.
    fn recent_memory(&self, name: &str, limit: usize) -> Vec<String> {
        let dir = self.root.join(name).join("MEMORY");
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "md"))
            .collect();
        files.sort();
        files
            .iter()
            .rev()
            .take(limit)
            .map(|p| {
                let text = std::fs::read_to_string(p).unwrap_or_default();
                let title = text
                    .lines()
                    .find_map(|l| l.strip_prefix("# "))
                    .unwrap_or_default()
                    .to_string();
                format!(
                    "MEMORY/{}  {title}",
                    p.file_name().unwrap_or_default().to_string_lossy()
                )
            })
            .collect()
    }

    /// The absolute path of `rel` (`<dossier>/...`), refusing anything that leaves the dossier.
    fn resolve(&self, rel: &str) -> Result<(String, PathBuf)> {
        let path = Path::new(rel);
        let mut parts = Vec::new();
        for component in path.components() {
            match component {
                Component::Normal(part) => {
                    let part = part.to_string_lossy();
                    ensure!(
                        !part.starts_with('.'),
                        "{rel}: hidden paths are not dossier files"
                    );
                    parts.push(part.into_owned());
                }
                _ => bail!(
                    "{rel}: use a path relative to the workspace root, such as <dossier>/plan.md"
                ),
            }
        }
        let dossier = parts.first().cloned().context("empty path")?;
        ensure!(
            valid_name(&dossier),
            "{rel}: '{dossier}' is not a dossier name"
        );
        let full = self.root.join(rel);
        // a symlink inside the dossier must not lead out of it
        if let Ok(real) = full.canonicalize() {
            let base = self.root.join(&dossier).canonicalize()?;
            ensure!(real.starts_with(&base), "{rel} leads out of the dossier");
        }
        Ok((dossier, full))
    }

    fn guard_write(&self, session: &str, rel: &str) -> Result<PathBuf> {
        let (dossier, full) = self.resolve(rel)?;
        let active = self.active(session)?;
        if active.as_deref() != Some(&dossier) {
            let holder: Option<String> = self
                .db
                .conn()
                .query_row(
                    "SELECT session FROM bindings WHERE dossier = ?1 AND released IS NULL",
                    [&dossier],
                    |r| r.get(0),
                )
                .optional()?;
            bail!(
                "{dossier} is not this session's active dossier{}. Run `legionctl dossier use {dossier}` (add --create for a new dossier) first.",
                holder.map_or(String::new(), |h| format!("; it is active in session {h}"))
            );
        }
        self.db.conn().execute(
            "UPDATE bindings SET updated = ?1 WHERE session = ?2 AND released IS NULL",
            params![now(), session],
        )?;
        Ok(full)
    }

    fn log(&self, session: &str, kind: &str, rel: &str) -> Result<()> {
        self.db.conn().execute(
            "INSERT INTO writes (ts, session, kind, path) VALUES (?1, ?2, ?3, ?4)",
            params![now(), session, kind, rel],
        )?;
        Ok(())
    }

    /// Whether `session` may write the absolute `path`: anything outside the workspace root passes; inside, only
    /// the session's active dossier. Returns the reason for a refusal.
    ///
    /// # Errors
    /// The database read fails.
    pub fn write_refusal(&self, session: &str, path: &Path) -> Result<Option<String>> {
        let Ok(rel) = path.strip_prefix(&self.root) else {
            return Ok(None);
        };
        let rel = rel.to_string_lossy();
        Ok(self.guard_write(session, &rel).err().map(|e| e.to_string()))
    }

    /// Lines `first..=last` (1-based; `last` defaults to the end) as `<line>:<hash>|<text>`.
    ///
    /// # Errors
    /// The path is not a dossier file or cannot be read.
    pub fn read(&self, rel: &str, first: usize, last: Option<usize>) -> Result<Vec<String>> {
        let (_, full) = self.resolve(rel)?;
        let text = std::fs::read_to_string(&full).with_context(|| format!("reading {rel}"))?;
        Ok(lines(&text)
            .iter()
            .enumerate()
            .map(|(i, l)| (i + 1, l))
            .filter(|(n, _)| *n >= first.max(1) && last.is_none_or(|last| *n <= last))
            .map(|(n, l)| hashline(n, l))
            .collect())
    }

    /// Lines that contain `pattern` (case-insensitive with `ignore_case`) in every dossier, or in `dossier`, as
    /// `<path>:<line>:<hash>|<text>`.
    #[must_use]
    pub fn search(
        &self,
        pattern: &str,
        dossier: Option<&str>,
        ignore_case: bool,
        limit: usize,
    ) -> Vec<String> {
        let needle = if ignore_case {
            pattern.to_lowercase()
        } else {
            pattern.to_string()
        };
        let mut files = Vec::new();
        let dossiers: Vec<String> = match dossier {
            Some(d) => vec![d.to_string()],
            None => self.list().into_iter().map(|d| d.name).collect(),
        };
        for d in dossiers {
            collect_files(&self.root.join(&d), &mut files);
        }
        files.sort();
        let mut out = Vec::new();
        for file in files {
            let Ok(text) = std::fs::read_to_string(&file) else {
                continue;
            };
            let rel = file
                .strip_prefix(&self.root)
                .unwrap_or(&file)
                .display()
                .to_string();
            for (i, line) in lines(&text).iter().enumerate() {
                let hay = if ignore_case {
                    line.to_lowercase()
                } else {
                    line.clone()
                };
                if hay.contains(&needle) {
                    out.push(format!("{rel}:{}", hashline(i + 1, line)));
                    if out.len() >= limit {
                        return out;
                    }
                }
            }
        }
        out
    }

    /// Write a new file (or replace one with `overwrite`) in the session's active dossier.
    ///
    /// # Errors
    /// The path is not in the session's active dossier, exists without `overwrite`, or cannot be written.
    pub fn write(
        &self,
        session: &str,
        rel: &str,
        content: &str,
        overwrite: bool,
    ) -> Result<PathBuf> {
        let full = self.guard_write(session, rel)?;
        ensure!(
            overwrite || !full.exists(),
            "{rel} exists; use edit, or pass overwrite"
        );
        atomic_write(&full, &with_newline(content))?;
        self.log(session, "write", rel)?;
        Ok(full)
    }

    /// # Errors
    /// The path is not in the session's active dossier, does not exist, or cannot be written.
    pub fn append(&self, session: &str, rel: &str, content: &str) -> Result<PathBuf> {
        let full = self.guard_write(session, rel)?;
        let old = std::fs::read_to_string(&full)
            .with_context(|| format!("{rel} does not exist; use write"))?;
        let sep = if old.is_empty() || old.ends_with('\n') {
            ""
        } else {
            "\n"
        };
        atomic_write(&full, &format!("{old}{sep}{}", with_newline(content)))?;
        self.log(session, "append", rel)?;
        Ok(full)
    }

    /// Change anchored lines. Both anchors must match the current file, so an edit from an outdated read is refused.
    /// Returns the changed lines as hashlines.
    ///
    /// # Errors
    /// The path is not in the session's active dossier, an anchor does not match, or the file cannot be written.
    pub fn edit(
        &self,
        session: &str,
        rel: &str,
        mode: EditMode,
        start: &str,
        end: Option<&str>,
        content: &str,
    ) -> Result<Vec<String>> {
        let full = self.guard_write(session, rel)?;
        let mut file =
            lines(&std::fs::read_to_string(&full).with_context(|| format!("reading {rel}"))?);
        let check = |anchor: &str| -> Result<usize> {
            let (line, hash) = parse_anchor(anchor)?;
            ensure!(
                line <= file.len() && line_hash(&file[line - 1]) == hash,
                "anchor {line}:{hash} no longer matches; read the file again"
            );
            Ok(line)
        };
        let start = check(start)?;
        let end = match end {
            Some(end) if mode != EditMode::InsertAfter => check(end)?,
            _ => start,
        };
        ensure!(end >= start, "the end anchor comes before the start anchor");
        let new = lines(content);
        let (from, changed) = match mode {
            EditMode::Replace => {
                file.splice(start - 1..end, new.clone());
                (start, new.len())
            }
            EditMode::InsertAfter => {
                file.splice(start..start, new.clone());
                (start + 1, new.len())
            }
            EditMode::Delete => {
                file.drain(start - 1..end);
                (start, 0)
            }
        };
        let text: String = file.iter().flat_map(|l| [l.as_str(), "\n"]).collect();
        atomic_write(&full, &text)?;
        self.log(session, "edit", rel)?;
        Ok(file
            .iter()
            .enumerate()
            .skip(from - 1)
            .take(changed)
            .map(|(i, l)| hashline(i + 1, l))
            .collect())
    }

    /// Create the next numbered artifact `<CLASS>[/sub]/NN-<slug>.md` in the session's active dossier. Returns its
    /// path relative to the workspace root.
    ///
    /// # Errors
    /// The class or slug is not valid, the session has no active dossier, or the file cannot be created.
    pub fn add(&self, session: &str, class: &str, slug: &str, content: &str) -> Result<String> {
        let dossier = self.active(session)?.context(
            "this session has no active dossier; run `legionctl dossier use <name>` first",
        )?;
        let top = class.split('/').next().unwrap_or_default();
        ensure!(
            self.config.classes.contains_key(top),
            "unknown class {top}; classes: {}",
            self.config
                .classes
                .keys()
                .cloned()
                .collect::<Vec<_>>()
                .join(", ")
        );
        ensure!(
            valid_name(slug),
            "slug must be a-z, 0-9 and '-', starting with a letter or digit"
        );
        let folder_rel = format!("{dossier}/{class}");
        let folder = self.guard_write(session, &folder_rel)?;
        std::fs::create_dir_all(&folder)?;
        let mut next = std::fs::read_dir(&folder)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|e| {
                let name = e.file_name().to_string_lossy().into_owned();
                let (digits, _) = name.split_once('-')?;
                digits.parse::<u32>().ok()
            })
            .max()
            .unwrap_or(0)
            + 1;
        // another agent may take the same number at the same time: create exclusively and move on
        loop {
            let name = format!("{next:02}-{slug}.md");
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(folder.join(&name))
            {
                Ok(mut file) => {
                    file.write_all(with_newline(content).as_bytes())?;
                    let rel = format!("{folder_rel}/{name}");
                    self.log(session, "add", &rel)?;
                    return Ok(rel);
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => next += 1,
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Ledger findings: a missing `plan.md` or a missing section.
    #[must_use]
    pub fn check(&self, dossier: &str) -> Vec<String> {
        let Ok(plan) = std::fs::read_to_string(self.root.join(dossier).join("plan.md")) else {
            return vec![format!("{dossier}/plan.md is missing")];
        };
        let present: Vec<&str> = plan
            .lines()
            .filter_map(|l| l.trim().strip_prefix("## "))
            .collect();
        self.config
            .plan_sections
            .iter()
            .filter(|s| !present.contains(&s.as_str()))
            .map(|s| format!("plan.md has no '## {s}' section"))
            .collect()
    }
}

fn collect_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if hidden || kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            collect_files(&path, out);
        } else if entry.metadata().is_ok_and(|m| m.len() <= 2 << 20) {
            out.push(path);
        }
    }
}

fn with_newline(text: &str) -> String {
    if text.is_empty() || text.ends_with('\n') {
        text.to_string()
    } else {
        format!("{text}\n")
    }
}

/// Write through a temporary file in the same folder and rename, so a reader never sees half a file.
fn atomic_write(path: &Path, text: &str) -> Result<()> {
    let dir = path.parent().context("no parent folder")?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchors_match_safecoder_and_refuse_outdated_edits() {
        // the hashes safecoder (and Python's blake2b(digest_size=2)) give, so anchors move between the tools
        assert_eq!(line_hash(""), "b1fe");
        assert_eq!(line_hash("hello"), "accd");
        assert_eq!(line_hash("alpha"), "0cf8");

        let dir = std::env::temp_dir().join(format!("legionctl-dossier-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let config: crate::config::Config = crate::config::resolve(
            &crate::config::Dirs {
                system: dir.join("etc"),
                user: dir.join("user"),
            },
            None,
        )
        .unwrap()
        .config;
        let mut dossier = config.dossier;
        dossier.root = dir.join("ws").display().to_string();
        let ws = Workspace::open(&dossier).unwrap();
        ws.bind("s1", "test", "port", true, false, None).unwrap();
        ws.write("s1", "port/MEMORY/01-a.md", "alpha\nbeta\ngamma", false)
            .unwrap();
        let read = ws.read("port/MEMORY/01-a.md", 1, None).unwrap();
        assert_eq!(read[0], "1:0cf8|alpha");
        let beta = read[1].split('|').next().unwrap().to_string();
        assert_eq!(
            ws.edit(
                "s1",
                "port/MEMORY/01-a.md",
                EditMode::Replace,
                &beta,
                None,
                "BETA\nbeta2"
            )
            .unwrap()
            .len(),
            2
        );
        let err = ws
            .edit(
                "s1",
                "port/MEMORY/01-a.md",
                EditMode::Delete,
                &beta,
                None,
                "",
            )
            .unwrap_err();
        assert!(err.to_string().contains("no longer matches"), "{err}");

        // a second session cannot write or bind the held dossier; numbers continue per folder
        assert!(ws.write("s2", "port/MEMORY/02-b.md", "x", false).is_err());
        assert!(ws.bind("s2", "test", "port", false, false, None).is_err());
        assert_eq!(
            ws.add("s1", "MEMORY", "b", "# B").unwrap(),
            "port/MEMORY/02-b.md"
        );
        assert!(
            ws.check("port").is_empty(),
            "a new dossier has every ledger section"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
