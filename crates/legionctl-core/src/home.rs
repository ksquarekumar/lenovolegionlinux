//! The dossier home in each directory (design 23, E1a): at the first dossier use in a directory, `<root>/agentWorkspaces`
//! becomes a link to the dossier root (the whole home; the bound dossier is `agentWorkspaces/<name>`). A real folder
//! there is imported first: each first-level subfolder becomes a registered dossier, copied with a hash compare, and
//! the folder goes to a backup. In a git repository, git ignores the link: `/agentWorkspaces` in the repository's
//! `info/exclude` (shared by the main checkout and its worktrees; the user's `.gitignore` and index stay as they are).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use blake2::{Blake2b, Digest as _, digest::consts::U16};

use crate::dossier::{Dossiers, new_id, valid_name, write_id};
use crate::git::text as git;
use crate::workspace::git_common_dir;

/// The name of the link in a directory root.
pub const LINK: &str = "agentWorkspaces";

/// Where the migration and the import keep what they replace: `~/.local/state/legionctl/backup`.
pub(crate) fn backup_root() -> PathBuf {
    crate::db::default_path().with_file_name("backup")
}

/// Link the dossier root into `dir` (a repository root or a plain folder), import a real `agentWorkspaces` folder
/// there first, and make git ignore the link. Returns a line for each thing done or refused; a link to another place
/// and a folder that keeps files that are not dossiers stay as they are.
///
/// # Errors
/// The import, the link or the ignore line fails; nothing of the source folder is lost.
pub(crate) fn link(dossiers: &Dossiers, dir: &Path) -> Result<Vec<String>> {
    let at = dir.join(LINK);
    // the root is made with the first dossier or the first link to it (a first start makes nothing)
    std::fs::create_dir_all(&dossiers.root)
        .with_context(|| format!("making {}", dossiers.root.display()))?;
    let root = dossiers.root.canonicalize()?;
    if holds_root(&root, dir) {
        return Ok(Vec::new());
    }
    let mut out = Vec::new();
    match std::fs::symlink_metadata(&at) {
        Ok(meta) if meta.file_type().is_symlink() => {
            if at.canonicalize().ok() != Some(root) {
                return Ok(vec![refused(&at, dossiers)]);
            }
        }
        Ok(meta) if meta.is_dir() => {
            let (lines, kept) = import(dossiers, &at)?;
            out.extend(lines);
            if !kept.is_empty() {
                out.push(format!(
                    "{} keeps {}, which are not dossiers: move them out, then use a dossier here again",
                    at.display(),
                    kept.join(", ")
                ));
                return Ok(out);
            }
            std::os::unix::fs::symlink(&dossiers.root, &at)?;
            out.push(format!(
                "linked {} to {}",
                at.display(),
                dossiers.root.display()
            ));
        }
        Ok(_) => return Ok(vec![refused(&at, dossiers)]),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            match std::os::unix::fs::symlink(&dossiers.root, &at) {
                Ok(()) => out.push(format!(
                    "linked {} to {}",
                    at.display(),
                    dossiers.root.display()
                )),
                // a session that started at the same time made the same link
                Err(e)
                    if e.kind() == std::io::ErrorKind::AlreadyExists
                        && at.canonicalize().ok() == Some(root) => {}
                Err(e) => return Err(e.into()),
            }
        }
        Err(e) => return Err(e.into()),
    }
    out.extend(ignore(dir)?);
    Ok(out)
}

/// The folder that holds the root (the home, where `agentWorkspaces` is the root itself), and the root and the folders
/// in it, get no link: there it is the root.
fn holds_root(root: &Path, dir: &Path) -> bool {
    let at = dir.join(LINK);
    let is_link = std::fs::symlink_metadata(&at).is_ok_and(|m| m.file_type().is_symlink());
    (!is_link && at.canonicalize().is_ok_and(|a| a == root))
        || dir.canonicalize().is_ok_and(|d| d.starts_with(root))
}

