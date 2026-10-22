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
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use rusqlite::{OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::dossier::Dossiers;
use crate::git::text as git;

/// A gate's refusal with what it refused first; the error keeps its code.
fn refused(what: &str, e: &anyhow::Error) -> anyhow::Error {
    crate::error::coded(crate::error::code_of(e), format!("{what}: {e:#}"))
}

/// The first 12 characters of a commit id.
fn short(oid: &str) -> &str {
    &oid[..oid.len().min(12)]
}

/// The `git diff` options and range of a branch's change to `.legionctl/` (`base..tested`): the text of each file and
/// submodule commit as git stores them, whatever the branch sets (no diff driver, no textconv, no `-diff` attribute,
/// no submodule `ignore`).
fn gates_range<'a>(base: &'a str, tested: &'a str) -> [&'a str; 8] {
    [
        "--text",
        "--no-ext-diff",
        "--no-textconv",
        "--ignore-submodules=none",
        base,
        tested,
        "--",
        ".legionctl",
    ]
}

/// What the user approves in the merge of a branch that changes `.legionctl/`: the branch, the commits `base..tested`,
/// the changed files and their diff.
fn gates_diff(ws: &Workspace, (base, tested): (&str, &str), files: &str) -> Result<String> {
    let range = gates_range(base, tested);
    let diff = git(&ws.path, &[&["diff"][..], &range].concat())?;
    let head = format!(
        "merge {} into {} in {} ({}..{}): it changes {}, which sets the repository's own gates",
        ws.branch,
        ws.base,
        ws.repo.display(),
        short(base),
        short(tested),
        files.lines().collect::<Vec<_>>().join(", ")
    );
    // shortcut: a desktop notice is not a diff viewer; a long diff is named with its command, not shown
    Ok(if diff.len() > 16 * 1024 {
        format!(
            "{head}\nthe diff is {} bytes; read it in the main checkout with: git diff {}",
            diff.len(),
            range.join(" ")
        )
    } else {
        format!("{head}\n\n{diff}")
    })
}

/// The main checkout can take the fast-forward to `tested`: on the base branch, no uncommitted change that the
/// fast-forward touches, and the base still at `base`. Checked before the user's answer (no dialog for a merge that
/// cannot land) and again after it.
fn main_ready(ws: &Workspace, (base, tested): (&str, &str)) -> Result<()> {
    let main_head =
        git(&ws.repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]).unwrap_or_default();
    ensure!(
        main_head == ws.base,
        "the main checkout {} is on '{main_head}', not the base {}; switch it first",
        ws.repo.display(),
        ws.base
    );
    refuse_dirty_main(&ws.repo, (base, tested))?;
    let now = git(&ws.repo, &["rev-parse", &ws.base])?;
    ensure!(
        now == base,
        "{} moved from {} to {} while the merge ran; {} stays there: merge again",
        ws.base,
        short(base),
        short(&now),
        ws.base
    );
    Ok(())
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

/// The main checkout of the repository that contains `dir`; `None` only when git says `dir` is in no repository
/// (another git failure, such as dubious ownership or a broken `.git`, is an error).
///
/// # Errors
/// git does not run, or fails for another cause than a folder in no repository.
pub fn repo_of(dir: &Path) -> Result<Option<PathBuf>> {
    let out = crate::git::output(crate::git::command(dir).args([
        "rev-parse",
        "--path-format=absolute",
        "--git-common-dir",
    ]))?;
    if !out.status.success() {
        let why = String::from_utf8_lossy(&out.stderr);
        if why.contains("not a git repository") {
            return Ok(None);
        }
        bail!("git rev-parse in {}: {}", dir.display(), why.trim());
    }
    let common = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    common
        .parent()
        .map(|p| Some(p.to_path_buf()))
        .context("the repository has no main checkout")
}

