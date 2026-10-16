//! What legionctl remembers about the firmware mode across its own restarts but not across a reboot: the mode it wrote
//! last, and the mode the user chose with Fn+Q. Rows live in `system.db` tagged with the boot id; rows from another
//! boot are never read, so a reboot starts from the policy again. The user's holds and fan choices last across
//! reboots.

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::sets::Selection;
use crate::sysroot::SysRoot;

pub const DB: &str = "/var/lib/legionctl/system.db";
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// The firmware mode legionctl wrote last; a change to it is our own.
    Written,
    /// The firmware mode the user chose with Fn+Q; kept until the power source changes or `apply` runs.
    Override,
    /// What a running Windows VM changed on the host (`winvm::Record` as JSON), for its end.
    WindowsVm,
    /// The NVIDIA services that `GpuRelease` stopped (space-separated), for `GpuReturn`.
    GpuServices,
    /// The `OpenRGB` profile legionctl loaded last (`rgb.profile`).
    RgbProfile,
}

impl Key {
    fn name(self) -> &'static str {
        match self {
            Key::Written => "firmware-written",
            Key::Override => "firmware-override",
            Key::WindowsVm => "windows-vm",
            Key::GpuServices => "gpu-services",
            Key::RgbProfile => "rgb-profile",
        }
    }
}

/// The user's fan choices; unlike [`Key`] they last across reboots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FanKey {
    /// The user's fan curve, levels separated by spaces.
    Curve,
    /// Fan max mode is on.
    Max,
}

impl FanKey {
    fn name(self) -> &'static str {
        match self {
            FanKey::Curve => "curve",
            FanKey::Max => "max",
        }
    }
}

pub struct Ledger {
    /// `None` when a reader finds no database (the daemon never ran).
    conn: Option<Connection>,
    boot: String,
}

const SCHEMA: &str = "CREATE TABLE IF NOT EXISTS ledger (
                        key     TEXT PRIMARY KEY,
                        value   TEXT NOT NULL,
                        boot_id TEXT NOT NULL
                     );
                     -- the user's holds (D1, decided 2026-10-13): a path the policy leaves at this value until released;
                     -- a * matches the CPU policies of a per-CPU knob
                     CREATE TABLE IF NOT EXISTS holds (
                        path  TEXT PRIMARY KEY,
                        value TEXT NOT NULL
                     );
                     -- the user's fan choices (todo-77), set again after resume and at custom-mode entry
                     CREATE TABLE IF NOT EXISTS fan (
                        key   TEXT PRIMARY KEY,
                        value TEXT NOT NULL
                     );
                     -- configset layers turned on or off (`legionctl rules enable|disable`, todo-74), by id
                     CREATE TABLE IF NOT EXISTS layers (
                        id      TEXT PRIMARY KEY,
                        enabled INTEGER NOT NULL
                     );
                     -- saved sets (`legionctl rules sets`, todo-85): a copy of the layers and holds tables as JSON
                     CREATE TABLE IF NOT EXISTS sets (
                        name      TEXT PRIMARY KEY,
                        selection TEXT NOT NULL
                     );";

impl Ledger {
    fn boot(root: &SysRoot) -> Result<String> {
        root.read(BOOT_ID)?
            .context("reading the boot id: no such file")
    }