/// Why `<dir>/agentWorkspaces` is not the link, for `doctor`; none when it is the link, is not there yet, or `dir`
/// holds the root (EV-review-11 U-15: a workspace on $HOME gave a false finding).
#[must_use]
pub fn finding(dossiers: &Dossiers, dir: &Path) -> Option<String> {
    if dossiers
        .root
        .canonicalize()
        .is_ok_and(|root| holds_root(&root, dir))
    {
        return None;
    }
    let at = dir.join(LINK);
    match std::fs::symlink_metadata(&at) {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => return Some(format!("cannot read {}: {e}", at.display())),
    }
    // a link that resolves to the home, or the home itself (a folder workspace in $HOME); a link to nowhere never
    // matches
    if at
        .canonicalize()
        .is_ok_and(|to| dossiers.root.canonicalize().is_ok_and(|root| to == root))
    {
        return None;
    }
    Some(refused(&at, dossiers))
}

fn refused(at: &Path, dossiers: &Dossiers) -> String {
    let what = match std::fs::read_link(at) {
        Ok(target) => format!("links to {}", target.display()),
        Err(_) if at.is_dir() => "is a folder that keeps files that are not dossiers".into(),
        Err(_) => "is a file".into(),
    };
    format!(
        "{} {what}, not {}: legionctl does not replace it",
        at.display(),
        dossiers.root.display()
    )
}

/// Make git ignore `<dir>/agentWorkspaces` when it does not: `/agentWorkspaces` (no trailing `/`: git sees a link as
/// a file) goes into `<common dir>/info/exclude`, never into a file of the user's checkout (EV-review-10 S7). A plain
/// folder has nothing to do.
fn ignore(dir: &Path) -> Result<Vec<String>> {
    if crate::workspace::repo_of(dir)?.is_none() || git(dir, &["check-ignore", "-q", LINK]).is_ok()
    {
        return Ok(Vec::new());
    }
    let exclude = git_common_dir(dir)?.join("info/exclude");
    let line = format!("/{LINK}");
    append_line(&exclude, &line)?;
    let done = format!("added {line} to {}", exclude.display());
    if let Err(e) = git(dir, &["check-ignore", "-q", LINK]) {
        bail!("git does not ignore {LINK} in {}: {e}", dir.display());
    }
    Ok(vec![done])
}

fn append_line(file: &Path, line: &str) -> Result<()> {
    // a file that cannot be read is not written over
    let text = legionctl_machine::sysroot::read_file(file)?.unwrap_or_default();
    let sep = if text.is_empty() || text.ends_with('\n') {
        ""
    } else {
        "\n"
    };
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(file, format!("{text}{sep}{line}\n"))
        .with_context(|| format!("writing {}", file.display()))
}

/// Import the real folder `from` (job `dossier.import`, one event per dossier): each first-level subfolder with a
/// dossier name is copied to `<root>/.import-<source hash>/<name>`, compared by hash, given an id, registered and moved
/// to `<root>/<name>` (`<name>-imported-<n>` when the name is in use). A subfolder that an earlier, cut-off import put
/// in place is not copied again; one that it did not is copied again from the source. When nothing else is in `from`, it moves to the backup. Returns the lines and the
/// names that are not dossiers (they stay, and so does `from`).
fn import(dossiers: &Dossiers, from: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let ops = dossiers.operator();
    let job = ops.job_begin("dossier.import", &serde_json::json!({"from": from}))?;
    let outcome = import_steps(dossiers, from);
    let result = match &outcome {
        Ok((lines, kept)) => Ok(serde_json::json!({"lines": lines, "kept": kept})),
        Err(e) => Err(format!("{e:#}")),
    };
    ops.job_end(job, &result)?;
    outcome
}

