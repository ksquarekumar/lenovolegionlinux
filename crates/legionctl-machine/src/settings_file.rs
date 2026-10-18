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

/// Set `key` (dotted) in `<dir>/managed.jsonc` to `value`, or remove it there (`None`; emptied objects go too); the
/// value it had there before (null: none).
///
/// # Errors
/// The folder cannot be made or locked, or the file cannot be read, parsed or written.
pub fn edit(dir: &Path, key: &str, value: Option<Value>) -> Result<Value> {
    std::fs::create_dir_all(dir).with_context(|| format!("making {}", dir.display()))?;
    let folder = std::fs::File::open(dir).with_context(|| format!("opening {}", dir.display()))?;
    rustix::fs::flock(&folder, rustix::fs::FlockOperation::LockExclusive)
        .with_context(|| format!("locking {}", dir.display()))?;
    let path = dir.join(MANAGED);
    let mut managed = match std::fs::read(&path) {
        Ok(bytes) => {
            match serde_json::from_reader(json_comments::StripComments::new(bytes.as_slice()))
                .with_context(|| format!("parsing {}", path.display()))?
            {
                Value::Object(map) => map,
                _ => bail!("{} is not a JSON object", path.display()),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Map::new(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };
    let parts: Vec<&str> = key.split('.').collect();
    let old = put(&mut managed, &parts, value);
    let tmp = dir.join(format!(
        ".{MANAGED}.{}.{}.tmp",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos())
    ));
    let written = (|| -> Result<()> {
        let mut file = std::fs::File::create(&tmp)?;
        writeln!(
            file,
            "{HEADER}{}",
            serde_json::to_string_pretty(&Value::Object(managed))?
        )?;
        file.sync_all()?;
        std::fs::rename(&tmp, &path)?;
        folder.sync_all()?;
        Ok(())
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written.with_context(|| format!("writing {}", path.display()))?;
    Ok(old)
}

/// Put `value` at `parts` in `map` (or remove it; an object left empty goes too); the old value (null: none).
fn put(map: &mut Map<String, Value>, parts: &[&str], value: Option<Value>) -> Value {
    let Some((first, rest)) = parts.split_first() else {
        return Value::Null;
    };
    if rest.is_empty() {
        return match value {
            Some(v) => map.insert((*first).to_string(), v),
            None => map.remove(*first),
        }
        .unwrap_or(Value::Null);
    }
    if value.is_none() && !map.get(*first).is_some_and(Value::is_object) {
        return Value::Null;
    }
    let next = map
        .entry((*first).to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !next.is_object() {
        *next = Value::Object(Map::new());
    }
    let Value::Object(inner) = next else {
        return Value::Null;
    };
    let old = put(inner, rest, value);
    if inner.is_empty() {
        map.remove(*first);
    }
    old
}