    /// Open for writing: the daemon, the one writer (13-daemon). It makes the database when there is none.
    ///
    /// # Errors
    /// The boot id is not readable, or `system.db` cannot be opened for writing (locked, corrupt, no permission).
    pub fn open(root: &SysRoot) -> Result<Ledger> {
        let boot = Ledger::boot(root)?;
        let path = root.path(DB);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).with_context(|| format!("creating the folder of {DB}"))?;
        }
        let conn = Connection::open(&path)
            .and_then(|c| c.execute_batch(SCHEMA).map(|()| c))
            .with_context(|| format!("opening {DB} for writing"))?;
        Ok(Ledger {
            conn: Some(conn),
            boot,
        })
    }

    /// Open read-only (a user's status or dry run); without a database, the ledger is empty.
    ///
    /// # Errors
    /// The boot id is not readable, or `system.db` is there but cannot be opened.
    pub fn read(root: &SysRoot) -> Result<Ledger> {
        let path = root.path(DB);
        if !path
            .try_exists()
            .with_context(|| format!("checking {DB}"))?
        {
            return Ok(Ledger {
                conn: None,
                boot: String::new(),
            });
        }
        let conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .with_context(|| format!("opening {DB} to read"))?;
        Ok(Ledger {
            conn: Some(conn),
            boot: Ledger::boot(root)?,
        })
    }

    /// The value of `key` in this boot.
    ///
    /// # Errors
    /// `system.db` cannot be read.
    pub fn get(&self, key: Key) -> Result<Option<String>> {
        let Some(conn) = self.conn.as_ref() else {
            return Ok(None);
        };
        conn.query_row(
            "SELECT value FROM ledger WHERE key = ?1 AND boot_id = ?2",
            params![key.name(), self.boot],
            |r| r.get(0),
        )
        .optional()
        .with_context(|| format!("reading {} from {DB}", key.name()))
    }

    /// The user's holds: path (a `*` for the CPUs of a per-CPU knob) -> the value it keeps.
    ///
    /// # Errors
    /// `system.db` cannot be read.
    pub fn holds(&self) -> Result<std::collections::BTreeMap<String, String>> {
        let Some(conn) = self.conn.as_ref() else {
            return Ok(std::collections::BTreeMap::new());
        };
        conn.prepare("SELECT path, value FROM holds")
            .and_then(|mut stmt| {
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()
            })
            .with_context(|| format!("reading the holds from {DB}"))
    }

    /// Keep `path` at `value` against the policy, until released; `None` releases it.
    ///
    /// # Errors
    /// `system.db` is not open for writing, or the write fails.
    pub fn hold(&self, path: &str, value: Option<&str>) -> Result<()> {
        let conn = self.conn.as_ref().context("system.db is not available")?;
        match value {
            Some(v) => conn.execute(
                "INSERT INTO holds (path, value) VALUES (?1, ?2) ON CONFLICT(path) DO UPDATE SET value = excluded.value",
                params![path, v],
            ),
            None => conn.execute("DELETE FROM holds WHERE path = ?1", [path]),
        }?;
        Ok(())
    }

    /// Whether the configset layer `id` (`shipped/20-ac`) was turned on or off; `None`: its default.
    ///
    /// # Errors
    /// `system.db` cannot be read.
    pub fn layer_enabled(&self, id: &str) -> Result<Option<bool>> {
        let Some(conn) = self.conn.as_ref() else {
            return Ok(None);
        };
        conn.query_row("SELECT enabled FROM layers WHERE id = ?1", [id], |r| {
            r.get(0)
        })
        .optional()
        .with_context(|| format!("reading the state of layer {id} from {DB}"))
    }

    /// Turn the configset layer `id` on or off; `None` gives it its default again.
    ///
    /// # Errors
    /// `system.db` is not open for writing, or the write fails.
    pub fn set_layer(&self, id: &str, enabled: Option<bool>) -> Result<()> {
        let conn = self.conn.as_ref().context("system.db is not available")?;
        match enabled {
            Some(on) => conn.execute(
                "INSERT INTO layers (id, enabled) VALUES (?1, ?2) ON CONFLICT(id) DO UPDATE SET enabled = excluded.enabled",
                params![id, on],
            ),
            None => conn.execute("DELETE FROM layers WHERE id = ?1", [id]),
        }
        .with_context(|| format!("writing the state of layer {id} to {DB}"))?;
        Ok(())
    }

    /// The selection now: the layer states and the holds (not a set's `boot`).
    ///
    /// # Errors
    /// `system.db` cannot be read.
    pub fn selection(&self) -> Result<Selection> {
        let layers = match self.conn.as_ref() {
            None => std::collections::BTreeMap::default(),
            Some(conn) => conn
                .prepare("SELECT id, enabled FROM layers")
                .and_then(|mut stmt| {
                    stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<rusqlite::Result<_>>()
                })
                .with_context(|| format!("reading the layer states from {DB}"))?,
        };
        Ok(Selection {
            layers,
            holds: self.holds()?,
            boot: false,
        })
    }

    /// The saved sets by name.
    ///
    /// # Errors
    /// `system.db` cannot be read, or a set is not valid JSON.
    pub fn sets(&self) -> Result<std::collections::BTreeMap<String, Selection>> {
        let Some(conn) = self.conn.as_ref() else {
            return Ok(std::collections::BTreeMap::new());
        };
        conn.prepare("SELECT name, selection FROM sets")
            .and_then(|mut stmt| {
                stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()
            })
            .with_context(|| format!("reading the sets from {DB}"))?
            .into_iter()
            .map(|(name, json)| {
                let set = serde_json::from_str(&json)
                    .with_context(|| format!("the set {name} in {DB} is not valid"))?;
                Ok((name, set))
            })
            .collect()
    }

    /// Keep `selection` as the set `name`; `None` removes the set.
    ///
    /// # Errors
    /// `system.db` is not open for writing, or the write fails.
    pub fn save_set(&self, name: &str, selection: Option<&Selection>) -> Result<()> {
        let conn = self.conn.as_ref().context("system.db is not available")?;
        match selection {
            Some(s) => conn.execute(
                "INSERT INTO sets (name, selection) VALUES (?1, ?2) ON CONFLICT(name) DO UPDATE SET selection = excluded.selection",
                params![name, serde_json::to_string(s)?],
            ),
            None => conn.execute("DELETE FROM sets WHERE name = ?1", [name]),
        }
        .with_context(|| format!("writing the set {name} to {DB}"))?;
        Ok(())
    }

    /// The layer states become those of `selection`, in one transaction (the holds change through the policy's
    /// steps, which write the files too).
    ///
    /// # Errors
    /// `system.db` is not open for writing, or the write fails.
    pub fn set_layers(&self, selection: &Selection) -> Result<()> {
        let conn = self.conn.as_ref().context("system.db is not available")?;
        let tx = conn.unchecked_transaction()?;
        tx.execute("DELETE FROM layers", [])?;
        for (id, on) in &selection.layers {
            tx.execute(
                "INSERT INTO layers (id, enabled) VALUES (?1, ?2)",
                params![id, on],
            )?;
        }
        tx.commit()
            .with_context(|| format!("writing the layer states to {DB}"))?;
        Ok(())
    }

    /// The user's fan choice `key`.
    ///
    /// # Errors
    /// `system.db` cannot be read.
    pub fn fan(&self, key: FanKey) -> Result<Option<String>> {
        let Some(conn) = self.conn.as_ref() else {
            return Ok(None);
        };
        conn.query_row("SELECT value FROM fan WHERE key = ?1", [key.name()], |r| {
            r.get(0)
        })
        .optional()
        .with_context(|| format!("reading the fan {} from {DB}", key.name()))
    }

    /// `None` removes the choice.
    ///
    /// # Errors
    /// `system.db` is not open for writing, or the write fails.
    pub fn set_fan(&self, key: FanKey, value: Option<&str>) -> Result<()> {
        let conn = self.conn.as_ref().context("system.db is not available")?;
        match value {
            Some(v) => conn.execute(
                "INSERT INTO fan (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key.name(), v],
            ),
            None => conn.execute("DELETE FROM fan WHERE key = ?1", [key.name()]),
        }
        .with_context(|| format!("writing the fan {} to {DB}", key.name()))?;
        Ok(())
    }

    /// `None` removes the value.
    ///
    /// # Errors
    /// `system.db` is not open for writing, or the write fails.
    pub fn set(&self, key: Key, value: Option<&str>) -> Result<()> {
        let conn = self.conn.as_ref().context("system.db is not available")?;
        match value {
            Some(v) => conn.execute(
                "INSERT INTO ledger (key, value, boot_id) VALUES (?1, ?2, ?3)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value, boot_id = excluded.boot_id",
                params![key.name(), v, self.boot],
            ),
            None => conn.execute("DELETE FROM ledger WHERE key = ?1", [key.name()]),
        }
        .with_context(|| format!("writing {} to {DB}", key.name()))?;
        Ok(())
    }
}
