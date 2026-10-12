//! Agent worktrees: isolated checkouts of any git repo, created, tracked (size, dirty state) and removed.
//!
//! The disks here are ext4, which cannot reflink, so a worktree is a `git worktree`: it shares the repo's object
//! store and only the checked-out files and build output use space. On btrfs/xfs a reflink copy of build
//! directories could be added later; the registry and API stay the same.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Worktree {
    pub name: String,
    pub repo: PathBuf,
    pub path: PathBuf,
    pub branch: String,
    pub created_unix: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct WorktreeStatus {
    #[serde(flatten)]
    pub worktree: Worktree,
    pub exists: bool,
    /// Bytes on disk inside the worktree (checked-out files and build output; git objects are shared).
    pub size_bytes: Option<u64>,
    /// Uncommitted changes present.
    pub dirty: Option<bool>,
}

/// Where worktrees live (config `worktree.root`) and where the registry is kept.
#[derive(Debug, Clone)]
pub struct Store {
    pub root: PathBuf,
    pub registry: PathBuf,
}

impl Store {
    /// The store for worktrees under `root`, with the registry in `$XDG_STATE_HOME/legionctl`.
    #[must_use]
    pub fn new(root: PathBuf) -> Store {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        let state = std::env::var_os("XDG_STATE_HOME")
            .map_or_else(|| home.join(".local/state"), PathBuf::from);
        Store {
            root,
            registry: state.join("legionctl/worktrees.json"),
        }
    }
}

/// Names arrive from agents over MCP: only allow characters that cannot escape the worktree root.
///
/// # Errors
/// `name` is not 1–64 characters, has characters other than ASCII letters, digits, `-`, `_` and `.`, or starts
/// with `.` or `-`.
pub fn validate_name(name: &str) -> Result<()> {
    ensure!(
        (1..=64).contains(&name.len()),
        "worktree name must be 1-64 characters"
    );
    ensure!(
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')),
        "worktree name may only contain letters, digits, '-', '_' and '.'"
    );
    ensure!(
        !name.starts_with(['.', '-']),
        "worktree name must not start with '.' or '-'"
    );
    Ok(())
}

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

#[must_use]
pub fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_dir() {
        return meta.len();
    }
    std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| dir_size(&e.path()))
        .sum()
}

impl Store {
    fn load(&self) -> Result<Vec<Worktree>> {
        match std::fs::read_to_string(&self.registry) {
            Ok(text) => serde_json::from_str(&text)
                .with_context(|| format!("reading {}", self.registry.display())),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
            Err(e) => Err(e).with_context(|| format!("reading {}", self.registry.display())),
        }
    }

    fn save(&self, items: &[Worktree]) -> Result<()> {
        let dir = self
            .registry
            .parent()
            .context("registry path has no parent")?;
        std::fs::create_dir_all(dir)?;
        // write then rename, so a crash never leaves a half-written registry
        let tmp = self.registry.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(items)?)?;
        std::fs::rename(&tmp, &self.registry)?;
        Ok(())
    }

    /// # Errors
    /// `name` or `base` is not valid, `repo` is not a git repo, the worktree path exists, or `git worktree add` fails.
    pub fn create(&self, repo: &Path, name: &str, base: Option<&str>) -> Result<Worktree> {
        validate_name(name)?;
        if let Some(base) = base {
            ensure!(
                !base.starts_with('-'),
                "base must be a git ref, not an option"
            );
        }
        let mut items = self.load()?;
        ensure!(
            !items.iter().any(|w| w.name == name),
            "a worktree named {name} already exists"
        );
        let top = PathBuf::from(git(repo, &["rev-parse", "--show-toplevel"])?);
        let repo_name = top.file_name().context("repo has no directory name")?;
        let path = self.root.join(repo_name).join(name);
        ensure!(!path.exists(), "{} already exists", path.display());
        std::fs::create_dir_all(path.parent().context("worktree path has no parent")?)?;
        let branch = format!("legionctl/{name}");
        let path_str = path.to_str().context("worktree path is not UTF-8")?;
        git(
            &top,
            &[
                "worktree",
                "add",
                "-b",
                &branch,
                path_str,
                base.unwrap_or("HEAD"),
            ],
        )?;
        let created_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_secs());
        let wt = Worktree {
            name: name.into(),
            repo: top,
            path,
            branch,
            created_unix,
        };
        items.push(wt.clone());
        self.save(&items)?;
        Ok(wt)
    }

    /// # Errors
    /// The worktree registry cannot be read.
    pub fn list(&self, with_sizes: bool) -> Result<Vec<WorktreeStatus>> {
        Ok(self
            .load()?
            .into_iter()
            .map(|w| {
                let exists = w.path.is_dir();
                WorktreeStatus {
                    size_bytes: (exists && with_sizes).then(|| dir_size(&w.path)),
                    dirty: exists.then(|| {
                        git(&w.path, &["status", "--porcelain"]).is_ok_and(|s| !s.is_empty())
                    }),
                    exists,
                    worktree: w,
                }
            })
            .collect())
    }

    /// Removes the checkout; the branch stays so committed work is never lost.
    ///
    /// # Errors
    /// `name` is not valid or not known, or `git worktree remove` fails (uncommitted changes need `force`).
    pub fn remove(&self, name: &str, force: bool) -> Result<Worktree> {
        validate_name(name)?;
        let mut items = self.load()?;
        let Some(index) = items.iter().position(|w| w.name == name) else {
            bail!("no worktree named {name}")
        };
        let wt = items[index].clone();
        if wt.path.exists() {
            let path_str = wt.path.to_str().context("worktree path is not UTF-8")?;
            let mut args = vec!["worktree", "remove", path_str];
            if force {
                args.push("--force");
            }
            git(&wt.repo, &args)
                .context("worktree has uncommitted changes? pass force to discard them")?;
        } else {
            git(&wt.repo, &["worktree", "prune"])?;
        }
        items.remove(index);
        self.save(&items)?;
        Ok(wt)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("legionctl-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn rejects_names_that_could_escape_the_root() {
        for bad in ["", "../x", "a/b", ".hidden", "-rf", "x y", &"a".repeat(65)] {
            assert!(validate_name(bad).is_err(), "{bad:?} should be rejected");
        }
        for good in ["feat-1", "fix_2", "v1.2"] {
            assert!(validate_name(good).is_ok(), "{good:?} should be accepted");
        }
    }

    #[test]
    fn create_list_remove_round_trip() {
        let base = temp_dir("wt");
        let repo = base.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let run = |args: &[&str]| {
            assert!(
                Command::new("git")
                    .arg("-C")
                    .arg(&repo)
                    .args(args)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        run(&["init", "-q"]);
        run(&[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ]);

        let store = Store {
            root: base.join("worktrees"),
            registry: base.join("state/worktrees.json"),
        };
        let wt = store.create(&repo, "task-1", None).unwrap();
        assert!(wt.path.ends_with("repo/task-1") && wt.path.is_dir());
        assert_eq!(wt.branch, "legionctl/task-1");
        assert!(
            store.create(&repo, "task-1", None).is_err(),
            "duplicate names are refused"
        );

        std::fs::write(wt.path.join("scratch.txt"), "x").unwrap();
        let listed = store.list(true).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].dirty, Some(true));
        assert!(listed[0].size_bytes.unwrap() > 0);

        assert!(
            store.remove("task-1", false).is_err(),
            "dirty worktree needs force"
        );
        store.remove("task-1", true).unwrap();
        assert!(store.list(false).unwrap().is_empty() && !wt.path.exists());
        let _ = std::fs::remove_dir_all(&base);
    }
}
