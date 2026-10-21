//! The one writer of a settings layer file (`managed.jsonc` of /etc/legionctl, ~/.config/legionctl or a repository's
//! .legionctl): a read-modify-write of one key under an exclusive lock of the layer folder, through a temporary file
//! of its own and a rename, so two writers never lose each other's change (review/16 B10) and a reader never sees half
//! a file. The operator writes the user and project layers; the system service writes the computer layer.

use std::io::Write as _;
use std::path::Path;

use anyhow::{Context, Result, bail};
use serde_json::{Map, Value};

/// The layer file that legionctl writes in each layer folder; it merges after the other files of its folder.
pub const MANAGED: &str = "managed.jsonc";

const HEADER: &str = "// Written by legionctl (`legionctl config set`, Settings). Change it there, or move keys into your own file\n\
                      // in this folder; this file merges after your files.\n";

/// Change `key` (dotted) in `<dir>/managed.jsonc`: `change` gets the value it has there (`None`: none) under the lock
/// and gives the new one (`None`: remove it there; emptied objects go too), so it builds on what is there when it is
/// written. The value it had there before. A key that is not there and stays so writes nothing.
///
/// # Errors
/// The folder cannot be made or locked, the file cannot be read, parsed or written, or `change` fails.
pub fn edit(
    dir: &Path,
    key: &str,
    change: impl FnOnce(Option<&Value>) -> Result<Option<Value>>,
) -> Result<Option<Value>> {
    let path = dir.join(MANAGED);
    update(&path, |bytes| {
        let mut managed = match bytes {
            Some(bytes) => match crate::profile::parse_jsonc(bytes)
                .with_context(|| format!("parsing {}", path.display()))?
            {
                Value::Object(map) => map,
                _ => bail!("{} is not a JSON object", path.display()),
            },
            None => Map::new(),
        };
        let parts: Vec<&str> = key.split('.').collect();
        let old = parts
            .split_first()
            .and_then(|(first, rest)| {
                rest.iter()
                    .try_fold(managed.get(*first)?, |at, part| at.get(*part))
            })
            .cloned();
        let value = change(old.as_ref())?;
        if old.is_none() && value.is_none() {
            return Ok((None, old));
        }
        put(&mut managed, &parts, value);
        let text = format!(
            "{HEADER}{}\n",
            serde_json::to_string_pretty(&Value::Object(managed))?
        );
        Ok((Some(text.into_bytes()), old))
    })
}

/// Change the file `path` from its bytes (none: no file yet) to the bytes that `change` gives (none: leave it), under
/// an exclusive lock of its folder, through a temporary file of its own and a rename: concurrent changes never lose
/// each other (review/16 B10), and a reader never sees half a file. What `change` gives besides.
///
/// # Errors
/// The folder cannot be made or locked, the file cannot be read or written, or `change` fails.
pub fn update<T>(
    path: &Path,
    change: impl FnOnce(Option<&[u8]>) -> Result<(Option<Vec<u8>>, T)>,
) -> Result<T> {
    let dir = path.parent().context("a file has a folder")?;
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    let folder = std::fs::File::open(dir).with_context(|| format!("opening {}", dir.display()))?;
    rustix::fs::flock(&folder, rustix::fs::FlockOperation::LockExclusive)
        .with_context(|| format!("locking {}", dir.display()))?;
    let old = match std::fs::read(path) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let (bytes, out) = change(old.as_deref())?;
    let Some(bytes) = bytes else {
        return Ok(out);
    };
    let name = path
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let tmp = dir.join(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let written = (|| -> Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)?;
        folder.sync_all()?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.with_context(|| format!("writing {}", path.display()))?;
    Ok(out)
}

/// Put `value` at `parts` in `map` (or remove it; an object left empty goes too).
fn put(map: &mut Map<String, Value>, parts: &[&str], value: Option<Value>) {
    let Some((first, rest)) = parts.split_first() else {
        return;
    };
    if rest.is_empty() {
        match value {
            Some(v) => map.insert((*first).to_string(), v),
            None => map.remove(*first),
        };
        return;
    }
    if value.is_none() && !map.get(*first).is_some_and(Value::is_object) {
        return;
    }
    let next = map
        .entry((*first).to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !next.is_object() {
        *next = Value::Object(Map::new());
    }
    let Value::Object(inner) = next else {
        return;
    };
    put(inner, rest, value);
    if inner.is_empty() {
        map.remove(*first);
    }
}
