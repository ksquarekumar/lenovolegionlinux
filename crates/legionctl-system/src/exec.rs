//! Session activity (note 04): loads `bpf/exec.bpf.c`, points its `watched` map at the agent sessions' scopes
//! (`legionctl-session-<id>.scope`) of the users who asked (`WatchSessions`), and reads the programs they start and
//! the files they open from its ring buffers.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use anyhow::{Context, Result};
use aya::maps::{HashMap, MapData, RingBuf};
use legionctl_machine::sysroot::SysRoot;
use legionctl_machine::telemetry::session_scopes;
use tokio::io::unix::AsyncFd;

/// `struct exec` and `struct open` of the program: cgroup id, pid, length, then the arguments or the path.
const HEADER: usize = 16;

/// What a process of a session did: started a program (its arguments) or opened a file (its path).
pub enum Did {
    Exec(Vec<String>),
    Open(String),
}

/// One thing a session did.
pub struct Activity {
    pub uid: u32,
    pub session: String,
    pub pid: u32,
    pub did: Did,
}

/// The two ring buffers, ready for the event loop.
pub struct Rings {
    pub execs: AsyncFd<RingBuf<MapData>>,
    pub opens: AsyncFd<RingBuf<MapData>>,
}

/// The `watched` map of the program, with the scopes it watches: cgroup id -> (uid, session id).
pub struct Watch {
    watched: HashMap<MapData, u64, u32>,
    scopes: BTreeMap<u64, (u32, String)>,
}

/// The pinned programs of this build, else load, attach and pin them; their ring buffers and the watch.
///
/// # Errors
/// No BPF permission, no BTF, the verifier refuses a program (its message is in the error), or a pin fails.
pub fn start(root: &SysRoot) -> Result<(Rings, Watch)> {
    let object = aya::include_bytes_aligned!(concat!(env!("OUT_DIR"), "/exec.bpf.o"));
    let [execs, opens, watched] = crate::pins::attach(
        root,
        "exec",
        object,
        &[("exec", "sched_process_exec"), ("open", "sys_enter")],
        &["execs", "opens", "watched"],
    )?
    .try_into()
    .ok()
    .context("three maps")?;
    let mut watched: HashMap<MapData, u64, u32> = HashMap::try_from(watched)?;
    // the scopes a failed daemon watched; `refresh` adds the live ones again
    let stale: Vec<u64> = watched.keys().collect::<Result<_, _>>()?;
    for id in &stale {
        watched.remove(id)?;
    }
    Ok((
        Rings {
            execs: AsyncFd::new(RingBuf::try_from(execs)?)?,
            opens: AsyncFd::new(RingBuf::try_from(opens)?)?,
        },
        Watch {
            watched,
            scopes: BTreeMap::new(),
        },
    ))
}

/// The session scopes of `uids` in the cgroup tree under `root` (`/sys/fs/cgroup`): cgroup id (the folder's inode)
/// -> (uid, session id).
fn scopes(root: &Path, uids: &BTreeSet<u32>) -> BTreeMap<u64, (u32, String)> {
    uids.iter()
        .flat_map(|uid| {
            session_scopes(root, *uid)
                .into_iter()
                .filter_map(move |(dir, id)| Some((std::fs::metadata(dir).ok()?.ino(), (*uid, id))))
        })
        .collect()
}

impl Watch {
    /// Watch exactly the session scopes of `uids` now.
    ///
    /// # Errors
    /// The map cannot be written.
    pub fn refresh(&mut self, uids: &BTreeSet<u32>) -> Result<()> {
        let found = scopes(Path::new("/sys/fs/cgroup"), uids);
        for gone in self.scopes.keys().filter(|id| !found.contains_key(id)) {
            self.watched.remove(gone)?;
        }
        for new in found.keys().filter(|id| !self.scopes.contains_key(id)) {
            self.watched.insert(new, 1, 0)?;
        }
        self.scopes = found;
        Ok(())
    }

    /// Whether `session`'s scope is watched now.
    pub fn watches(&self, session: &str) -> bool {
        self.scopes.values().any(|(_, s)| s == session)
    }

    /// The events in `ring` now, for the scopes still watched; `open`: the ring of opened files.
    pub fn drain(&self, ring: &mut RingBuf<MapData>, open: bool) -> Vec<Activity> {
        let mut out = Vec::new();
        while let Some(item) = ring.next() {
            let bytes: &[u8] = &item;
            if bytes.len() < HEADER {
                continue;
            }
            let word = |at: usize| {
                u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
            };
            let mut cgroup = [0u8; 8];
            cgroup.copy_from_slice(&bytes[..8]);
            let Some((uid, session)) = self.scopes.get(&u64::from_le_bytes(cgroup)) else {
                continue;
            };
            let len = usize::try_from(word(12))
                .unwrap_or(0)
                .min(bytes.len() - HEADER);
            let data = &bytes[HEADER..HEADER + len];
            let did = if open {
                if data.is_empty() {
                    continue;
                }
                Did::Open(String::from_utf8_lossy(data).into_owned())
            } else {
                Did::Exec(
                    data.split(|b| *b == 0)
                        .filter(|a| !a.is_empty())
                        .map(|a| String::from_utf8_lossy(a).into_owned())
                        .collect(),
                )
            };
            out.push(Activity {
                uid: *uid,
                session: session.clone(),
                pid: word(8),
                did,
            });
        }
        out
    }
}