/// The entries of `git status --porcelain` (`XY path`), also of each submodule, with each path from the top of `path`.
/// `workspace`: every file that is not committed, also one that the repository's own settings
/// (`status.showUntrackedFiles`, a submodule's `ignore`) hide, because a merge ships only commits; else (the main
/// checkout) a submodule's untracked files are left out, because a fast-forward cannot touch them (F4).
fn status(path: &Path, workspace: bool) -> Result<Vec<String>> {
    let args: &[&str] = if workspace {
        &[
            "status",
            "--porcelain=v1",
            "--ignore-submodules=none",
            "--untracked-files=all",
        ]
    } else {
        &["status", "--porcelain=v1", "--ignore-submodules=untracked"]
    };
    // git's own lines, not trimmed: the first one may start with a space (" M file", unstaged)
    let lines = |args: &[&str]| -> Result<String> {
        Ok(String::from_utf8_lossy(&crate::git::bytes(path, args)?).into_owned())
    };
    let mut entries: Vec<String> = lines(args)?.lines().map(String::from).collect();
    // `--ignore-submodules` does not reach a nested submodule, whose own `ignore = all` hides it: each one is read
    if path.join(".gitmodules").exists() {
        let each = format!("echo \"#$displaypath\"; git {}", args.join(" "));
        let mut submodule = String::new();
        for line in lines(&["submodule", "foreach", "--quiet", "--recursive", &each])?.lines() {
            match (line.strip_prefix('#'), line.get(..3), line.get(3..)) {
                (Some(at), ..) => submodule = format!("{at}/"),
                (None, Some(code), Some(file)) => entries.push(format!("{code}{submodule}{file}")),
                _ => {}
            }
        }
    }
    Ok(entries)
}

