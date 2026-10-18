//! Workspaces of plain folders (note 18, W3): a session sees the source folder at its own path through an overlay
//! (bwrap `--overlay`, in the session's user namespace); every write goes to the dossier's upper layer. Status lists
//! the changes, checkpoint keeps a copy of them, merge applies them to the source, close drops them.
//!
//! The upper layer as the kernel writes it: a changed or new file is a file, a deleted entry is a 0:0 character
//! device (a whiteout), and a folder that replaced the source's folder carries `user.overlay.opaque`. The kernel makes
//! an upper entry when the session first changes it (copy up), so the entry's birth time is the time of the session's
//! copy: a source entry changed after that (its ctime is later) is a conflict, and merge does not write it.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail, ensure};

/// One change in the upper layer, relative to the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    Added(PathBuf),
    Modified(PathBuf),
    Deleted(PathBuf),
    /// A folder that replaces the source's folder of that name (its content is in the upper layer).
    Replaced(PathBuf),
}

impl Change {
    fn path(&self) -> &Path {
        match self {
            Change::Added(p) | Change::Modified(p) | Change::Deleted(p) | Change::Replaced(p) => p,
        }
    }
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
    use std::os::unix::fs::FileTypeExt;
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
pub(crate) fn clear(dir: &Path) -> Result<()> {
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

/// The latest ctime of `path` and, for a folder, of everything in it (symbolic links are not followed).
fn changed_at(path: &Path) -> Result<SystemTime> {
    let meta = std::fs::symlink_metadata(path)?;
    let mut latest = SystemTime::UNIX_EPOCH
        + Duration::new(
            u64::try_from(meta.ctime())?,
            u32::try_from(meta.ctime_nsec())?,
        );
    if meta.is_dir() {
        for entry in std::fs::read_dir(path)? {
            latest = latest.max(changed_at(&entry?.path())?);
        }
    }
    Ok(latest)
}

/// Whether the source changed `change` after the session's copy, or a folder on its path in the source is not a
/// folder now (a symbolic link there would take the write out of the source).
fn conflict(upper: &Path, source: &Path, change: &Change) -> Result<bool> {
    let rel = change.path();
    let mut at = source.to_path_buf();
    for part in rel.parent().into_iter().flat_map(Path::components) {
        at.push(part);
        if std::fs::symlink_metadata(&at).is_ok_and(|m| !m.is_dir()) {
            return Ok(true);
        }
    }
    let to = source.join(rel);
    if matches!(change, Change::Added(_)) || to.symlink_metadata().is_err() {
        return Ok(false);
    }
    let copied = std::fs::symlink_metadata(upper.join(rel))?
        .created()
        .context("the workspace's file system does not keep the birth time of a file")?;
    Ok(changed_at(&to)? > copied)
}

/// Write the upper entry of `change` into the source: a new entry goes in next to its place and then takes it in one
/// rename (an exchange when an entry is there), so the source has the old entry or the new one, never a part.
fn apply(upper: &Path, source: &Path, change: &Change) -> Result<()> {
    let rel = change.path();
    let to = source.join(rel);
    if let Change::Deleted(_) = change {
        return remove(&to);
    }
    let parent = to.parent().context("no parent")?;
    std::fs::create_dir_all(parent)?;
    let name = to.file_name().context("no file name")?.to_string_lossy();
    let new = parent.join(format!(".{name}.legionctl-merge"));
    // a merge that stopped half way left it
    remove(&new)?;
    copy(&upper.join(rel), &new)?;
    if to.symlink_metadata().is_ok() {
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &new,
            rustix::fs::CWD,
            &to,
            rustix::fs::RenameFlags::EXCHANGE,
        )
        .with_context(|| format!("replacing {}", to.display()))?;
        // the old entry, after the exchange
        remove(&new)
    } else {
        std::fs::rename(&new, &to).with_context(|| format!("writing {}", to.display()))
    }
}

/// Apply the changes to the source; each change that is applied leaves the layer. A change of an entry that the source
/// changed after the session's copy stays in the layer, and the merge says which.
///
/// # Errors
/// Nothing to merge, a conflict, or a change cannot be applied (it and the changes after it stay in the layer, so
/// nothing is lost).
pub fn merge(path: &Path, source: &Path) -> Result<Vec<String>> {
    let changes = changes(path, source)?;
    ensure!(!changes.is_empty(), "nothing to merge");
    let upper = upper(path);
    let (mut merged, mut kept) = (Vec::new(), Vec::new());
    for change in &changes {
        if conflict(&upper, source, change)? {
            kept.push(change.path().display().to_string());
            continue;
        }
        apply(&upper, source, change)?;
        remove(&upper.join(change.path()))?;
        merged.push(change.to_string());
    }
    if kept.is_empty() {
        clear(&upper)?;
        clear(&work(path))?;
        return Ok(merged);
    }
    bail!(
        "{}these changed in {} after the session changed them, so the merge did not write them: {}. The session's \
         versions are in {}: compare them with your files and copy what you want to keep, then run `legionctl \
         workspace close --force` to drop them",
        if merged.is_empty() {
            String::new()
        } else {
            format!("merged {}; ", merged.join(", "))
        },
        source.display(),
        kept.join(", "),
        upper.display()
    )
}

/// Drop the layer and its checkpoints; `dry_run` makes the check only.
///
/// # Errors
/// Changes are not merged and `force` is not given.
pub fn close(path: &Path, source: &Path, force: bool, dry_run: bool) -> Result<()> {
    let changes = changes(path, source)?;
    ensure!(
        force || changes.is_empty(),
        "the workspace {} has {} changes not merged into {}; run `legionctl workspace merge`, or add --force to drop \
         them",
        path.display(),
        changes.len(),
        source.display()
    );
    if dry_run {
        return Ok(());
    }
    clear(path)?;
    std::fs::remove_dir(path)?;
    Ok(())
}
