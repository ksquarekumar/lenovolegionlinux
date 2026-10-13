//! What legionctl remembers about the firmware mode across its own restarts but not across a reboot: the mode it wrote
//! last, and the mode the user chose with Fn+Q. Rows live in `system.db` tagged with the boot id; rows from another
//! boot are never read, so a reboot starts from the policy again.

use anyhow::{Context, Result};
use rusqlite::{Connection, OpenFlags, OptionalExtension, params};

use crate::sysroot::SysRoot;

pub const DB: &str = "/var/lib/legionctl/system.db";
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// The firmware mode legionctl wrote last; a change to it is our own.
    Written,
    /// The firmware mode the user chose with Fn+Q; kept until the power source changes or `apply` runs.
    Override,
}

impl Key {
    fn name(self) -> &'static str {
        match self {
            Key::Written => "firmware-written",
            Key::Override => "firmware-override",
        }
    }
}

pub struct Ledger {
    /// `None` when the database does not exist and cannot be created (a user reading before the daemon ever ran).
    conn: Option<Connection>,
    boot: String,
}

impl Ledger {
    /// Open for writing (root), else read-only (a user's status or dry run), else empty.
    ///
    /// # Errors
    /// The boot id is not readable.
    pub fn open(root: &SysRoot) -> Result<Ledger> {
        let boot = root.read(BOOT_ID).context("reading the boot id")?;
        let path = root.path(DB);
        let writable = path
            .parent()
            .is_some_and(|dir| std::fs::create_dir_all(dir).is_ok())
            .then(|| Connection::open(&path).ok())
            .flatten()
            .filter(|c| {
                c.execute_batch(
                    "CREATE TABLE IF NOT EXISTS ledger (
                        key     TEXT PRIMARY KEY,
                        value   TEXT NOT NULL,
                        boot_id TEXT NOT NULL
                     );
                     -- the user's holds (D1, decided 2026-10-13): a path the policy leaves at this value until released;
                     -- a * matches the CPU policies of a per-CPU knob
                     CREATE TABLE IF NOT EXISTS holds (
                        path  TEXT PRIMARY KEY,
                        value TEXT NOT NULL
                     );",
                )
                .is_ok()
            });
        let conn = writable
            .or_else(|| Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok());
        Ok(Ledger { conn, boot })
    }

    pub fn get(&self, key: Key) -> Option<String> {
        self.conn
            .as_ref()?
            .query_row(
                "SELECT value FROM ledger WHERE key = ?1 AND boot_id = ?2",
                params![key.name(), self.boot],
                |r| r.get(0),
            )
            .optional()
            .ok()
            .flatten()
    }

    /// The user's holds: path (a `*` for the CPUs of a per-CPU knob) -> the value it keeps.
    #[must_use]
    pub fn holds(&self) -> std::collections::BTreeMap<String, String> {
        let Some(conn) = self.conn.as_ref() else {
            return std::collections::BTreeMap::new();
        };
        conn.prepare("SELECT path, value FROM holds")
            .and_then(|mut stmt| {
                stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<_>>()
            })
            .unwrap_or_default()
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
