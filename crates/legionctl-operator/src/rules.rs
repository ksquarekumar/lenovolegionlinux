//! `legionctl rules` (note 01, configsets): the user's own layers are the user's own files
//! (`~/.config/legionctl/configsets/`); system layers and the layers' on/off states go through `legionctl-system`
//! (polkit `org.legionctl.rules`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use legionctl_machine::configset::{self, Folder, Layer};
use legionctl_machine::policy::Machine;
use legionctl_machine::profile::Profile;
use legionctl_machine::sysroot::SysRoot;
use serde_json::Value;

/// This user's layer folder.
pub fn own_folder(home: &Path) -> PathBuf {
    home.join(configset::USER)
}

/// The machine path of layer `name` in `folder` (the user's own folder is a real path: this process's home).
pub fn file_of(folder: Folder, home: &Path, name: &str) -> String {
    match folder {
        Folder::User => own_folder(home)
            .join(format!("{name}.jsonc"))
            .display()
            .to_string(),
        _ => format!("{}/{name}.jsonc", configset::SYSTEM),
    }
}

/// The text of `file` (machine path for a system layer, real path for the user's), or why it cannot be read.
fn read(root: &SysRoot, folder: Folder, file: &str) -> Option<Result<String, String>> {
    let path = match folder {
        Folder::User => PathBuf::from(file),
        _ => root.path(file),
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => Some(Ok(text)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => Some(Err(e.to_string())),
    }
}

/// The own layer `name` (user or system) with `set` (`knob=value`, `knob=` removes) and `when` applied; a new layer
/// takes the conditions of the shipped layer of the same name. Refused when the result fails its checks.
///
/// # Errors
/// A malformed `set` or `when`, an own layer that cannot be read, or a layer that fails its checks.
pub fn edited(
    m: &Machine,
    folder: Folder,
    home: &Path,
    name: &str,
    set: &[String],
    when: Option<&str>,
) -> Result<Layer> {
    anyhow::ensure!(
        configset::good_name(name),
        "{name}: a layer name is <two digits>-<a-z, 0-9 and -> (legionctl rules list)"
    );
    let (root, profile) = (m.root, m.profile);
    let file = file_of(folder, home, name);
    let mut layer = match read(root, folder, &file) {
        Some(text) => configset::parse(Some(root), profile, folder, name, &file, text),
        None => Layer {
            name: name.to_string(),
            folder,
            file: file.clone(),
            when: m
                .layers
                .iter()
                .find(|l| l.folder == Folder::Shipped && l.name == name)
                .map(|l| l.when.clone())
                .unwrap_or_default(),
            set: serde_json::Map::new(),
            locked: Vec::new(),
            off: None,
            failed: false,
        },
    };
    for pair in set {
        let (knob, value) = pair
            .split_once('=')
            .with_context(|| format!("{pair}: give knob=value (knob= removes it)"))?;
        if value.is_empty() {
            layer.set.remove(knob);
        } else {
            // a list (platform_profile's preference) is JSON; everything else is text
            let value = if value.starts_with('[') {
                serde_json::from_str(value).with_context(|| format!("{pair}: not a JSON list"))?
            } else {
                Value::String(value.to_string())
            };
            layer.set.insert(knob.to_string(), value);
        }
    }
    if let Some(when) = when {
        layer.when.clear();
        for pair in when.split(',').filter(|p| !p.is_empty() && *p != "always") {
            let (key, value) = pair
                .split_once('=')
                .with_context(|| format!("{pair}: give condition=value (power=ac)"))?;
            layer.when.insert(key.to_string(), value.to_string());
        }
    }
    let checked = configset::parse(
        Some(root),
        profile,
        folder,
        name,
        &file,
        Ok(configset::content(&layer)),
    );
    if let Some(why) = checked.off {
        anyhow::bail!("{file}: {why}");
    }
    Ok(checked)
}

/// Write the user's own layer, or remove it when it sets nothing. Returns what was done.
///
/// # Errors
/// The file cannot be written or removed.
pub fn write_own(layer: &Layer) -> Result<String> {
    let path = Path::new(&layer.file);
    if layer.set.is_empty() {
        return match std::fs::remove_file(path) {
            Ok(()) => Ok(format!("removed {}", layer.file)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                Ok(format!("{} sets nothing", layer.file))
            }
            Err(e) => Err(e).with_context(|| format!("removing {}", layer.file)),
        };
    }
    std::fs::create_dir_all(path.parent().context("no parent")?)?;
    std::fs::write(path, configset::content(layer))
        .with_context(|| format!("writing {}", layer.file))?;
    Ok(format!("wrote {}", layer.file))
}

/// The user's own layers: each file's layer, as `rules reset` changes them.
///
/// # Errors
/// The folder is there but cannot be listed.
pub fn own_layers(root: &SysRoot, profile: &Profile, home: &Path) -> Result<Vec<Layer>> {
    let folder = own_folder(home);
    let entries = match std::fs::read_dir(&folder) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        entries => entries
            .and_then(Iterator::collect::<std::io::Result<Vec<_>>>)
            .with_context(|| format!("listing {}", folder.display()))?,
    };
    let mut names: Vec<String> = entries
        .iter()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|n| n.strip_suffix(".jsonc").map(String::from))
        .collect();
    names.sort();
    Ok(names
        .into_iter()
        .map(|name| {
            let file = file_of(Folder::User, home, &name);
            // a file that went since the listing is an empty layer; one that cannot be read is the layer's fault
            let text = read(root, Folder::User, &file).unwrap_or(Ok(String::new()));
            configset::parse(Some(root), profile, Folder::User, &name, &file, text)
        })
        .collect())
}