/// The entries of [`status`] by kind: (staged, unstaged, untracked, conflicted).
fn changes(path: &Path, workspace: bool) -> Result<(u32, u32, u32, u32)> {
    let mut counts = (0, 0, 0, 0);
    for line in status(path, workspace)? {
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

/// Refuse a merge while the main checkout has a tracked change in a path that the fast-forward from `base` to `tested`
/// changes (a file, or a submodule that the branch moves): name each file, and for Claude Code's
/// `.claude/settings.json` (the permissions that the user allows at a prompt go there) say where they can go instead.
/// Other changes stay through the fast-forward. Nothing is changed (F5, feedbacks/0001 item 5).
fn refuse_dirty_main(repo: &Path, (base, tested): (&str, &str)) -> Result<()> {
    // git keeps a change of a path that the fast-forward does not change (F5: the user's own .claude/settings.json);
    // a submodule's own `ignore = all` hides its move from the diff too
    let range = ["--name-only", "--no-renames", "--ignore-submodules=none"];
    let changed = git(repo, &[&["diff"][..], &range, &[base, tested]].concat())?;
    let touched = |file: &str| {
        changed
            .lines()
            .any(|path| file == path || file.starts_with(&format!("{path}/")))
    };
    let files: Vec<String> = status(repo, false)?
        .iter()
        .filter(|line| !line.starts_with("??"))
        .filter_map(|line| line.get(3..).map(String::from))
        .filter(|file| file.split(" -> ").any(touched))
        .collect();
    if files.is_empty() {
        return Ok(());
    }
    let settings = if files.iter().any(|f| f == ".claude/settings.json") {
        " Claude Code writes the permissions that you allow at a prompt into .claude/settings.json: commit them, or \
         keep them in .claude/settings.local.json (Claude Code's file for your own settings, not for git)."
    } else {
        ""
    };
    bail!(
        "the main checkout {} has uncommitted changes that the fast-forward would touch: {}. Commit them or move them away, then \
         merge again.{settings}",
        repo.display(),
        files.join(", ")
    )
}

/// The repository has a commit: a workspace needs one for its base.
fn committed(repo: &Path) -> bool {
    git(repo, &["rev-parse", "--verify", "--quiet", "HEAD"]).is_ok()
}

/// What the agent hears in a repository with no commit (F3, feedbacks/0001 item 3).
#[must_use]
pub fn no_commit_yet(repo: &Path) -> String {
    format!(
        "{} has no commit yet: make a first commit, then open a workspace",
        repo.display()
    )
}

/// What the hooks need to nudge the agent about the workspace of `dossier` where it works (`dir`).
#[derive(Debug, Clone)]
pub struct Place {
    pub repo: PathBuf,
    pub workspace: Option<Workspace>,
    /// The repository has a commit, so a workspace can be opened.
    pub committed: bool,
    /// The main checkout has uncommitted changes (someone worked there instead of in a workspace).
    pub main_dirty: bool,
}

impl Dossiers {
    /// The repository that contains `dir`, the dossier's open workspace there (the one that contains `dir`, else the
    /// dossier's own workspace), and whether the main checkout is dirty; `None` when `dir` is not in a git repository.
    ///
    /// # Errors
    /// git or the workspace rows cannot be read.
    pub fn place(&self, dossier: &str, dir: &Path) -> Result<Option<Place>> {
        let Some(repo) = repo_of(dir)? else {
            return Ok(None);
        };
        let (staged, unstaged, untracked, conflicted) = changes(&repo, false)?;
        let dir = std::fs::canonicalize(dir)?;
        // a workspace whose worktree is gone contains nothing
        let containing = self
            .workspaces(Some(dossier))?
            .into_iter()
            .find(|ws| std::fs::canonicalize(&ws.path).is_ok_and(|path| dir.starts_with(path)));
        let workspace = match containing {
            Some(ws) => Some(ws),
            None => self.row(dossier, &repo, "")?,
        };
        Ok(Some(Place {
            workspace,
            committed: committed(&repo),
            main_dirty: staged + unstaged + untracked + conflicted > 0,
            repo,
        }))
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

    /// The workspace of `dossier` (the caller's) for `dir`: with a topic, that topic's workspace in the repository of
    /// `dir`; else the workspace that contains `dir`, else the dossier's own workspace in its repository (a plain
    /// folder: by its own path).
    ///
    /// # Errors
    /// The workspace is not open.
    pub fn workspace_of(
        &self,
        dossier: &str,
        dir: &Path,
        topic: Option<&str>,
    ) -> Result<Workspace> {
        let dossier = dossier.to_owned();
        let found = if let Some(topic) = topic {
            self.row(&dossier, &main_checkout(dir)?, topic)?
        } else if let Some(other) = self.innermost(dir)?
            && other.dossier != dossier
        {
            // never the caller's own workspace for a folder in another dossier's (todo-172)
            bail!(
                "{} is in the workspace {} of dossier {}, not of dossier {dossier}: a session of \
                 dossier {} must do this",
                dir.display(),
                other.path.display(),
                other.dossier,
                other.dossier
            );
        } else if let Some(place) = self.place(&dossier, dir)? {
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

    /// The innermost open workspace, of any dossier, that contains `dir`.
    fn innermost(&self, dir: &Path) -> Result<Option<Workspace>> {
        let dir = std::fs::canonicalize(dir)?;
        Ok(self
            .workspaces(None)?
            .into_iter()
            .filter_map(|ws| {
                let path = std::fs::canonicalize(&ws.path).ok()?;
                dir.starts_with(&path)
                    .then(|| (path.components().count(), ws))
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, ws)| ws))
    }

    /// Open (or reopen) the workspace of `dossier` in the repository that contains `dir`: a worktree on
    /// `legionctl/<dossier>` from `base` (default: the main checkout's current branch), with the dossier linked in.
    /// With a topic, a separate worktree on `legionctl/<dossier>--<topic>` (created from `base` the first time,
    /// reused after), so that agents of one dossier work in parallel.
    ///
    /// # Errors
    /// The topic is not valid, `dir` is not in a git repository (for a topic), or git refuses.
    pub fn open_workspace(
        &self,
        dossier: String,
        dir: &Path,
        base: Option<&str>,
        topic: Option<&str>,
        root: &Path,
    ) -> Result<Workspace> {
        ensure!(
            topic.is_none_or(crate::dossier::valid_name),
            "topic must be a-z, 0-9 and '-', starting with a letter or digit"
        );
        let Some(repo) = repo_of(dir)? else {
            ensure!(topic.is_none(), "a plain folder's workspace has no topics");
            return self.open_folder(dossier, dir, root);
        };
        let topic = topic.unwrap_or_default();
        if let Some(open) = self.row(&dossier, &repo, topic)?
            && open.path.join(".git").exists()
        {
            return Ok(open);
        }
        let base = if let Some(b) = base {
            ensure!(!b.starts_with('-'), "base must be a git ref, not an option");
            ensure!(
                git(
                    &repo,
                    &[
                        "rev-parse",
                        "--verify",
                        "--quiet",
                        &format!("{b}^{{commit}}")
                    ]
                )
                .is_ok(),
                "the base {b} names no commit in {}",
                repo.display()
            );
            b.to_string()
        } else {
            // the current branch, which needs a commit (an explicit --base may name another branch)
            ensure!(committed(&repo), no_commit_yet(&repo));
            match git(&repo, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
                Ok(branch) => branch,
                Err(_) => git(&repo, &["rev-parse", "HEAD"])?,
            }
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
        ) = changes(&ws.path, true)?;
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
        // the tree that the test gate reviews: a commit hook that runs the same review (legionctl's
        // scripts/test-gate.sh, which cannot reach the operator from a session's sandbox) lets this tree alone through
        let tree = git(&ws.path, &["write-tree"])?;
        self.test_gate(&ws.path)?;
        let mut lines = self
            .gate(
                &ws.path,
                crate::checks::Gate::Checkpoint,
                (Some(session), jail),
            )
            .map_err(|e| refused("checkpoint refused, nothing committed", &e))?;
        let commit = ["commit", "--quiet", "-m", message];
        let mut cmd = if let Some(jail) = jail {
            jail.command(&ws.path, &[&["git"][..], &commit].concat())
        } else {
            let mut cmd = crate::git::command(&ws.path);
            cmd.args(commit);
            cmd
        };
        let out = cmd
            .env("LEGIONCTL_CHECKPOINT_TREE", &tree)
            .output()
            .context("running git commit")?;
        ensure!(
            out.status.success(),
            "git commit: {}{}",
            String::from_utf8_lossy(&out.stdout).trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
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
    /// is the user (todo-147). A branch that changes `.legionctl/` sets its own gates: an agent cannot merge it, but it
    /// can `request` the merge (`workspace.merge-request`, feedbacks/0001 item 1). The user's merge and the request run
    /// the same gate and the main-checkout checks, then `confirm` shows the user the `.legionctl/` diff (a desktop
    /// dialog) before the fast-forward.
    ///
    /// # Errors
    /// The workspace is not on its branch or has uncommitted or untracked files (the gate would test files that the
    /// merge does not ship), the rebase stops on conflicts, an agent's branch changes `.legionctl/` (or a request's does
    /// not), the user does not confirm, the main checkout is not clean or not on the base branch, or the base moves
    /// while the merge runs.
    pub fn merge(
        &self,
        ws: &Workspace,
        (session, jail): (Option<&str>, Option<&crate::session::Jail>),
        request: bool,
        confirm: impl FnOnce(&str) -> Result<bool>,
    ) -> Result<Vec<String>> {
        if ws.is_folder() {
            ensure!(
                !request,
                "a plain folder's workspace has no gates of its own: merge it with workspace merge"
            );
            return crate::folder::merge(&ws.path, &ws.repo);
        }
        let on = git(&ws.path, &["symbolic-ref", "--quiet", "--short", "HEAD"]).unwrap_or_default();
        ensure!(
            on == ws.branch,
            "the workspace {} is on '{on}', not on its branch {}: switch back with `git switch {}`",
            ws.path.display(),
            ws.branch,
            ws.branch
        );
        let (staged, unstaged, untracked, conflicted) = changes(&ws.path, true)?;
        ensure!(
            staged + unstaged + untracked + conflicted == 0,
            "the workspace has uncommitted or untracked files, which the merge does not ship but its gate would test; \
             checkpoint first"
        );
        // one base commit for the rebase, the diff the user sees and the checks before the fast-forward
        let base = git(
            &ws.path,
            &["rev-parse", "--verify", &format!("{}^{{commit}}", ws.base)],
        )?;
        if git(&ws.path, &["rebase", "--quiet", &base]).is_err() {
            bail!(
                "the rebase onto {} stopped on conflicts; resolve them in {}, run `git rebase --continue`, then merge again",
                ws.base,
                ws.path.display()
            );
        }
        let tested = git(&ws.path, &["rev-parse", "HEAD"])?;
        let own_gates = git(
            &ws.path,
            &[&["diff", "--name-only"][..], &gates_range(&base, &tested)].concat(),
        )?;
        ensure!(
            !request || !own_gates.is_empty(),
            "the branch {} does not change .legionctl/: merge it with workspace merge",
            ws.branch
        );
        if !own_gates.is_empty() && session.is_some() && !request {
            let topic = if ws.topic.is_empty() {
                String::new()
            } else {
                format!(" --topic {}", ws.topic)
            };
            bail!(
                "the branch {} changes .legionctl/, which sets its own gates: an agent cannot merge it. Ask the user \
                 with `legionctl workspace merge-request{topic}` (the same gate, then a desktop dialog with the \
                 diff), or the user merges it from a terminal outside an agent session: legionctl -C {} workspace \
                 merge{topic} --dossier {}",
                ws.branch,
                ws.repo.display(),
                ws.dossier
            );
        }
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
        main_ready(ws, (&base, &tested))?;
        if !own_gates.is_empty() {
            ensure!(
                confirm(&gates_diff(ws, (&base, &tested), &own_gates)?)?,
                "the user did not confirm the merge of {}; {} stays where it was (the branch stays rebased)",
                ws.branch,
                ws.base
            );
            // the dialog can wait long: check again after the user's answer
            main_ready(ws, (&base, &tested))?;
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

    /// The commits of `ws` that its base does not have, when its worktree is gone: none while it is there (or for a
    /// plain folder); 0 when its branch is merged into its base, or gone with the branch or the repository.
    ///
    /// # Errors
    /// A folder cannot be read, or git fails (its base is gone).
    pub fn gone_unmerged(&self, ws: &Workspace) -> Result<Option<u32>> {
        // a folder that cannot be read is not gone: the error says why
        let exists = |path: &Path| {
            path.try_exists()
                .with_context(|| format!("cannot read {}", path.display()))
        };
        if ws.is_folder() || exists(&ws.path.join(".git"))? {
            return Ok(None);
        }
        if !exists(&ws.repo)? {
            return Ok(Some(0));
        }
        let branch = format!("refs/heads/{}", ws.branch);
        if git(&ws.repo, &["for-each-ref", "--format=%(refname)", &branch])?.is_empty() {
            return Ok(Some(0));
        }
        let count = git(
            &ws.repo,
            &["rev-list", "--count", &format!("{}..{branch}", ws.base)],
        )?;
        Ok(Some(count.trim().parse()?))
    }

    /// Forget the record of `ws`, whose worktree is gone; its branch stays.
    ///
    /// # Errors
    /// The worktree is still there, or the database write fails.
    pub fn forget(&self, ws: &Workspace) -> Result<String> {
        let unmerged = self.gone_unmerged(ws)?.with_context(|| {
            format!(
                "the workspace {} is still there: close it with legionctl workspace close",
                ws.path.display()
            )
        })?;
        self.db().execute(
            "UPDATE workspaces SET closed = ?1 WHERE path = ?2 AND closed IS NULL",
            params![now(), ws.path.to_string_lossy()],
        )?;
        let branch = if unmerged == 0 {
            format!(
                "its branch {} is merged into {} or gone",
                ws.branch, ws.base
            )
        } else {
            format!("branch {} kept", ws.branch)
        };
        Ok(format!(
            "forgot the workspace {} ({branch})",
            ws.path.display()
        ))
    }

    /// Forget each workspace whose worktree is gone and whose branch is merged into its base or gone (the operator's
    /// start): one line per workspace forgotten, or kept because git could not tell.
    ///
    /// # Errors
    /// The database read fails.
    pub fn forget_merged_gone_workspaces(&self) -> Result<Vec<String>> {
        let mut lines = Vec::new();
        for ws in self.workspaces(None)? {
            match self.gone_unmerged(&ws) {
                Ok(Some(0)) => lines.push(self.forget(&ws)?),
                Ok(_) => {}
                Err(e) => lines.push(format!("kept the workspace {}: {e:#}", ws.path.display())),
            }
        }
        Ok(lines)
    }

    /// Remove the worktree; the branch stays until it is merged, and the dossier stays. `dry_run` makes the same
    /// checks and tells what close would do.
    ///
    /// # Errors
    /// The workspace has uncommitted changes without `force`, or git refuses.
    pub fn close(&self, ws: &Workspace, force: bool, dry_run: bool) -> Result<String> {
        let what = format!("{} (dossier {})", ws.path.display(), ws.dossier);
        if ws.is_folder() {
            crate::folder::close(&ws.path, &ws.repo, force, dry_run)?;
        } else if ws.path.join(".git").exists() {
            let (staged, unstaged, untracked, conflicted) = changes(&ws.path, true)?;
            let dirty = staged + unstaged + untracked + conflicted;
            ensure!(
                force || dirty == 0,
                "the workspace {what} has {dirty} uncommitted changes; run `legionctl workspace checkpoint <message>`, \
                 or add --force to discard them"
            );
            if dry_run {
                return Ok(format!(
                    "would close {what}{}",
                    if dirty > 0 {
                        format!(" and discard its {dirty} uncommitted changes")
                    } else {
                        String::new()
                    }
                ));
            }
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
        if dry_run {
            return Ok(format!("would close {what}"));
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
        let closed = format!("closed {what}");
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
