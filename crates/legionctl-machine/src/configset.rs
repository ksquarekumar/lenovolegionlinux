//! Configsets (note 01): the desired state is a merge of JSONC layers, `<order>-<name>.jsonc` =
//! `{ "when": {...}, "set": { "<knob id>": value } }`. Folders, lowest first: the shipped layers of the profile, the
//! generated ones (`tuned`, off until enabled), `/etc` (system defaults, which can lock knobs), and the layers of the
//! user of the active local session. A later layer wins per knob; the user's holds (D1) stay on top of the merge.
//!
//! Each layer is checked when it is read; a layer that fails is not used, and the reason is a `doctor` finding.

use std::collections::{BTreeMap, BTreeSet};
use std::io::Read;
use std::path::Path;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::ledger::Ledger;
use crate::policy::Source;
use crate::profile::Profile;
use crate::sysroot::SysRoot;

/// The shipped layers, in a folder per profile id.
pub const SHIPPED: &str = "/usr/share/legionctl/configsets";
/// Layers that legionctl makes (`tuned` from `legionctl tune`); each is off until `legionctl rules enable`.
pub const GENERATED: &str = "/var/lib/legionctl/configsets";
/// The system defaults; written by the daemon (polkit `org.legionctl.rules`).
pub const SYSTEM: &str = "/etc/legionctl/configsets";
/// The user's own layers, under the home of the user of the active local session.
pub const USER: &str = ".config/legionctl/configsets";
/// logind's state of the local seat: `ACTIVE_UID=` is the user of the session in front.
const SEAT: &str = "/run/systemd/seats/seat0";
/// The workloads that run now, one per line; written by the daemon's workload sensor.
pub const WORKLOAD: &str = "/run/legionctl/workload";
/// Why the workload sensor does not run; written by the daemon instead of [`WORKLOAD`], read by `doctor`.
pub const WORKLOAD_ERROR: &str = "/run/legionctl/workload.error";
/// The kinds of workload the workload sensor finds.
pub const WORKLOADS: [&str; 3] = ["game", "build", "vm"];
/// A layer is a few lines; a larger file is not read.
const MAX_BYTES: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Folder {
    Shipped,
    Generated,
    System,
    User,
}

impl Folder {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Folder::Shipped => "shipped",
            Folder::Generated => "generated",
            Folder::System => "system",
            Folder::User => "user",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Layer {
    /// `<order>-<name>`: the file name without `.jsonc`.
    pub name: String,
    pub folder: Folder,
    /// The file (machine path).
    pub file: String,
    /// The conditions: `power` (ac, battery), `fnq` (the firmware mode chosen with Fn+Q), `workload` (game, build,
    /// vm), `kernel` (the running kernel's package). Empty: always.
    pub when: BTreeMap<String, String>,
    /// Knob id -> value, in file order.
    pub set: Map<String, Value>,
    /// Knobs that user layers cannot set (`/etc` layers only).
    pub locked: Vec<String>,
    /// Why the layer is not used; `None`: it is used when its conditions hold.
    pub off: Option<String>,
    /// It is not used because it fails a check (a `doctor` finding), not because it is disabled or not enabled.
    pub failed: bool,
}

impl Layer {
    /// `<folder>/<name>`, unique among the layers.
    #[must_use]
    pub fn id(&self) -> String {
        format!("{}/{}", self.folder.name(), self.name)
    }

    /// Whether `name` names this layer: its id, its file name, or the name without the order (`tuned`).
    #[must_use]
    pub fn is(&self, name: &str) -> bool {
        self.id() == name
            || self.name == name
            || self.name.split_once('-').is_some_and(|(_, n)| n == name)
    }

    /// Whether its conditions hold under `c`.
    #[must_use]
    pub fn applies(&self, c: &Conditions) -> bool {
        self.when.iter().all(|(key, value)| match key.as_str() {
            "power" => value == c.power.name(),
            "fnq" => c.fnq.as_deref() == Some(value.as_str()),
            "workload" => c.workload.contains(value),
            "kernel" => c.kernel.as_deref() == Some(value.as_str()),
            _ => false,
        })
    }
}

/// The one layer that `name` names ([`Layer::is`]).
///
/// # Errors
/// No layer, or more than one, has that name.
pub fn find<'a>(layers: &'a [Layer], name: &str) -> Result<&'a Layer, String> {
    let found: Vec<&Layer> = layers.iter().filter(|l| l.is(name)).collect();
    match found[..] {
        [layer] => Ok(layer),
        [] => Err(format!("no layer {name} (see legionctl rules list)")),
        _ => Err(format!(
            "{name} names {} layers ({}); give one id",
            found.len(),
            found.iter().map(|l| l.id()).collect::<Vec<_>>().join(", ")
        )),
    }
}

