//! The package store (note 14, T1): MCP servers and tools fetched once, pinned to a version and a content hash, and
//! run from the store. Fetching runs in a sandbox with network but a scratch home (install scripts cannot touch the
//! real one); a package's server runs without network unless the package allows it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail, ensure};
use blake2::digest::consts::U32;
use blake2::{Blake2b, Digest};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::config::PackageConfig;

const MANIFEST: &str = ".legionctl-package.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum PackageKind {
    Npm,
    Pypi,
    Cargo,
    Git,
    /// A local file or folder, copied in.
    Path,
}

/// What the store recorded at fetch time.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Manifest {
    pub name: String,
    pub kind: PackageKind,
    pub source: String,
    pub version: String,
    /// BLAKE2b-256 of the installed tree, hex.
    pub hash: String,
    /// Seconds since the epoch.
    pub fetched: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct PackageState {
    pub name: String,
    pub kind: PackageKind,
    pub source: String,
    pub version: String,
    pub dir: String,
    /// None: not fetched yet.
    pub manifest: Option<Manifest>,
    /// The tree still has the recorded hash.
    pub intact: Option<bool>,
}

pub struct Store {
    root: PathBuf,
}

fn version_of(p: &PackageConfig) -> String {
    p.version.clone().unwrap_or_else(|| "local".into())
}

