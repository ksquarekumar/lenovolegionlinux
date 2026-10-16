//! Workspaces (note 18): the operating directory of a dossier in one repository. A workspace is a `git worktree` at
//! `<workspace.root>/<repo>/<dossier>` on the dossier's branch `legionctl/<dossier>`, made from a recorded base ref,
//! with the dossier home linked in as `agentWorkspaces` (design 23, E1a). A topic workspace (for agents of one dossier that work in
//! parallel) is a separate worktree at `<workspace.root>/<repo>/<dossier>--<topic>` on `legionctl/<dossier>--<topic>`.
//! legionctl does the git work around the agent: open, checkpoint, merge (rebase onto the base, then fast-forward the
//! base) and close; `status` reports git state and drift (a branch switched outside legionctl, a missing worktree, a
//! base that moved).
//!
//! The disks here are ext4, which cannot reflink, so the copy is a worktree: it shares the repo's object store.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::dossier::Dossiers;

pub(crate) fn git(dir: &Path, args: &[&str]) -> Result<String> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .context("running git")?;
    ensure!(
        out.status.success(),
        "git {}: {}",
        args.join(" "),
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// A gate's refusal with what it refused first; the error keeps its code.
fn refused(what: &str, e: &anyhow::Error) -> anyhow::Error {
    crate::error::coded(crate::error::code_of(e), format!("{what}: {e:#}"))
}

/// The content of a workspace as one hash: for a repository its HEAD, `git status`, the diff against HEAD and the
/// size and change time of each untracked file; for a plain folder its changes. Equal hashes mean nothing changed.
///
/// # Errors
/// git fails in the workspace.
pub fn work_state(dossiers: &Dossiers, ws: &Workspace) -> Result<String> {
    use blake2::{Blake2b, Digest, digest::consts::U16};
    if ws.path.join(".git").exists() {
        return git_state(&ws.path);
    }
    Ok(format!(
        "{:x}",
        Blake2b::<U16>::digest(dossiers.workspace_status(ws)?.files.join("\n"))
    ))
}

/// The content of the git working copy that holds `dir` as one hash: HEAD, `git status`, the diff against HEAD and
/// the size and change time of each untracked file. Equal hashes mean nothing changed.
///
/// # Errors
/// git fails in `dir`.
pub fn git_state(dir: &Path) -> Result<String> {
    use blake2::{Blake2b, Digest, digest::consts::U16};
    let mut hash = Blake2b::<U16>::new();
    for args in [
        &["rev-parse", "HEAD"][..],
        &["status", "--porcelain=v1", "--untracked-files=all"],
        &["diff", "HEAD"],
    ] {
        hash.update(git(dir, args)?);
    }
    let top = git(dir, &["rev-parse", "--show-toplevel"])?;
    for file in git(
        dir,
        &["ls-files", "--others", "--exclude-standard", "--full-name"],
    )?
    .lines()
    {
        if let Ok(meta) = Path::new(&top).join(file).metadata() {
            hash.update(format!("{file} {} {:?}", meta.len(), meta.modified().ok()));
        }
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// One open workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Workspace {
    pub dossier: String,
    /// The repository's main checkout; for a plain folder, the folder.
    pub repo: PathBuf,
    /// The operating directory: work here, not in the main checkout.
    pub path: PathBuf,
    /// The branch: `legionctl/<dossier>`, or `legionctl/<dossier>--<topic>` for a topic workspace.
    pub branch: String,
    /// The ref the branch started from and merges into.
    pub base: String,
    /// The topic of a topic workspace; empty for the dossier's own.
    pub topic: String,
}

/// Git state of a workspace and what drifted from the record.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceStatus {
    #[serde(flatten)]
    pub workspace: Workspace,
    /// Commits on the branch that the base does not have, and the other way round.
    pub ahead: u32,
    pub behind: u32,
    pub staged: u32,
    pub unstaged: u32,
    pub untracked: u32,
    pub conflicted: u32,
    /// What differs from the record: a branch switched outside legionctl, a missing worktree.
    pub drift: Vec<String>,
    /// A plain folder's changes, one per line (A, M, D, R and the path); empty for a repository.
    pub files: Vec<String>,
}

impl Workspace {
    /// A plain folder's workspace (an overlay layer, no branch).
    #[must_use]
    pub fn is_folder(&self) -> bool {
        self.branch.is_empty()
    }
}

/// The repository's `.git` folder that its worktrees share (objects, refs): what a commit in a worktree writes.
///
/// # Errors
/// `dir` is not in a git repository.
pub fn git_common_dir(dir: &Path) -> Result<PathBuf> {
    Ok(PathBuf::from(git(
        dir,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?))
}

/// The repository's main checkout for `dir`, also when `dir` is inside one of its worktrees.
pub(crate) fn main_checkout(dir: &Path) -> Result<PathBuf> {
    git_common_dir(dir)?
        .parent()
        .map(Path::to_path_buf)
        .context("the repository has no main checkout")
}

/// Count the entries of `git status --porcelain` by kind: (staged, unstaged, untracked, conflicted).
fn changes(path: &Path) -> Result<(u32, u32, u32, u32)> {
    let mut counts = (0, 0, 0, 0);
    for line in git(path, &["status", "--porcelain=v1"])?.lines() {
        let code: Vec<char> = line.chars().take(2).collect();
        let [x, y] = code[..] else { continue };
        if x == '?' {
            counts.2 += 1;
        } else if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
            counts.3 += 1;
        } else {
            counts.0 += u32::from(x != ' ');
            counts.1 += u32::from(y != ' ');
        }
    }
    Ok(counts)
}

/// What the hooks need to nudge the agent about the workspace of `dossier` where it works (`dir`).
#[derive(Debug, Clone)]
pub struct Place {
    pub repo: PathBuf,
    pub workspace: Option<Workspace>,
    /// The main checkout has uncommitted changes (someone worked there instead of in a workspace).
    pub main_dirty: bool,
}

impl Dossiers {
    /// The repository that contains `dir`, the dossier's open workspace there (the one that contains `dir`, else the
    /// dossier's own workspace), and whether the main checkout is dirty; `None` when `dir` is not in a git repository.
    #[must_use]
    pub fn place(&self, dossier: &str, dir: &Path) -> Option<Place> {
        let repo = main_checkout(dir).ok()?;
        let (staged, unstaged, untracked, conflicted) = changes(&repo).ok()?;
        let dir = std::fs::canonicalize(dir).ok()?;
        let containing = self
            .workspaces(Some(dossier))
            .ok()?
            .into_iter()
            .find(|ws| std::fs::canonicalize(&ws.path).is_ok_and(|path| dir.starts_with(path)));
        Some(Place {
            workspace: containing.or_else(|| self.row(dossier, &repo, "").ok().flatten()),
            main_dirty: staged + unstaged + untracked + conflicted > 0,
            repo,
        })
    }

    fn row(&self, dossier: &str, repo: &Path, topic: &str) -> Result<Option<Workspace>> {
        Ok(self
            .db()
            .query_row(
                "SELECT path, branch, base FROM workspaces
                 WHERE dossier = ?1 AND repo = ?2 AND topic = ?3 AND closed IS NULL",
                params![dossier, repo.to_string_lossy(), topic],
                |r| {
                    Ok(Workspace {
                        dossier: dossier.to_string(),
                        repo: repo.to_path_buf(),
                        path: PathBuf::from(r.get::<_, String>(0)?),
                        branch: r.get(1)?,
                        base: r.get(2)?,
                        topic: topic.to_string(),
                    })
                },
            )
            .optional()?)
    }

    /// Open workspaces, of one dossier or of all.
    ///
    /// # Errors
    /// The database read fails.
    pub fn workspaces(&self, dossier: Option<&str>) -> Result<Vec<Workspace>> {
        let mut stmt = self.db().prepare(
            "SELECT dossier, repo, path, branch, base, topic FROM workspaces
             WHERE closed IS NULL AND (?1 IS NULL OR dossier = ?1) ORDER BY dossier, repo",
        )?;
        let rows = stmt.query_map([dossier], |r| {
            Ok(Workspace {
                dossier: r.get(0)?,
                repo: PathBuf::from(r.get::<_, String>(1)?),
                path: PathBuf::from(r.get::<_, String>(2)?),
                branch: r.get(3)?,
                base: r.get(4)?,
                topic: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn active_dossier(&self, session: &str) -> Result<String> {
        self.active(session)?
            .context("this session has no active dossier; run `legionctl dossier use <name>` first")
    }

    /// The workspace of this session's dossier for `dir`: with a topic, that topic's workspace in the repository of
    /// `dir`; else the workspace that contains `dir`, else the dossier's own workspace in its repository (a plain
    /// folder: by its own path).
    ///
    /// # Errors
    /// The session has no dossier, or the workspace is not open.
    pub fn workspace_of(
        &self,
        session: &str,
        dir: &Path,
        topic: Option<&str>,
    ) -> Result<Workspace> {
        let dossier = self.active_dossier(session)?;
        let found = if let Some(topic) = topic {
            self.row(&dossier, &main_checkout(dir)?, topic)?
        } else if let Some(place) = self.place(&dossier, dir) {
            place.workspace
        } else {
            self.row(&dossier, &std::fs::canonicalize(dir)?, "")?
        };
        found.with_context(|| {
            let topic = topic.map(|t| format!(" --topic {t}")).unwrap_or_default();
            format!(
                "dossier {dossier} has no workspace{topic} for {}; run `legionctl workspace open{topic}` there",
                dir.display()
            )
        })
    }

    /// Open (or reopen) the workspace of this session's dossier in the repository that contains `dir`: a worktree on
    /// `legionctl/<dossier>` from `base` (default: the main checkout's current branch), with the dossier linked in.
    /// With a topic, a separate worktree on `legionctl/<dossier>--<topic>` (created from `base` the first time,
    /// reused after), so that agents of one dossier work in parallel.
    ///
    /// # Errors
    /// The session has no dossier, the topic is not valid, `dir` is not in a git repository (for a topic), or git
    /// refuses.
    pub fn open_workspace(
        &self,
        session: &str,
        dir: &Path,
        base: Option<&str>,
        topic: Option<&str>,
        root: &Path,
    ) -> Result<Workspace> {
        let dossier = self.active_dossier(session)?;
        ensure!(
            topic.is_none_or(crate::dossier::valid_name),
            "topic must be a-z, 0-9 and '-', starting with a letter or digit"
        );
        let Ok(repo) = main_checkout(dir) else {
            ensure!(topic.is_none(), "a plain folder's workspace has no topics");
            return self.open_folder(dossier, dir, root);
        };
        let topic = topic.unwrap_or_default();
        if let Some(open) = self.row(&dossier, &repo, topic)?
            && open.path.join(".git").exists()
        {
            return Ok(open);
        }
        let base = match base {
            Some(b) => {
                ensure!(!b.starts_with('-'), "base must be a git ref, not an option");
                b.to_string()
            }
            None => match git(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
                Ok(branch) => branch,
                Err(_) => git(&repo, &["rev-parse", "HEAD"])?,
            },
        };
        let name = repo
            .file_name()
            .context("the repository folder has no name")?;
        // not legionctl/<dossier>/<topic>: git cannot nest a ref under the existing branch legionctl/<dossier>
        let own = if topic.is_empty() {
            dossier.clone()
        } else {
            format!("{dossier}--{topic}")
        };
        let path = root.join(name).join(&own);
        let path_str = path.to_str().context("the workspace path is not UTF-8")?;
        let branch = format!("legionctl/{own}");
        std::fs::create_dir_all(path.parent().context("no parent folder")?)?;
        let exists = git(
            &repo,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
        )
        .is_ok();
        if exists {
            git(&repo, &["worktree", "add", path_str, &branch])?;
        } else {
            git(&repo, &["worktree", "add", "-b", &branch, path_str, &base])?;
        }
        // each worktree gets its own link: a session works there (E1a rule 5)
        crate::home::link(self, &path)?;
        self.db().execute(
            "UPDATE workspaces SET closed = ?1 WHERE dossier = ?2 AND repo = ?3 AND topic = ?4 AND closed IS NULL",
            params![now(), dossier, repo.to_string_lossy(), topic],
        )?;
        self.db().execute(
            "INSERT INTO workspaces (dossier, repo, path, branch, base, opened, topic)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                dossier,
                repo.to_string_lossy(),
                path_str,
                branch,
                base,
                now(),
                topic
            ],
        )?;
        Ok(Workspace {
            dossier,
            repo,
            path,
            branch,
            base,
            topic: topic.to_string(),
        })
    }

    /// A plain folder's workspace: an overlay layer at `<root>/<folder name>/<dossier>` over `dir`.
    fn open_folder(&self, dossier: String, dir: &Path, root: &Path) -> Result<Workspace> {
        let source = std::fs::canonicalize(dir)?;
        if let Some(open) = self.row(&dossier, &source, "")? {
            return Ok(open);
        }
        let name = source.file_name().context("the folder has no name")?;
        let path = root.join(name).join(&dossier);
        for dir in [crate::folder::upper(&path), crate::folder::work(&path)] {
            std::fs::create_dir_all(dir)?;
        }
        // the folder itself gets the link, so the agent sees it through the overlay
        crate::home::link(self, &source)?;
        self.db().execute(
            "INSERT INTO workspaces (dossier, repo, path, branch, base, opened) VALUES (?1, ?2, ?3, '', '', ?4)",
            params![dossier, source.to_string_lossy(), path.to_string_lossy(), now()],
        )?;
        Ok(Workspace {
            dossier,
            repo: source,
            path,
            branch: String::new(),
            base: String::new(),
            topic: String::new(),
        })
    }

    /// Git state and drift of a workspace.
    ///
    /// # Errors
    /// Git cannot read the worktree.
    pub fn workspace_status(&self, ws: &Workspace) -> Result<WorkspaceStatus> {
        let mut status = WorkspaceStatus {
            workspace: ws.clone(),
            ahead: 0,
            behind: 0,
            staged: 0,
            unstaged: 0,
            untracked: 0,
            conflicted: 0,
            drift: Vec::new(),
            files: Vec::new(),
        };
        if ws.is_folder() {
            status.files = crate::folder::changes(&ws.path, &ws.repo)?
                .iter()
                .map(ToString::to_string)
                .collect();
            status.unstaged = u32::try_from(status.files.len()).unwrap_or(u32::MAX);
            return Ok(status);
        }
        if !ws.path.join(".git").exists() {
            status.drift.push(format!(
                "the worktree {} is missing; open the workspace again",
                ws.path.display()
            ));
            return Ok(status);
        }
        let head = git(&ws.path, &["symbolic-ref", "--quiet", "--short", "HEAD"])
            .unwrap_or_else(|_| "(detached)".into());
        if head != ws.branch {
            status.drift.push(format!(
                "the branch is {head}, recorded {}: switched outside legionctl (switch it back)",
                ws.branch
            ));
        }
        let counts = git(
            &ws.path,
            &[
                "rev-list",
                "--left-right",
                "--count",
                &format!("HEAD...{}", ws.base),
            ],
        )?;
        let mut parts = counts
            .split_whitespace()
            .map(|n| n.parse::<u32>().unwrap_or(0));
        status.ahead = parts.next().unwrap_or(0);
        status.behind = parts.next().unwrap_or(0);
        (
            status.staged,
            status.unstaged,
            status.untracked,
            status.conflicted,
        ) = changes(&ws.path)?;
        Ok(status)
    }

    /// Stage everything in the workspace and commit it, when the test gate lets the new and changed tests through and
    /// each `gate: checkpoint` check has a passing run at the tree (one without runs now, design 23 E2 rule 4). For a
    /// session (`jail`), the checks and the commit (with the repository's commit hooks) run in its sandbox (O-1).
    ///
    /// # Errors
    /// The workspace is not open, there are conflicts, the test gate or a check refuses (nothing is committed), or git
    /// refuses.
    pub fn checkpoint(
        &self,
        ws: &Workspace,
        message: &str,
        (session, jail): (&str, Option<&crate::session::Jail>),
    ) -> Result<Vec<String>> {
        ensure!(!message.trim().is_empty(), "a checkpoint needs a message");
        if ws.is_folder() {
            return Ok(vec![crate::folder::checkpoint(
                &ws.path, &ws.repo, message,
            )?]);
        }
        git(&ws.path, &["add", "--all"])?;
        if git(&ws.path, &["diff", "--cached", "--quiet"]).is_ok() {
            return Ok(vec!["nothing to checkpoint".into()]);
        }
        self.test_gate(&ws.path)?;
        let mut lines = self
            .gate(
                &ws.path,
                crate::checks::Gate::Checkpoint,
                (Some(session), jail),
            )
            .map_err(|e| refused("checkpoint refused, nothing committed", &e))?;
        let commit = ["commit", "--quiet", "-m", message];
        match jail {
            Some(jail) => {
                let out = jail
                    .command(&ws.path, &[&["git"][..], &commit].concat())
                    .output()
                    .context("running the commit in the session's sandbox")?;
                ensure!(
                    out.status.success(),
                    "git commit: {}{}",
                    String::from_utf8_lossy(&out.stdout).trim(),
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            None => {
                git(&ws.path, &commit)?;
            }
        }
        lines.push(git(&ws.path, &["log", "-1", "--format=%h %s"])?);
        Ok(lines)
    }

    /// The workspace that the user (no agent session) names for a merge: of `dossier`, or of the one dossier with a
    /// workspace there; with a topic, that topic's workspace in the repository of `dir`, else the workspace that
    /// contains `dir`, else the dossier's own workspace in the repository `dir`.
    ///
    /// # Errors
    /// No such workspace, or more than one dossier has one (then `--dossier` chooses).
    pub fn workspace_for_user(
        &self,
        dossier: Option<&str>,
        dir: &Path,
        topic: Option<&str>,
    ) -> Result<Workspace> {
        let dir = std::fs::canonicalize(dir)?;
        let repo = main_checkout(&dir)?;
        let mut found: Vec<Workspace> = self
            .workspaces(dossier)?
            .into_iter()
            .filter(|ws| match topic {
                Some(t) => ws.repo == repo && ws.topic == t,
                None => dir.starts_with(&ws.path) || (ws.repo == dir && ws.topic.is_empty()),
            })
            .collect();
        let topic = topic.map(|t| format!(" --topic {t}")).unwrap_or_default();
        match found.len() {
            1 => Ok(found.remove(0)),
            0 => bail!(
                "no open workspace{topic} for {}{}",
                dir.display(),
                dossier
                    .map(|d| format!(" of dossier {d}"))
                    .unwrap_or_default()
            ),
            _ => bail!(
                "the dossiers {} each have a workspace{topic} here: choose one with --dossier",
                found
                    .iter()
                    .map(|w| w.dossier.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    /// Rebase the branch onto the base, then fast-forward the base in the main checkout to the commit that the gate
    /// tested. Conflicts stop the rebase in the workspace, for the agent to resolve. `session` is the agent's; `None`
    /// is the user (todo-147). A branch that changes `.legionctl/` sets its own gates: an agent cannot merge it, and the
    /// user's merge runs the same gate, then `confirm` asks the user (a desktop dialog) before the fast-forward.
    ///
    /// # Errors
    /// The workspace has uncommitted or untracked files (the gate would test files that the merge does not ship), the
    /// rebase stops on conflicts, an agent's branch changes `.legionctl/`, the user does not confirm, or the main
    /// checkout is not clean or not on the base branch.
    pub fn merge(
        &self,
        ws: &Workspace,
        (session, jail): (Option<&str>, Option<&crate::session::Jail>),
        confirm: impl FnOnce(&str) -> Result<bool>,
    ) -> Result<Vec<String>> {
        if ws.is_folder() {
            return crate::folder::merge(&ws.path, &ws.repo);
        }
        let (staged, unstaged, untracked, conflicted) = changes(&ws.path)?;
        ensure!(
            staged + unstaged + untracked + conflicted == 0,
            "the workspace has uncommitted or untracked files, which the merge does not ship but its gate would test; \
             checkpoint first"
        );
        if git(&ws.path, &["rebase", "--quiet", &ws.base]).is_err() {
            bail!(
                "the rebase onto {} stopped on conflicts; resolve them in {}, run `git rebase --continue`, then merge again",
                ws.base,
                ws.path.display()
            );
        }
        let own_gates = git(
            &ws.path,
            &["diff", "--name-only", &ws.base, "HEAD", "--", ".legionctl"],
        )?;
        if !own_gates.is_empty() && session.is_some() {
            let topic = if ws.topic.is_empty() {
                String::new()
            } else {
                format!(" --topic {}", ws.topic)
            };
            bail!(
                "the branch {} changes .legionctl/, which sets its own gates: an agent cannot merge it. The user \
                 merges it from a terminal outside an agent session: legionctl workspace merge --repo {}{topic} \
                 --dossier {} (the same gate, then a desktop dialog to confirm)",
                ws.branch,
                ws.repo.display(),
                ws.dossier
            );
        }
        let tested = git(&ws.path, &["rev-parse", "HEAD"])?;
        let mut lines = self
            .gate(&ws.path, crate::checks::Gate::Merge, (session, jail))
            .map_err(|e| {
                refused(
                    &format!(
                        "merge refused at the rebased head of {} (the branch stays rebased onto {})",
                        ws.branch, ws.base
                    ),
                    &e,
                )
            })?;
        let main_head =
            git(&ws.repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]).unwrap_or_default();
        ensure!(
            main_head == ws.base,
            "the main checkout {} is on '{main_head}', not the base {}; switch it first",
            ws.repo.display(),
            ws.base
        );
        let (staged, unstaged, _, conflicted) = changes(&ws.repo)?;
        ensure!(
            staged + unstaged + conflicted == 0,
            "the main checkout {} has uncommitted changes; the fast-forward would touch them",
            ws.repo.display()
        );
        if !own_gates.is_empty() {
            let what = format!(
                "merge {} into {} in {}: it changes {}, which sets the repository's own gates",
                ws.branch,
                ws.base,
                ws.repo.display(),
                own_gates.lines().collect::<Vec<_>>().join(", ")
            );
            ensure!(
                confirm(&what)?,
                "the user did not confirm the merge of {}; {} stays where it was (the branch stays rebased)",
                ws.branch,
                ws.base
            );
        }
        // the tested commit, not the branch: a checkpoint after the gate is not tested
        git(&ws.repo, &["merge", "--quiet", "--ff-only", &tested])?;
        lines.extend([
            format!("rebased {} onto {}", ws.branch, ws.base),
            format!(
                "{} is now {}",
                ws.base,
                git(&ws.repo, &["log", "-1", "--format=%h %s"])?
            ),
        ]);
        Ok(lines)
    }

    /// Remove the worktree; the branch stays until it is merged, and the dossier stays.
    ///
    /// # Errors
    /// The workspace has uncommitted changes without `force`, or git refuses.
    pub fn close(&self, ws: &Workspace, force: bool) -> Result<String> {
        if ws.is_folder() {
            crate::folder::close(&ws.path, &ws.repo, force)?;
        } else if ws.path.join(".git").exists() {
            let (staged, unstaged, untracked, conflicted) = changes(&ws.path)?;
            ensure!(
                force || staged + unstaged + untracked + conflicted == 0,
                "the workspace has uncommitted changes; run `legionctl workspace checkpoint <message>`, or add --force to discard them"
            );
            let path = ws
                .path
                .to_str()
                .context("the workspace path is not UTF-8")?;
            let mut args = vec!["worktree", "remove", path];
            if force {
                args.push("--force");
            }
            git(&ws.repo, &args)?;
        }
        self.db().execute(
            "UPDATE workspaces SET closed = ?1 WHERE path = ?2 AND closed IS NULL",
            params![now(), ws.path.to_string_lossy()],
        )?;
        if ws.is_folder() {
            return Ok(format!(
                "closed {} (its changes dropped or merged)",
                ws.path.display()
            ));
        }
        let closed = format!("closed {}", ws.path.display());
        if ws.topic.is_empty() {
            return Ok(format!("{closed} (branch {} kept)", ws.branch));
        }
        // A topic branch goes when the base has all its commits; `branch -d` refuses it otherwise.
        let merged = git(
            &ws.repo,
            &["merge-base", "--is-ancestor", &ws.branch, &ws.base],
        )
        .is_ok();
        Ok(if merged {
            match git(&ws.repo, &["branch", "-d", &ws.branch]) {
                Ok(_) => format!("{closed} (branch {} merged and deleted)", ws.branch),
                Err(e) => format!("{closed} (branch {} kept: {e})", ws.branch),
            }
        } else {
            format!(
                "{closed} (branch {} kept: it is not merged into {})",
                ws.branch, ws.base
            )
        })
    }
}