/// `when` as text: `power ac, fnq balanced`, or `always`.
#[must_use]
pub fn when_text(when: &BTreeMap<String, String>) -> String {
    if when.is_empty() {
        return "always".into();
    }
    when.iter()
        .map(|(k, v)| format!("{k} {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// What the layers' conditions are tested against.
#[derive(Debug, Clone)]
pub struct Conditions {
    pub power: Source,
    /// The Fn+Q choice that the policy follows now.
    pub fnq: Option<String>,
    pub workload: BTreeSet<String>,
    /// The package of the running kernel (`linux-cachyos-bore-lto`).
    pub kernel: Option<String>,
}

/// One layer that set a knob, for the "because" chain.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Link {
    /// The layer (`shipped/20-ac`).
    pub layer: String,
    /// Its conditions as text (`power ac`, `always`).
    pub when: String,
    pub value: Value,
}

/// The merged desired state: knob id -> value (in the order the knobs were first set) and, per knob, every layer that
/// set it; the last one wins.
#[derive(Debug, Clone, Default)]
pub struct Merged {
    pub values: Map<String, Value>,
    pub chain: BTreeMap<String, Vec<Link>>,
}

impl Merged {
    /// The value of `id` as text for a file (strings as they are, numbers in decimal).
    #[must_use]
    pub fn text(&self, id: &str) -> Option<String> {
        self.values.get(id).map(crate::profile::text)
    }

    /// The knobs that start with `prefix` (`sysctl.`), without it, and their values as text.
    #[must_use]
    pub fn under(&self, prefix: &str) -> Vec<(String, String)> {
        self.values
            .iter()
            .filter_map(|(id, v)| {
                Some((
                    id.strip_prefix(prefix)?.to_string(),
                    crate::profile::text(v),
                ))
            })
            .collect()
    }
}

/// Merge the used layers whose conditions hold, in order.
#[must_use]
pub fn merge(layers: &[Layer], c: &Conditions) -> Merged {
    let mut merged = Merged::default();
    for layer in layers.iter().filter(|l| l.off.is_none() && l.applies(c)) {
        for (id, value) in &layer.set {
            merged.values.insert(id.clone(), value.clone());
            merged.chain.entry(id.clone()).or_default().push(Link {
                layer: layer.id(),
                when: when_text(&layer.when),
                value: value.clone(),
            });
        }
    }
    merged
}

/// The workloads that run now (the daemon's exec sensor writes them).
#[must_use]
pub fn workload(root: &SysRoot) -> BTreeSet<String> {
    root.read(WORKLOAD)
        .unwrap_or_default()
        .split_whitespace()
        .map(String::from)
        .collect()
}

/// The user of the active local session (logind seat0) and their home, if there is one.
#[must_use]
pub fn active_user(root: &SysRoot) -> Option<(u32, String)> {
    let uid: u32 = root
        .read(SEAT)?
        .lines()
        .find_map(|l| l.strip_prefix("ACTIVE_UID="))?
        .parse()
        .ok()?;
    let passwd = root.read("/etc/passwd")?;
    let home = passwd.lines().find_map(|l| {
        let f: Vec<&str> = l.split(':').collect();
        (f.get(2)? == &uid.to_string().as_str()).then(|| f.get(5).map(|h| (*h).to_string()))?
    })?;
    Some((uid, home))
}

/// The user's layer folder (machine path).
#[must_use]
pub fn user_folder(home: &str) -> String {
    format!("{}/{USER}", home.trim_end_matches('/'))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    when: BTreeMap<String, String>,
    #[serde(default)]
    set: Map<String, Value>,
    #[serde(default)]
    locked: Vec<String>,
}

/// The `.jsonc` files of `dir`, by name: each one's text, or why it was not read. A link is never followed (neither the
/// folder nor a file in it); with `owner`, the folder and each file must belong to that uid.
fn read_folder(dir: &Path, owner: Option<u32>) -> Vec<(String, Result<String, String>)> {
    use rustix::fs::{FileType, Mode, OFlags};
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::CLOEXEC;
    let Ok(fd) = rustix::fs::open(dir, flags | OFlags::DIRECTORY, Mode::empty()) else {
        return Vec::new();
    };
    if let Some(uid) = owner
        && !rustix::fs::fstat(&fd).is_ok_and(|s| s.st_uid == uid)
    {
        return vec![(
            dir.display().to_string(),
            Err(format!("the folder does not belong to uid {uid}")),
        )];
    }
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| Path::new(n).extension().is_some_and(|e| e == "jsonc"))
        .collect();
    names.sort();
    names
        .into_iter()
        .map(|name| {
            let text = (|| {
                let file =
                    rustix::fs::openat(&fd, name.as_str(), flags | OFlags::NONBLOCK, Mode::empty())
                        .map_err(|e| format!("cannot open it (a link is not read): {e}"))?;
                let stat = rustix::fs::fstat(&file).map_err(|e| e.to_string())?;
                if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
                    return Err("it is not a plain file".to_string());
                }
                if let Some(uid) = owner
                    && stat.st_uid != uid
                {
                    return Err(format!("it does not belong to uid {uid}"));
                }
                if u64::try_from(stat.st_size).unwrap_or(u64::MAX) > MAX_BYTES {
                    return Err(format!("it is larger than {MAX_BYTES} bytes"));
                }
                let mut text = String::new();
                std::fs::File::from(file)
                    .take(MAX_BYTES)
                    .read_to_string(&mut text)
                    .map_err(|e| e.to_string())?;
                Ok(text)
            })();
            (name, text)
        })
        .collect()
}

