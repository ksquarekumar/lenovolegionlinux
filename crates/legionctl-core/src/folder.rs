//! Workspaces of plain folders (note 18, W3): a session sees the source folder at its own path through an overlay
//! (bwrap `--overlay`, in the session's user namespace); every write goes to the dossier's upper layer. Status lists
//! the changes, checkpoint keeps a copy of them, merge applies them to the source, close drops them.
//!
//! The upper layer as the kernel writes it: a changed or new file is a file, a deleted entry is a 0:0 character
//! device (a whiteout), and a folder that replaced the source's folder carries `user.overlay.opaque`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use crate::workspace::LINK;

/// One change in the upper layer, relative to the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Added(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
    /// A folder that replaces the source's folder of that name (its content is in the upper layer).
    Replaced(PathBuf),
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Change::Added(p) => write!(f, "A {}", p.display()),
            Change::Modified(p) => write!(f, "M {}", p.display()),
            Change::Deleted(p) => write!(f, "D {}", p.display()),
            Change::Replaced(p) => write!(f, "R {}/", p.display()),
        }
    }
}

#[must_use]
pub fn upper(path: &Path) -> PathBuf {
    path.join("upper")
}

#[must_use]
pub fn work(path: &Path) -> PathBuf {
    path.join("work")
}

fn whiteout(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    meta.file_type().is_char_device() && meta.rdev() == 0
}

fn opaque(dir: &Path) -> bool {
    let mut value = [0u8; 8];
    rustix::fs::getxattr(dir, "user.overlay.opaque", &mut value).is_ok_and(|n| value[..n] == *b"y")
}

/// The changes in the upper layer at `path` against the source `source`, sorted by path.
///
/// # Errors
/// The upper layer cannot be read.
pub fn changes(path: &Path, source: &Path) -> Result<Vec<Change>> {
    fn walk(upper: &Path, source: &Path, rel: &Path, out: &mut Vec<Change>) -> Result<()> {
        let mut entries: Vec<_> =
            std::fs::read_dir(upper.join(rel))?.collect::<std::io::Result<_>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let rel = rel.join(entry.file_name());
            if rel == Path::new(LINK) {
                continue;
            }
            let meta = std::fs::symlink_metadata(entry.path())?;
            if whiteout(&meta) {
                out.push(Change::Deleted(rel));
            } else if meta.is_dir() {
                if opaque(&entry.path()) {
                    out.push(Change::Replaced(rel));
                } else if source.join(&rel).is_dir() {
                    walk(upper, source, &rel, out)?;
                } else {
                    out.push(Change::Added(rel));
                }
            } else if source.join(&rel).symlink_metadata().is_ok() {
                out.push(Change::Modified(rel));
            } else {
                out.push(Change::Added(rel));
            }
        }
        Ok(())
    }
    let mut out = Vec::new();
    let upper = upper(path);
    if upper.exists() {
        walk(&upper, source, Path::new(""), &mut out)?;
    }
    Ok(out)
}

/// Copy `from` (a file, a symlink or a folder) to `to`, keeping modes; whiteouts inside are left out.
fn copy(from: &Path, to: &Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(from)?;
    if meta.is_dir() {
        std::fs::create_dir_all(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            if !whiteout(&entry.metadata()?) {
                copy(&entry.path(), &to.join(entry.file_name()))?;
            }
        }
    } else if meta.file_type().is_symlink() {
        let _ = std::fs::remove_file(to);
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)?;
    } else {
        std::fs::copy(from, to).with_context(|| format!("copying {}", from.display()))?;
    }
    Ok(())
}

fn remove(path: &Path) -> Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path)?,
        Ok(_) => std::fs::remove_file(path)?,
        Err(_) => {}
    }
    Ok(())
}

/// Empty a folder the overlay wrote (its `work/work` folder has mode 000).
fn clear(dir: &Path) -> Result<()> {
    if !dir.exists() {
        return Ok(());
    }
    walk_dirs(dir);
    std::fs::remove_dir_all(dir)?;
    std::fs::create_dir_all(dir)?;
    Ok(())
}

/// Make every folder under `dir` accessible (the overlay leaves `work/work` with mode 000).
fn walk_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut out = vec![dir.to_path_buf()];
    let mut i = 0;
    while i < out.len() {
        let current = out[i].clone();
        // a folder with mode 000 cannot be listed before it is made readable
        let _ = std::fs::set_permissions(
            &current,
            std::os::unix::fs::PermissionsExt::from_mode(0o700),
        );
        for entry in std::fs::read_dir(&current).into_iter().flatten().flatten() {
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                out.push(entry.path());
            }
        }
        i += 1;
    }
    out
}

/// Keep a copy of the changes in `checkpoints/<time>/` (files, plus `DELETED` and `MESSAGE`).
///
/// # Errors
/// Nothing changed, or the copy fails.
pub fn checkpoint(path: &Path, source: &Path, message: &str) -> Result<String> {
    let changes = changes(path, source)?;
    if changes.is_empty() {
        return Ok("nothing to checkpoint".into());
    }
    let dir = path.join("checkpoints").join(crate::db::now().to_string());
    std::fs::create_dir_all(&dir)?;
    let mut deleted = Vec::new();
    for change in &changes {
        match change {
            Change::Added(rel) | Change::Modified(rel) | Change::Replaced(rel) => {
                let to = dir.join("files").join(rel);
                std::fs::create_dir_all(to.parent().context("no parent")?)?;
                copy(&upper(path).join(rel), &to)?;
            }
            Change::Deleted(rel) => deleted.push(rel.display().to_string()),
        }
    }
    std::fs::write(dir.join("DELETED"), deleted.join("\n"))?;
    std::fs::write(dir.join("MESSAGE"), message)?;
    Ok(format!(
        "checkpoint {}: {} changes ({message})",
        dir.display(),
        changes.len()
    ))
}

/// Apply the changes to the source, then empty the layer.
///
/// # Errors
/// A change cannot be applied (the layer stays, so nothing is lost).
pub fn merge(path: &Path, source: &Path) -> Result<Vec<String>> {
    let changes = changes(path, source)?;
    ensure!(!changes.is_empty(), "nothing to merge");
    for change in &changes {
        match change {
            Change::Added(rel) | Change::Modified(rel) => {
                let to = source.join(rel);
                std::fs::create_dir_all(to.parent().context("no parent")?)?;
                copy(&upper(path).join(rel), &to)?;
            }
            Change::Replaced(rel) => {
                remove(&source.join(rel))?;
                copy(&upper(path).join(rel), &source.join(rel))?;
            }
            Change::Deleted(rel) => remove(&source.join(rel))?,
        }
    }
    clear(&upper(path))?;
    clear(&work(path))?;
    Ok(changes.iter().map(ToString::to_string).collect())
}

/// Drop the layer and its checkpoints.
///
/// # Errors
/// Changes are not merged and `force` is not given.
pub fn close(path: &Path, source: &Path, force: bool) -> Result<()> {
    let changes = changes(path, source)?;
    ensure!(
        force || changes.is_empty(),
        "the workspace has {} changes not merged into {}; merge them, or pass force to drop them",
        changes.len(),
        source.display()
    );
    clear(path)?;
    std::fs::remove_dir(path)?;
    Ok(())
}
