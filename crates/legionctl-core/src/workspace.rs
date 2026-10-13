//! Workspaces (note 18): the operating directory of a dossier in one repository. A workspace is a `git worktree` at
//! `<workspace.root>/<repo>/<dossier>` on the dossier's branch `legionctl/<dossier>`, made from a recorded base ref,
//! with the dossier linked in as `.agentworkspace`. legionctl does the git work around the agent: open, new branch,
//! checkpoint, merge (rebase onto the base, then fast-forward the base) and close; `status` reports git state and
//! drift (a branch switched outside legionctl, a missing worktree, a base that moved).
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

/// The line that keeps the dossier link out of the product (decided 2026-10-12: a committed `.gitignore` line).
const IGNORE_LINE: &str = "/.agentworkspace";
const LINK: &str = ".agentworkspace";

fn git(dir: &Path, args: &[&str]) -> Result<String> {
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

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// One open workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Workspace {
    pub dossier: String,
    /// The repository's main checkout.
    pub repo: PathBuf,
    /// The operating directory: work here, not in the main checkout.
    pub path: PathBuf,
    /// The active branch: `legionctl/<dossier>` or `legionctl/<dossier>--<topic>`.
    pub branch: String,
    /// The ref the branch started from and merges into.
    pub base: String,
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
    /// The repository that contains `dir`, the dossier's open workspace there, and whether the main checkout is
    /// dirty; `None` when `dir` is not in a git repository.
    #[must_use]
    pub fn place(&self, dossier: &str, dir: &Path) -> Option<Place> {
        let repo = main_checkout(dir).ok()?;
        let (staged, unstaged, untracked, conflicted) = changes(&repo).ok()?;
        Some(Place {
            workspace: self.row(dossier, &repo).ok().flatten(),
            main_dirty: staged + unstaged + untracked + conflicted > 0,
            repo,
        })
    }

    fn row(&self, dossier: &str, repo: &Path) -> Result<Option<Workspace>> {
        Ok(self
            .db()
            .query_row(
                "SELECT path, branch, base FROM workspaces WHERE dossier = ?1 AND repo = ?2 AND closed IS NULL",
                params![dossier, repo.to_string_lossy()],
                |r| {
                    Ok(Workspace {
                        dossier: dossier.to_string(),
                        repo: repo.to_path_buf(),
                        path: PathBuf::from(r.get::<_, String>(0)?),
                        branch: r.get(1)?,
                        base: r.get(2)?,
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
            "SELECT dossier, repo, path, branch, base FROM workspaces
             WHERE closed IS NULL AND (?1 IS NULL OR dossier = ?1) ORDER BY dossier, repo",
        )?;
        let rows = stmt.query_map([dossier], |r| {
            Ok(Workspace {
                dossier: r.get(0)?,
                repo: PathBuf::from(r.get::<_, String>(1)?),
                path: PathBuf::from(r.get::<_, String>(2)?),
                branch: r.get(3)?,
                base: r.get(4)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    fn active_dossier(&self, session: &str) -> Result<String> {
        self.active(session)?
            .context("this session has no active dossier; run `legionctl dossier use <name>` first")
    }

    /// The workspace of this session's dossier in `repo`, or its only one when `repo` is not given.
    ///
    /// # Errors
    /// The session has no dossier, or the workspace is not open (or `repo` is needed to choose).
    pub fn workspace_of(&self, session: &str, repo: Option<&Path>) -> Result<Workspace> {
        let dossier = self.active_dossier(session)?;
        if let Some(dir) = repo {
            return self.row(&dossier, &main_checkout(dir)?)?.with_context(|| {
                format!(
                    "dossier {dossier} has no workspace for {}; open one",
                    dir.display()
                )
            });
        }
        let mut open = self.workspaces(Some(&dossier))?;
        ensure!(
            !open.is_empty(),
            "dossier {dossier} has no open workspace; open one first"
        );
        ensure!(
            open.len() == 1,
            "dossier {dossier} has several workspaces; name the repository"
        );
        Ok(open.remove(0))
    }

    /// Open (or reopen) the workspace of this session's dossier in the repository that contains `dir`: a worktree on
    /// `legionctl/<dossier>` from `base` (default: the main checkout's current branch), with the dossier linked in.
    ///
    /// # Errors
    /// The session has no dossier, `dir` is not in a git repository, or git refuses.
    pub fn open_workspace(
        &self,
        session: &str,
        dir: &Path,
        base: Option<&str>,
        root: &Path,
    ) -> Result<Workspace> {
        let dossier = self.active_dossier(session)?;
        let repo = main_checkout(dir)?;
        if let Some(open) = self.row(&dossier, &repo)?
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
        let path = root.join(name).join(&dossier);
        let path_str = path.to_str().context("the workspace path is not UTF-8")?;
        let branch = format!("legionctl/{dossier}");
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
        std::os::unix::fs::symlink(self.root.join(&dossier), path.join(LINK)).or_else(|e| {
            if e.kind() == std::io::ErrorKind::AlreadyExists {
                Ok(())
            } else {
                Err(e)
            }
        })?;
        let ignore = path.join(".gitignore");
        let text = std::fs::read_to_string(&ignore).unwrap_or_default();
        if !text.lines().any(|l| l.trim() == IGNORE_LINE) {
            let sep = if text.is_empty() || text.ends_with('\n') {
                ""
            } else {
                "\n"
            };
            std::fs::write(&ignore, format!("{text}{sep}{IGNORE_LINE}\n"))?;
            git(&path, &["add", ".gitignore"])?;
            git(
                &path,
                &[
                    "commit",
                    "--quiet",
                    "-m",
                    "legionctl: ignore the dossier link (/.agentworkspace)",
                ],
            )?;
        }
        self.db().execute(
            "UPDATE workspaces SET closed = ?1 WHERE dossier = ?2 AND repo = ?3 AND closed IS NULL",
            params![now(), dossier, repo.to_string_lossy()],
        )?;
        self.db().execute(
            "INSERT INTO workspaces (dossier, repo, path, branch, base, opened) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![dossier, repo.to_string_lossy(), path_str, branch, base, now()],
        )?;
        Ok(Workspace {
            dossier,
            repo,
            path,
            branch,
            base,
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
        };
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
                "the branch is {head}, recorded {}: switched outside legionctl (use workspace branch)",
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

    /// Stage everything in the workspace and commit it.
    ///
    /// # Errors
    /// The workspace is not open, there are conflicts, or git refuses.
    pub fn checkpoint(&self, ws: &Workspace, message: &str) -> Result<String> {
        ensure!(!message.trim().is_empty(), "a checkpoint needs a message");
        git(&ws.path, &["add", "--all"])?;
        if git(&ws.path, &["diff", "--cached", "--quiet"]).is_ok() {
            return Ok("nothing to checkpoint".into());
        }
        git(&ws.path, &["commit", "--quiet", "-m", message])?;
        git(&ws.path, &["log", "-1", "--format=%h %s"])
    }

    /// Start `legionctl/<dossier>--<topic>` from the current branch; it becomes the active branch.
    ///
    /// # Errors
    /// The topic is not valid, or git refuses.
    pub fn new_branch(&self, ws: &Workspace, topic: &str) -> Result<String> {
        ensure!(
            crate::dossier::valid_name(topic),
            "topic must be a-z, 0-9 and '-', starting with a letter or digit"
        );
        // not legionctl/<dossier>/<topic>: git cannot nest a ref under the existing branch legionctl/<dossier>
        let branch = format!("legionctl/{}--{topic}", ws.dossier);
        git(&ws.path, &["switch", "--quiet", "-c", &branch])?;
        self.db().execute(
            "UPDATE workspaces SET branch = ?1 WHERE dossier = ?2 AND repo = ?3 AND closed IS NULL",
            params![branch, ws.dossier, ws.repo.to_string_lossy()],
        )?;
        Ok(branch)
    }

    /// Rebase the branch onto the base, then fast-forward the base in the main checkout. Conflicts stop the rebase in
    /// the workspace, for the agent to resolve.
    ///
    /// # Errors
    /// The workspace has uncommitted changes, the rebase stops on conflicts, or the main checkout is not clean or not
    /// on the base branch.
    pub fn merge(&self, ws: &Workspace) -> Result<Vec<String>> {
        let (staged, unstaged, _, conflicted) = changes(&ws.path)?;
        ensure!(
            staged + unstaged + conflicted == 0,
            "the workspace has uncommitted changes; checkpoint first"
        );
        if git(&ws.path, &["rebase", "--quiet", &ws.base]).is_err() {
            bail!(
                "the rebase onto {} stopped on conflicts; resolve them in {}, run `git rebase --continue`, then merge again",
                ws.base,
                ws.path.display()
            );
        }
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
        git(&ws.repo, &["merge", "--quiet", "--ff-only", &ws.branch])?;
        Ok(vec![
            format!("rebased {} onto {}", ws.branch, ws.base),
            format!(
                "{} is now {}",
                ws.base,
                git(&ws.repo, &["log", "-1", "--format=%h %s"])?
            ),
        ])
    }

    /// Remove the worktree; the branch stays until it is merged, and the dossier stays.
    ///
    /// # Errors
    /// The workspace has uncommitted changes without `force`, or git refuses.
    pub fn close(&self, ws: &Workspace, force: bool) -> Result<String> {
        if ws.path.join(".git").exists() {
            let (staged, unstaged, untracked, conflicted) = changes(&ws.path)?;
            ensure!(
                force || staged + unstaged + untracked + conflicted == 0,
                "the workspace has uncommitted changes; checkpoint them, or pass force to discard them"
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
            "UPDATE workspaces SET closed = ?1 WHERE dossier = ?2 AND repo = ?3 AND closed IS NULL",
            params![now(), ws.dossier, ws.repo.to_string_lossy()],
        )?;
        Ok(format!(
            "closed {} (branch {} kept)",
            ws.path.display(),
            ws.branch
        ))
    }
}