/// `<two digits>-<a-z, 0-9 and ->`: a layer name, which is also safe as a file name.
#[must_use]
pub fn good_name(name: &str) -> bool {
    name.split_once('-').is_some_and(|(order, rest)| {
        order.len() == 2
            && order.bytes().all(|b| b.is_ascii_digit())
            && !rest.is_empty()
            && rest
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// A layer's own faults: its name, conditions and values. `None` when it is good.
fn check(
    root: Option<&SysRoot>,
    profile: &Profile,
    folder: Folder,
    name: &str,
    file: &File,
) -> Option<String> {
    let mut faults = Vec::new();
    if !good_name(name) {
        faults.push("the name must be <two digits>-<a-z, 0-9 and ->.jsonc".into());
    }
    for (key, value) in &file.when {
        let valid = match key.as_str() {
            "power" => ["ac", "battery"].contains(&value.as_str()),
            "fnq" => !value.is_empty(),
            "workload" => WORKLOADS.contains(&value.as_str()),
            "kernel" => {
                // without a machine (`validate`), the installed kernels are not known
                let installed = root.map(crate::kernel::installed_packages);
                if installed.as_ref().is_some_and(|k| !k.contains(value)) {
                    faults.push(format!(
                        "condition kernel: no installed kernel {value} (installed: {})",
                        installed.unwrap_or_default().join(", ")
                    ));
                    continue;
                }
                !value.is_empty()
            }
            _ => {
                faults.push(format!(
                    "unknown condition {key} (power, fnq, workload, kernel)"
                ));
                continue;
            }
        };
        if !valid {
            faults.push(format!("condition {key}: {value} is not a valid value"));
        }
    }
    if !file.locked.is_empty() && folder != Folder::System {
        faults.push("only a layer in /etc/legionctl/configsets can lock knobs".into());
    }
    for (id, value) in &file.set {
        if let Err(e) = crate::knobs::check(root, profile, id, value) {
            faults.push(e);
        }
    }
    (!faults.is_empty()).then(|| faults.join("; "))
}

/// The layers in `dir` (machine path `shown`), each with its own faults; `root` is the machine to check the values
/// against.
fn read_layers(
    root: Option<&SysRoot>,
    profile: &Profile,
    folder: Folder,
    dir: &Path,
    shown: &str,
    owner: Option<u32>,
) -> Vec<Layer> {
    read_folder(dir, owner)
        .into_iter()
        .map(|(name, text)| {
            let stem = name.trim_end_matches(".jsonc");
            parse(
                root,
                profile,
                folder,
                stem,
                &format!("{shown}/{name}"),
                text,
            )
        })
        .collect()
}

/// The layer `name` (`<order>-<name>`, the file `file`) from its text (or why it could not be read), with its own
/// faults in `off`; the rules between layers are checked at [`load`].
#[must_use]
pub fn parse(
    root: Option<&SysRoot>,
    profile: &Profile,
    folder: Folder,
    name: &str,
    file: &str,
    text: Result<String, String>,
) -> Layer {
    let parsed = text.and_then(|t| {
        serde_json::from_reader::<_, File>(json_comments::StripComments::new(t.as_bytes()))
            .map_err(|e| e.to_string())
    });
    let (content, off) = match parsed {
        Ok(content) => {
            let off = check(root, profile, folder, name, &content);
            (content, off)
        }
        Err(e) => (
            File {
                when: BTreeMap::new(),
                set: Map::new(),
                locked: Vec::new(),
            },
            Some(e),
        ),
    };
    Layer {
        file: file.to_string(),
        name: name.to_string(),
        folder,
        when: content.when,
        set: content.set,
        locked: content.locked,
        failed: off.is_some(),
        off,
    }
}

/// A layer as the JSONC text of its file.
#[must_use]
pub fn content(layer: &Layer) -> String {
    let mut value = serde_json::json!({ "when": layer.when, "set": layer.set });
    if !layer.locked.is_empty() {
        value["locked"] = serde_json::json!(layer.locked);
    }
    serde_json::to_string_pretty(&value).unwrap_or_default() + "\n"
}

/// The faults of the shipped layers in `dir`, without a machine (`legionctl validate`): each layer's own, and the
/// rules between them.
#[must_use]
pub fn validate(profile: &Profile, dir: &Path) -> Vec<String> {
    let shown = dir.display().to_string();
    let mut layers = read_layers(None, profile, Folder::Shipped, dir, &shown, None);
    if layers.is_empty() {
        return vec![format!("no configset layers in {shown}")];
    }
    across(&mut layers);
    findings(&layers)
}

/// Every layer of `profile` on this machine, in merge order, each checked: one that fails, that is disabled
/// (`rules disable`) or that is generated and not enabled has the reason in `off`.
#[must_use]
pub fn load(root: &SysRoot, profile: &Profile, ledger: &Ledger) -> Vec<Layer> {
    let mut folders = vec![
        (Folder::Shipped, format!("{SHIPPED}/{}", profile.id), None),
        (Folder::Generated, GENERATED.to_string(), None),
        (Folder::System, SYSTEM.to_string(), None),
    ];
    if let Some((uid, home)) = active_user(root) {
        folders.push((Folder::User, user_folder(&home), Some(uid)));
    }
    let mut layers = Vec::new();
    for (folder, dir, owner) in folders {
        layers.extend(read_layers(
            Some(root),
            profile,
            folder,
            &root.path(&dir),
            &dir,
            owner,
        ));
    }
    for layer in &mut layers {
        if layer.off.is_some() {
            continue;
        }
        layer.off = match (ledger.layer_enabled(&layer.id()), layer.folder) {
            (Some(false), _) => Some("disabled (legionctl rules enable turns it on)".into()),
            (None, Folder::Generated) => Some(format!(
                "generated: off until `legionctl rules enable {}`",
                layer.name
            )),
            _ => None,
        };
    }
    across(&mut layers);
    layers
}

/// The rules between layers, until no more layer drops out: a user layer cannot set a knob that a used `/etc` layer
/// locks, and a knob that a `power` layer sets is set for the other power source too, or by a layer without `power`
/// (else a change of the power source leaves the other source's value behind).
fn across(layers: &mut [Layer]) {
    loop {
        let used: Vec<&Layer> = layers.iter().filter(|l| l.off.is_none()).collect();
        let locked: BTreeMap<&str, String> = used
            .iter()
            .filter(|l| l.folder == Folder::System)
            .flat_map(|l| l.locked.iter().map(move |k| (k.as_str(), l.id())))
            .collect();
        let set_for = |power: Option<&str>, knob: &str| {
            used.iter().any(|l| {
                l.when.get("power").map(String::as_str) == power && l.set.contains_key(knob)
            })
        };
        let mut faults: Vec<(usize, String)> = Vec::new();
        for (i, layer) in layers.iter().enumerate().filter(|(_, l)| l.off.is_none()) {
            let mut why = Vec::new();
            for knob in layer.set.keys() {
                if layer.folder == Folder::User
                    && let Some(by) = locked.get(knob.as_str())
                {
                    why.push(format!("{knob} is locked by {by}"));
                }
                if let Some(power) = layer.when.get("power") {
                    let other = if power == "ac" { "battery" } else { "ac" };
                    if !set_for(Some(other), knob) && !set_for(None, knob) {
                        why.push(format!(
                            "{knob} is set for {power} only; set it for {other} too, or in a layer without power"
                        ));
                    }
                }
            }
            if !why.is_empty() {
                faults.push((i, why.join("; ")));
            }
        }
        if faults.is_empty() {
            return;
        }
        for (i, why) in faults {
            layers[i].off = Some(why);
            layers[i].failed = true;
        }
    }
}

/// The layers that are not used because they fail a check, as `doctor` names them.
#[must_use]
pub fn findings(layers: &[Layer]) -> Vec<String> {
    layers
        .iter()
        .filter_map(|l| {
            let why = l.off.as_ref().filter(|_| l.failed)?;
            Some(format!("configset layer {} is not used: {why}", l.file))
        })
        .collect()
}
