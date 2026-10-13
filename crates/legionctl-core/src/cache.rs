//! The one cache contract (DESIGN/03): `$XDG_CACHE_HOME/legionctl/cache.db`, which is safe to delete. Each namespace
//! (`tools`, `gateway`) has a byte limit (`cache.<namespace>.max_bytes`); an entry has a time to live; a write
//! removes the expired entries and then the least recently used ones until the namespace fits. Keys are built by the
//! operator, never by a client.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use rusqlite::{OptionalExtension, params};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::db::Db;

const SCHEMA: &[&str] = &[
    // v1: entries of every namespace; the semantic cache's prompt embeddings beside the answers they find
    "CREATE TABLE entries (
        ns      TEXT NOT NULL,
        key     TEXT NOT NULL,
        value   BLOB NOT NULL,
        meta    TEXT NOT NULL,
        bytes   INTEGER NOT NULL,
        expires INTEGER NOT NULL,
        used    INTEGER NOT NULL,
        PRIMARY KEY (ns, key)
     );
     CREATE INDEX entries_used ON entries (ns, used);
     CREATE TABLE vectors (
        ns     TEXT NOT NULL,
        key    TEXT NOT NULL,
        scope  TEXT NOT NULL,
        vector BLOB NOT NULL,
        PRIMARY KEY (ns, key)
     );",
];

/// One namespace's size now.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    pub ns: String,
    pub entries: u64,
    pub bytes: u64,
}

/// A kept value and what was kept with it (the gateway keeps a response's status and content type there).
pub struct Entry {
    pub value: Vec<u8>,
    pub meta: serde_json::Value,
}

pub struct Cache {
    db: Db,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

/// `$XDG_CACHE_HOME/legionctl/cache.db`, else `~/.cache/legionctl/cache.db`.
#[must_use]
pub fn default_path() -> PathBuf {
    let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
    std::env::var_os("XDG_CACHE_HOME")
        .map_or_else(|| home.join(".cache"), PathBuf::from)
        .join("legionctl/cache.db")
}

impl Cache {
    /// # Errors
    /// The cache database cannot be opened or migrated.
    pub fn open() -> Result<Cache> {
        Ok(Cache {
            db: Db::open(&default_path(), SCHEMA)?,
        })
    }

    /// The entry of `key` in `ns` while it lives; reading it marks it as used.
    ///
    /// # Errors
    /// The database fails.
    pub fn get(&self, ns: &str, key: &str) -> Result<Option<Entry>> {
        let conn = self.db.conn();
        let found: Option<(Vec<u8>, String)> = conn
            .query_row(
                "SELECT value, meta FROM entries WHERE ns = ?1 AND key = ?2 AND expires > ?3",
                params![ns, key, now()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if found.is_some() {
            conn.execute(
                "UPDATE entries SET used = ?3 WHERE ns = ?1 AND key = ?2",
                params![ns, key, now()],
            )?;
        }
        Ok(found.map(|(value, meta)| Entry {
            value,
            meta: serde_json::from_str(&meta).unwrap_or_default(),
        }))
    }

    /// Keep `value` under `key` in `ns` for `ttl_s` seconds, then shrink `ns` to `max_bytes`: expired entries first,
    /// then the least recently used.
    ///
    /// # Errors
    /// The database fails.
    pub fn put(
        &self,
        (ns, key): (&str, &str),
        entry: &Entry,
        ttl_s: u64,
        max_bytes: u64,
    ) -> Result<()> {
        let conn = self.db.conn();
        let now = now();
        let bytes = i64::try_from(entry.value.len()).unwrap_or(i64::MAX);
        conn.execute(
            "INSERT OR REPLACE INTO entries (ns, key, value, meta, bytes, expires, used)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                ns,
                key,
                entry.value,
                entry.meta.to_string(),
                bytes,
                now.saturating_add(i64::try_from(ttl_s).unwrap_or(i64::MAX)),
                now
            ],
        )?;
        conn.execute(
            "DELETE FROM entries WHERE ns = ?1 AND expires <= ?2",
            params![ns, now],
        )?;
        let max = i64::try_from(max_bytes).unwrap_or(i64::MAX);
        // the newest entries that fit stay; the rest go
        conn.execute(
            "DELETE FROM entries WHERE ns = ?1 AND key IN (
                SELECT key FROM (
                    SELECT key, SUM(bytes) OVER (ORDER BY used DESC, key) AS total
                    FROM entries WHERE ns = ?1
                ) WHERE total > ?2
             )",
            params![ns, max],
        )?;
        conn.execute(
            "DELETE FROM vectors WHERE ns = ?1 AND key NOT IN (SELECT key FROM entries WHERE ns = ?1)",
            params![ns],
        )?;
        Ok(())
    }