/// BLAKE2b-256 over the tree: each path (sorted), its mode's exec bit, and its content or link target.
///
/// # Errors
/// The tree cannot be read.
pub fn tree_hash(dir: &Path) -> Result<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
            let path = entry?.path();
            let rel = path.strip_prefix(base)?.to_path_buf();
            if rel.as_os_str() == MANIFEST {
                continue;
            }
            let meta = std::fs::symlink_metadata(&path)?;
            if meta.is_dir() {
                walk(&path, base, out)?;
            } else {
                out.push(rel);
            }
        }
        Ok(())
    }
    let mut files = Vec::new();
    walk(dir, dir, &mut files)?;
    files.sort();
    let mut hash = Blake2b::<U32>::new();
    for rel in files {
        let path = dir.join(&rel);
        let meta = std::fs::symlink_metadata(&path)?;
        hash.update(rel.to_string_lossy().as_bytes());
        hash.update([0]);
        if meta.file_type().is_symlink() {
            hash.update(b"l");
            hash.update(std::fs::read_link(&path)?.to_string_lossy().as_bytes());
        } else {
            let exec = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0;
            hash.update(if exec { b"x" } else { b"f" });
            hash.update(std::fs::read(&path)?);
        }
        hash.update([0]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

impl Store {
    #[must_use]
    pub fn new(root: PathBuf) -> Store {
        Store { root }
    }

    /// `$XDG_DATA_HOME/legionctl/store`, or `~/.local/share/legionctl/store`.
    #[must_use]
    pub fn default_root() -> PathBuf {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        std::env::var_os("XDG_DATA_HOME")
            .map_or_else(|| home.join(".local/share"), PathBuf::from)
            .join("legionctl/store")
    }

    #[must_use]
    pub fn dir(&self, name: &str, p: &PackageConfig) -> PathBuf {
        self.root.join(name).join(version_of(p))
    }

    /// The manifest of a fetched package; `None` when it was not fetched.
    fn manifest(dir: &Path) -> Result<Option<Manifest>> {
        let path = dir.join(MANIFEST);
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        serde_json::from_slice(&bytes)
            .with_context(|| format!("{}: not a package manifest", path.display()))
            .map(Some)
    }

    /// The state of every configured package, with a fresh hash check of each fetched one.
    ///
    /// # Errors
    /// A fetched tree cannot be read.
    pub fn list(&self, packages: &BTreeMap<String, PackageConfig>) -> Result<Vec<PackageState>> {
        packages
            .iter()
            .map(|(name, p)| {
                let dir = self.dir(name, p);
                let manifest = Self::manifest(&dir)?;
                let intact = match &manifest {
                    Some(m) => Some(tree_hash(&dir)? == m.hash),
                    None => None,
                };
                Ok(PackageState {
                    name: name.clone(),
                    kind: p.kind,
                    source: p.source.clone(),
                    version: version_of(p),
                    dir: dir.display().to_string(),
                    manifest,
                    intact,
                })
            })
            .collect()
    }

    /// Fetch `name` into the store (a new folder; an existing version is fetched again only with `force`).
    ///
    /// # Errors
    /// The fetch command fails, or the version is missing for a kind that needs one.
    pub fn fetch(
        &self,
        name: &str,
        p: &PackageConfig,
        force: bool,
        hidden: &[String],
    ) -> Result<Manifest> {
        let dir = self.dir(name, p);
        if let Some(m) = Self::manifest(&dir)?.filter(|_| !force) {
            return Ok(m);
        }
        ensure!(
            p.kind == PackageKind::Path || p.version.is_some(),
            "package {name} needs a version (a pin) for kind {:?}",
            p.kind
        );
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        std::fs::create_dir_all(&dir)?;
        let version = version_of(p);
        let target = dir.display().to_string();
        let argv: Vec<String> = match p.kind {
            PackageKind::Npm => {
                vec![
                    "npm".into(),
                    "install".into(),
                    "--no-save".into(),
                    "--ignore-scripts".into(),
                    "--prefix".into(),
                    target,
                    format!("{}@{version}", p.source),
                ]
            }
            PackageKind::Pypi => {
                vec![
                    "uv".into(),
                    "pip".into(),
                    "install".into(),
                    "--target".into(),
                    target,
                    format!("{}=={version}", p.source),
                ]
            }
            PackageKind::Cargo => vec![
                "cargo".into(),
                "install".into(),
                "--locked".into(),
                "--root".into(),
                target,
                "--version".into(),
                version,
                p.source.clone(),
            ],
            PackageKind::Git => {
                vec![
                    "git".into(),
                    "clone".into(),
                    "--quiet".into(),
                    "--depth".into(),
                    "1".into(),
                    "--branch".into(),
                    version,
                    p.source.clone(),
                    target,
                ]
            }
            PackageKind::Path => {
                let source = crate::config::expand_home(&p.source)?;
                ensure!(source.exists(), "{} does not exist", source.display());
                // a folder's contents at the package root, as a git package has them; a file keeps its name
                let from = if source.is_dir() {
                    source.join(".")
                } else {
                    source
                };
                vec!["cp".into(), "-a".into(), from.display().to_string(), target]
            }
        };
        if let Err(e) = sandboxed(&dir, &argv, hidden) {
            let _ = std::fs::remove_dir_all(&dir);
            return Err(e.context(format!("fetching {name}")));
        }
        if p.kind == PackageKind::Git {
            // the history is not part of the pinned tree
            std::fs::remove_dir_all(dir.join(".git"))?;
        }
        let manifest = Manifest {
            name: name.into(),
            kind: p.kind,
            source: p.source.clone(),
            version: version_of(p),
            hash: tree_hash(&dir)?,
            fetched: crate::db::now(),
        };
        std::fs::write(dir.join(MANIFEST), serde_json::to_vec_pretty(&manifest)?)?;
        Ok(manifest)
    }

    /// The folder of `name` in the store, after a fresh hash check.
    ///
    /// # Errors
    /// The package is not fetched, or its tree no longer has the recorded hash.
    pub fn checked(&self, name: &str, p: &PackageConfig) -> Result<PathBuf> {
        let dir = self.dir(name, p);
        let Some(manifest) = Self::manifest(&dir)? else {
            bail!("package {name} is not fetched; run `legionctl packages fetch {name}`")
        };
        ensure!(
            tree_hash(&dir)? == manifest.hash,
            "package {name} changed since it was fetched ({}); run `legionctl packages fetch {name} --force`",
            dir.display()
        );
        Ok(dir)
    }

    /// The command that runs `name`'s server from the store; refuses a tree that changed since it was fetched.
    ///
    /// # Errors
    /// As [`Store::checked`].
    pub fn command(&self, name: &str, p: &PackageConfig) -> Result<Vec<String>> {
        let dir = self.checked(name, p)?;
        Ok(p.run
            .iter()
            .map(|a| a.replace("{dir}", &dir.to_string_lossy()))
            .collect())
    }
}

/// Run a fetch with network, but with a scratch home and /tmp, and without the ways out a session sandbox hides
/// (build and install scripts are code from the package).
fn sandboxed(dir: &Path, argv: &[String], hidden: &[String]) -> Result<()> {
    let scratch = std::env::temp_dir().join(format!("legionctl-fetch-{}", std::process::id()));
    std::fs::create_dir_all(&scratch)?;
    let mut hiding = Vec::new();
    if let Some(runtime) = std::env::var_os("XDG_RUNTIME_DIR") {
        crate::session::hide(&mut hiding, Path::new(&runtime), hidden)?;
    }
    let out = Command::new("bwrap")
        .args([
            "--die-with-parent",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--tmpfs",
            "/tmp",
        ])
        .args(&hiding)
        .arg("--bind")
        .arg(&scratch)
        .arg("/tmp/home")
        .arg("--bind")
        .arg(dir)
        .arg(dir)
        .args(["--setenv", "HOME", "/tmp/home", "--"])
        .args(argv)
        .output()
        .context("running bwrap")?;
    let _ = std::fs::remove_dir_all(&scratch);
    ensure!(
        out.status.success(),
        "{} failed: {}",
        argv[0],
        String::from_utf8_lossy(&out.stderr).trim()
    );
    Ok(())
}
