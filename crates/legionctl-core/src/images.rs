//! Images of a repository (note 20, I1): read its Dockerfiles and its compose file into the services description
//! (`docker-compose-services.json`) and the build-cache description (`docker-compose-cache.json`).
//!
//! A service per Dockerfile (named by its folder, or by `<name>.Dockerfile`), its stages and base images, ports from
//! `EXPOSE`, the run entrypoint from `ENTRYPOINT` + `CMD`, the health entrypoint from `HEALTHCHECK CMD`; the cache
//! description names the bases and the lockfiles in the build context that invalidate a layer. A compose file at the
//! repository root, as `docker compose config` normalizes it, names the services and adds prebuilt images, ports,
//! mounts, limits and dependencies; its environment is never read, as it can hold secrets.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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
    /// None for a prebuilt image from the compose file.
    pub build: Option<Build>,
    /// The prebuilt image a compose service runs (pulled, not built).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// run and health, when the Dockerfile (or the compose health check) says.
    pub entrypoints: BTreeMap<String, Vec<String>>,
    /// `EXPOSE`d ports, or the compose file's: `<published>:<port>` when published on the host.
    pub ports: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mounts: Vec<Mount>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<Resources>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Mount {
    /// bind (a host path, relative to the repository when inside it) or volume (a named volume).
    pub kind: String,
    pub source: String,
    pub target: String,
    pub read_only: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Resources {
    pub cpus: Option<f64>,
    pub memory_bytes: Option<u64>,
    /// The service reserves a GPU.
    pub gpu: bool,
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
    /// The repository's compose file, which `images run` passes before legionctl's override.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compose_file: Option<String>,
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
        let mut build = Build {
            context: context.display().to_string(),
            dockerfile: file.display().to_string(),
            target: None,
        };
        let mut service = Service::default();
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
                    build.target.clone_from(&stage);
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
        service.build = Some(build);
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

const COMPOSE_FILES: [&str; 4] = [
    "compose.yaml",
    "compose.yml",
    "docker-compose.yaml",
    "docker-compose.yml",
];

/// The Dockerfiles of the repository at `repo`, then its compose file (if any) as `docker compose config` reads it.
///
/// # Errors
/// The compose file is there but `docker compose config` refuses it.
pub fn scan_repository(repo: &Path) -> Result<Scan> {
    let mut out = scan(repo);
    let Some(file) = COMPOSE_FILES.iter().find(|f| repo.join(f).is_file()) else {
        return Ok(out);
    };
    let config = std::process::Command::new("docker")
        .current_dir(repo)
        .args(["compose", "-f", file, "config", "--format", "json"])
        .output()
        .context("running docker compose config")?;
    anyhow::ensure!(
        config.status.success(),
        "docker compose config: {}",
        String::from_utf8_lossy(&config.stderr).trim()
    );
    merge_compose(&mut out, repo, &serde_json::from_slice(&config.stdout)?);
    out.compose_file = Some((*file).to_string());
    Ok(out)
}

/// A path as the repository sees it: relative inside it, absolute outside.
fn relative(repo: &Path, path: &str) -> String {
    Path::new(path)
        .strip_prefix(repo)
        .map_or_else(|_| path.to_string(), |p| p.display().to_string())
}

/// The compose services (normalized: absolute paths, structured ports) over the Dockerfile scan: a service that
/// builds a scanned Dockerfile takes the compose name. The compose file names the services: a Dockerfile that no
/// service builds is left out, so `images run` starts nothing the repository did not declare.
fn merge_compose(scan: &mut Scan, repo: &Path, config: &serde_json::Value) {
    let Some(services) = config["services"].as_object() else {
        return;
    };
    let mut scanned = std::mem::take(&mut scan.services);
    let mut scanned_cache = std::mem::take(&mut scan.cache);
    for (name, svc) in services {
        let mut service = Service::default();
        if let Some(build) = svc.get("build") {
            let context = relative(repo, build["context"].as_str().unwrap_or("."));
            let dockerfile = Path::new(&context)
                .join(build["dockerfile"].as_str().unwrap_or("Dockerfile"))
                .display()
                .to_string();
            let found = scanned.iter().find_map(|(n, s)| {
                (s.build.as_ref().map(|b| &b.dockerfile) == Some(&dockerfile)).then(|| n.clone())
            });
            if let Some(old) = found {
                service = scanned.remove(&old).unwrap_or_default();
                if let Some(cache) = scanned_cache.remove(&old) {
                    scan.cache.insert(name.clone(), cache);
                }
            }
            let target = build["target"].as_str().map(String::from);
            let scanned = service.build.take().and_then(|b| b.target);
            service.build = Some(Build {
                context,
                dockerfile,
                target: target.or(scanned),
            });
        } else {
            service.image = svc["image"].as_str().map(String::from);
        }
        compose_details(&mut service, repo, svc);
        scan.services.insert(name.clone(), service);
    }
}

/// Ports, mounts, limits, dependencies and the health check of one normalized compose service.
fn compose_details(service: &mut Service, repo: &Path, svc: &serde_json::Value) {
    let mut ports: Vec<String> = svc["ports"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|p| {
            let port = match p["protocol"].as_str() {
                Some("udp") => format!("{}/udp", p["target"]),
                _ => p["target"].to_string(),
            };
            match p["published"].as_str() {
                Some(published) => format!("{published}:{port}"),
                None => port,
            }
        })
        .collect();
    ports.extend(
        svc["expose"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|e| e.as_str().map(String::from)),
    );
    if !ports.is_empty() {
        service.ports = ports;
    }
    service.mounts = svc["volumes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|v| Mount {
            kind: v["type"].as_str().unwrap_or_default().into(),
            source: relative(repo, v["source"].as_str().unwrap_or_default()),
            target: v["target"].as_str().unwrap_or_default().into(),
            read_only: v["read_only"] == true,
        })
        .collect();
    let resources = &svc["deploy"]["resources"];
    let limits = &resources["limits"];
    let number = |v: &serde_json::Value| {
        v.as_f64()
            .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
    };
    let gpu = resources["reservations"]["devices"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|d| {
            d["capabilities"]
                .as_array()
                .is_some_and(|c| c.iter().any(|c| c == "gpu"))
        });
    let memory = limits["memory"]
        .as_u64()
        .or_else(|| limits["memory"].as_str().and_then(|s| s.parse().ok()));
    let cpus = number(&limits["cpus"]);
    if cpus.is_some() || memory.is_some() || gpu {
        service.resources = Some(Resources {
            cpus,
            memory_bytes: memory,
            gpu,
        });
    }
    service.depends_on = svc["depends_on"]
        .as_object()
        .map(|d| d.keys().cloned().collect())
        .unwrap_or_default();
    if let Some(test) = svc["healthcheck"]["test"].as_array() {
        let test: Vec<String> = test
            .iter()
            .filter_map(|t| t.as_str().map(String::from))
            .collect();
        match test.split_first() {
            Some((kind, rest)) if kind == "CMD" => {
                service.entrypoints.insert("health".into(), rest.to_vec());
            }
            Some((kind, rest)) if kind == "CMD-SHELL" => {
                service
                    .entrypoints
                    .insert("health".into(), command(&rest.join(" ")));
            }
            _ => {
                service.entrypoints.remove("health");
            }
        }
    }
}

/// The project name of a repository: its folder name, lower case, with each other character as `-`.
#[must_use]
pub fn project_name(repo: &Path) -> String {
    repo.file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect()
}

/// The image tag of a repository: its short commit, or `dev` for a folder outside git.
///
/// # Errors
/// The folder is in git and `git rev-parse --short HEAD` fails (no commit yet, or git does not run).
pub fn tag(repo: &Path) -> Result<String> {
    if !repo.ancestors().any(|d| d.join(".git").exists()) {
        return Ok("dev".into());
    }
    crate::git::text(repo, &["rev-parse", "--short", "HEAD"])
}

/// The image reference of `service`: `localhost/<project>/<service>:<tag>`.
#[must_use]
pub fn image(project: &str, service: &str, tag: &str) -> String {
    format!("localhost/{project}/{service}:{tag}")
}

/// The compose file legionctl runs (JSON, which compose reads as YAML), an output never edited by hand. Without a
/// compose file of the repository: each service's image, build, exposed ports and health check. With one, it is an
/// override passed after the repository's file: only the tag of each built service, so compose keeps the rest
/// (environment, commands, volumes) as the repository says.
#[must_use]
pub fn compose(scan: &Scan, project: &str, tag: &str) -> serde_json::Value {
    let services: serde_json::Map<String, serde_json::Value> = scan
        .services
        .iter()
        .filter_map(|(name, s)| {
            let build = s.build.as_ref()?;
            let mut service = serde_json::json!({ "image": image(project, name, tag) });
            if scan.compose_file.is_some() {
                return Some((name.clone(), service));
            }
            service["build"] =
                serde_json::json!({"context": build.context, "dockerfile": build.dockerfile});
            if let Some(target) = &build.target {
                service["build"]["target"] = target.clone().into();
            }
            if !s.ports.is_empty() {
                service["expose"] = s.ports.clone().into();
            }
            if let Some(health) = s.entrypoints.get("health") {
                let test: Vec<String> = std::iter::once("CMD".to_string())
                    .chain(health.iter().cloned())
                    .collect();
                service["healthcheck"] = serde_json::json!({ "test": test });
            }
            Some((name.clone(), service))
        })
        .collect();
    serde_json::json!({ "name": project, "services": services })
}