    /// Keep the embedding of the entry under `key`, for a nearest match within `scope`.
    ///
    /// # Errors
    /// The database fails.
    pub fn keep_vector(&self, (ns, key): (&str, &str), scope: &str, vector: &[f64]) -> Result<()> {
        let bytes: Vec<u8> = vector.iter().flat_map(|f| f.to_le_bytes()).collect();
        self.db.conn().execute(
            "INSERT OR REPLACE INTO vectors (ns, key, scope, vector) VALUES (?1, ?2, ?3, ?4)",
            params![ns, key, scope, bytes],
        )?;
        Ok(())
    }

    /// The key of the live entry in `scope` whose embedding is nearest to `vector` (cosine), if it reaches
    /// `threshold`.
    ///
    /// # Errors
    /// The database fails.
    pub fn nearest(
        &self,
        ns: &str,
        scope: &str,
        vector: &[f64],
        threshold: f64,
    ) -> Result<Option<String>> {
        let mut stmt = self.db.conn().prepare(
            "SELECT v.key, v.vector FROM vectors v JOIN entries e ON e.ns = v.ns AND e.key = v.key
             WHERE v.ns = ?1 AND v.scope = ?2 AND e.expires > ?3",
        )?;
        let rows = stmt.query_map(params![ns, scope, now()], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        let mut best: Option<(f64, String)> = None;
        for row in rows {
            let (key, bytes) = row?;
            let kept: Vec<f64> = bytes
                .as_chunks::<8>()
                .0
                .iter()
                .map(|c| f64::from_le_bytes(*c))
                .collect();
            let similarity = cosine(vector, &kept);
            if similarity >= threshold && best.as_ref().is_none_or(|(b, _)| similarity > *b) {
                best = Some((similarity, key));
            }
        }
        Ok(best.map(|(_, key)| key))
    }

    /// Entries and bytes of each namespace.
    ///
    /// # Errors
    /// The database fails.
    pub fn usage(&self) -> Result<Vec<Usage>> {
        let mut stmt = self
            .db
            .conn()
            .prepare("SELECT ns, COUNT(*), SUM(bytes) FROM entries GROUP BY ns ORDER BY ns")?;
        let rows = stmt.query_map([], |r| {
            let (entries, bytes): (i64, i64) = (r.get(1)?, r.get(2)?);
            Ok(Usage {
                ns: r.get(0)?,
                entries: u64::try_from(entries).unwrap_or_default(),
                bytes: u64::try_from(bytes).unwrap_or_default(),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Delete every entry of `ns`, or of every namespace; the count deleted.
    ///
    /// # Errors
    /// The database fails.
    pub fn purge(&self, ns: Option<&str>) -> Result<usize> {
        let conn = self.db.conn();
        let deleted = match ns {
            Some(ns) => conn.execute("DELETE FROM entries WHERE ns = ?1", params![ns])?,
            None => conn.execute("DELETE FROM entries", [])?,
        };
        conn.execute(
            "DELETE FROM vectors WHERE NOT EXISTS (SELECT 1 FROM entries e WHERE e.ns = vectors.ns AND e.key = vectors.key)",
            [],
        )?;
        Ok(deleted)
    }
}

fn cosine(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    let (mut dot, mut na, mut nb) = (0.0_f64, 0.0_f64, 0.0_f64);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 {
        0.0
    } else {
        dot / (na.sqrt() * nb.sqrt())
    }
}
