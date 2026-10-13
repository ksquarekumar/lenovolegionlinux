//! Images of a repository (note 20, I1 part 1): read its Dockerfiles into the services description
//! (`docker-compose-services.json`) and the build-cache description (`docker-compose-cache.json`).
//!
//! A service per Dockerfile (named by its folder, or by `<name>.Dockerfile`), its stages and base images, ports from
//! `EXPOSE`, the run entrypoint from `ENTRYPOINT` + `CMD`, the health entrypoint from `HEALTHCHECK CMD`; the cache
//! description names the bases and the lockfiles in the build context that invalidate a layer.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

const LOCKFILES: [&str; 8] = [
    "Cargo.lock",
    "package-lock.json",
    "bun.lock",
    "pnpm-lock.yaml",
    "yarn.lock",
    "uv.lock",
    "poetry.lock",
    "go.sum",
];

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Build {
    pub context: String,
    pub dockerfile: String,
    /// The last stage, which `docker build` builds by default.
    pub target: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Service {
    pub build: Build,
    /// run and health, when the Dockerfile says.
    pub entrypoints: BTreeMap<String, Vec<String>>,
    pub ports: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CacheEntry {
    /// External base images (stages that build on other stages are not bases).
    pub bases: Vec<String>,
    /// Lockfiles in the build context: a change rebuilds the layers that copy them.
    pub lockfiles: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Scan {
    pub services: BTreeMap<String, Service>,
    pub cache: BTreeMap<String, CacheEntry>,
}

/// Dockerfile instructions with their continuation lines joined, comments dropped.
fn instructions(text: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut current = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('#') || (line.is_empty() && current.is_empty()) {
            continue;
        }
        if let Some(part) = line.strip_suffix('\\') {
            current.push_str(part);
            current.push(' ');
            continue;
        }
        current.push_str(line);
        if let Some((word, rest)) = current.trim().split_once(char::is_whitespace) {
            out.push((word.to_uppercase(), rest.trim().to_string()));
        }
        current.clear();
    }
    out
}

/// The exec form (`["a", "b"]`) as it is; the shell form as `sh -c`.
fn command(text: &str) -> Vec<String> {
    serde_json::from_str(text).unwrap_or_else(|_| vec!["/bin/sh".into(), "-c".into(), text.into()])
}

fn dockerfiles(dir: &Path, base: &Path, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = entry.path();
        if name.starts_with('.') || name == "target" || name == "node_modules" {
            continue;
        }
        if path.is_dir() {
            dockerfiles(&path, base, out);
        } else if name == "Dockerfile" || name == "Containerfile" || name.ends_with(".Dockerfile") {
            out.push(path.strip_prefix(base).unwrap_or(&path).to_path_buf());
        }
    }
}

/// Read every Dockerfile of the repository at `repo`.
#[must_use]
pub fn scan(repo: &Path) -> Scan {
    let mut files = Vec::new();
    dockerfiles(repo, repo, &mut files);
    let mut out = Scan::default();
    for file in files {
        let text = std::fs::read_to_string(repo.join(&file)).unwrap_or_default();
        let context = file
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let file_name = file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let name = match file_name.strip_suffix(".Dockerfile") {
            Some(stem) => stem.to_string(),
            None if context == Path::new(".") => repo
                .file_name()
                .map_or("app".into(), |n| n.to_string_lossy().into_owned()),
            None => context
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default(),
        };
        let mut service = Service {
            build: Build {
                context: context.display().to_string(),
                dockerfile: file.display().to_string(),
                target: None,
            },
            ..Service::default()
        };
        let mut stages = Vec::new();
        let mut entry = (None, None);
        let mut cache = CacheEntry::default();
        for (word, rest) in instructions(&text) {
            match word.as_str() {
                "FROM" => {
                    let parts: Vec<&str> = rest
                        .split_whitespace()
                        .filter(|p| !p.starts_with("--"))
                        .collect();
                    let image = parts.first().copied().unwrap_or_default().to_string();
                    let stage = match parts.as_slice() {
                        [_, as_word, stage] if as_word.eq_ignore_ascii_case("as") => {
                            Some((*stage).to_string())
                        }
                        _ => None,
                    };
                    if !stages.contains(&image)
                        && image != "scratch"
                        && !cache.bases.contains(&image)
                    {
                        cache.bases.push(image);
                    }
                    service.build.target.clone_from(&stage);
                    stages.extend(stage);
                    // each stage starts its own entrypoint and ports
                    entry = (None, None);
                    service.ports.clear();
                    service.entrypoints.remove("health");
                }
                "EXPOSE" => service
                    .ports
                    .extend(rest.split_whitespace().map(String::from)),
                "ENTRYPOINT" => entry.0 = Some(command(&rest)),
                "CMD" => entry.1 = Some(command(&rest)),
                "HEALTHCHECK" => {
                    if let Some(cmd) = rest.split_once("CMD").map(|(_, c)| c.trim()) {
                        service.entrypoints.insert("health".into(), command(cmd));
                    }
                }
                _ => {}
            }
        }
        let run: Vec<String> = entry
            .0
            .into_iter()
            .flatten()
            .chain(entry.1.into_iter().flatten())
            .collect();
        if !run.is_empty() {
            service.entrypoints.insert("run".into(), run);
        }
        let context_dir = repo.join(context);
        cache.lockfiles = LOCKFILES
            .iter()
            .filter(|l| context_dir.join(l).is_file())
            .map(|l| {
                if context == Path::new(".") {
                    (*l).to_string()
                } else {
                    context.join(l).display().to_string()
                }
            })
            .collect();
        out.cache.insert(name.clone(), cache);
        out.services.insert(name, service);
    }
    out
}