fn import_steps(dossiers: &Dossiers, from: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let time = crate::db::now();
    // one staging folder per source, so a cut-off import makes its copies again in the same place
    let staging = dossiers.root.join(format!(
        ".import-{}",
        &hash_of(from.to_string_lossy().as_bytes())[..12]
    ));
    let (mut lines, mut kept) = (Vec::new(), Vec::new());
    let mut entries: Vec<_> = std::fs::read_dir(from)?.collect::<std::io::Result<_>>()?;
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !entry.file_type()?.is_dir() || !valid_name(&name) {
            kept.push(name);
            continue;
        }
        let origin = entry.path();
        let (id, target, new) = match dossiers.by_origin(&origin)? {
            Some((_, registered)) if dossiers.root.join(&registered).is_dir() => {
                lines.push(format!(
                    "{} is dossier {registered} (imported before)",
                    origin.display()
                ));
                continue;
            }
            // registered, then cut off before the move: the source is still here, so copy it again
            Some((id, registered)) => (id, registered, false),
            None => {
                let mut target = name.clone();
                let mut n = 0;
                while dossiers.name_used(&target)? {
                    n += 1;
                    target = format!("{name}-imported-{n}");
                }
                (new_id()?, target, true)
            }
        };
        let copy = staging.join(&name);
        if std::fs::symlink_metadata(&copy).is_ok() {
            std::fs::remove_dir_all(&copy)?;
        }
        copy_verified(&origin, &copy)?;
        write_id(&copy, &id, &name)?;
        if new {
            dossiers.register(&id, &target, Some(&origin.to_string_lossy()))?;
        }
        std::fs::rename(&copy, dossiers.root.join(&target))?;
        dossiers.operator().record(
            "operator",
            "dossier.import",
            &serde_json::json!({"from": origin, "dossier": target, "id": id}),
        )?;
        lines.push(if target == name {
            format!("imported {} as dossier {target}", origin.display())
        } else {
            format!(
                "imported {} as dossier {target} (the name {name} is in use)",
                origin.display()
            )
        });
    }
    // the staging folder, when each copy is in place
    let _ = std::fs::remove_dir(&staging);
    if kept.is_empty() {
        let backup = backup_root().join(format!(
            "{LINK}-{}-{time}",
            &hash_of(from.to_string_lossy().as_bytes())[..12]
        ));
        std::fs::create_dir_all(backup_root())
            .and_then(|()| std::fs::rename(from, &backup))
            .with_context(|| format!("moving {} to {}", from.display(), backup.display()))?;
        lines.push(format!("moved {} to {}", from.display(), backup.display()));
    }
    Ok((lines, kept))
}

fn hash_of(bytes: &[u8]) -> String {
    format!("{:x}", Blake2b::<U16>::digest(bytes))
}

/// Copy the folder `from` to `to` (files, folders and links, with their modes), then compare the hash of every file
/// and link on both sides.
///
/// # Errors
/// A copy fails, or a hash differs.
pub(crate) fn copy_verified(from: &Path, to: &Path) -> Result<()> {
    fn copy(from: &Path, to: &Path) -> Result<()> {
        let meta = std::fs::symlink_metadata(from)?;
        if meta.file_type().is_symlink() {
            std::os::unix::fs::symlink(std::fs::read_link(from)?, to)?;
        } else if meta.is_dir() {
            std::fs::create_dir_all(to)?;
            for entry in std::fs::read_dir(from)? {
                let entry = entry?;
                copy(&entry.path(), &to.join(entry.file_name()))?;
            }
            std::fs::set_permissions(to, meta.permissions())?;
        } else {
            std::fs::copy(from, to)?;
        }
        Ok(())
    }
    copy(from, to).with_context(|| format!("copying {} to {}", from.display(), to.display()))?;
    let (a, b) = (tree_hash(from)?, tree_hash(to)?);
    ensure!(
        a == b,
        "the copy of {} at {} differs from the source; the source stays",
        from.display(),
        to.display()
    );
    Ok(())
}

/// One hash over each path in `dir` (sorted) with its file content or link target.
fn tree_hash(dir: &Path) -> Result<String> {
    fn walk(base: &Path, dir: &Path, hash: &mut Blake2b<U16>) -> Result<()> {
        let mut entries: Vec<_> = std::fs::read_dir(dir)?.collect::<std::io::Result<_>>()?;
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            hash.update(path.strip_prefix(base)?.to_string_lossy().as_bytes());
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                hash.update(std::fs::read_link(&path)?.to_string_lossy().as_bytes());
            } else if kind.is_dir() {
                walk(base, &path, hash)?;
            } else {
                hash.update(hash_of(&std::fs::read(&path)?).as_bytes());
            }
        }
        Ok(())
    }
    let mut hash = Blake2b::<U16>::new();
    walk(dir, dir, &mut hash)?;
    Ok(format!("{:x}", hash.finalize()))
}
