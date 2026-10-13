//! Session activity (note 04): loads `bpf/exec.bpf.c`, points its `watched` map at the agent sessions' scopes
//! (`legionctl-session-<id>.scope`) of the users who asked (`WatchSessions`), and reads the programs they start from
//! its ring buffer.

use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use anyhow::{Context, Result};
use aya::maps::{HashMap, MapData, RingBuf};
use aya::programs::BtfTracePoint;
use aya::programs::links::FdLink;
use aya::{Btf, Ebpf};
use legionctl_machine::sysroot::SysRoot;
use tokio::io::unix::AsyncFd;

use crate::pins::Pins;

/// `struct exec` of the program: cgroup id, pid, length, then the arguments.
const HEADER: usize = 16;

/// One program a session started.
pub struct Exec {
    pub uid: u32,
    pub session: String,
    pub pid: u32,
    pub argv: Vec<String>,
}

/// The `watched` map of the program, with the scopes it watches: cgroup id -> (uid, session id).
pub struct Watch {
    watched: HashMap<MapData, u64, u32>,
    scopes: BTreeMap<u64, (u32, String)>,
}

/// The pinned program of this build, else load, attach and pin it; its ring buffer, ready for the event loop, and the
/// watch.
///
/// # Errors
/// No BPF permission, no BTF, the verifier refuses the program (its message is in the error), or a pin fails.
pub fn start(root: &SysRoot) -> Result<(AsyncFd<RingBuf<MapData>>, Watch)> {
    let object = aya::include_bytes_aligned!(concat!(env!("OUT_DIR"), "/exec.bpf.o"));
    let pins = Pins::of(root, "exec", object);
    let (ring, watched) =
        if let (Some(ring), Some(watched)) = (pins.map("execs"), pins.map("watched")) {
            (ring, watched)
        } else {
            let mut bpf = Ebpf::load(object).context("loading the exec object")?;
            let btf = Btf::from_sys_fs().context("reading the kernel BTF")?;
            let program: &mut BtfTracePoint = bpf
                .program_mut("exec")
                .context("no program exec")?
                .try_into()?;
            program
                .load("sched_process_exec", &btf)
                .context("loading exec")?;
            let link = program
                .attach()
                .context("attaching exec to sched_process_exec")?;
            let link = FdLink::from(program.take_link(link)?);
            let ring = bpf.take_map("execs").context("no execs map")?;
            let watched = bpf.take_map("watched").context("no watched map")?;
            pins.pin(
                vec![("exec", link)],
                &[("execs", &ring), ("watched", &watched)],
            )?;
            (ring, watched)
        };
    let mut watched: HashMap<MapData, u64, u32> = HashMap::try_from(watched)?;
    // the scopes a failed daemon watched; `refresh` adds the live ones again
    let stale: Vec<u64> = watched.keys().collect::<Result<_, _>>()?;
    for id in &stale {
        watched.remove(id)?;
    }
    Ok((
        AsyncFd::new(RingBuf::try_from(ring)?)?,
        Watch {
            watched,
            scopes: BTreeMap::new(),
        },
    ))
}

/// The session scopes of `uids` in the cgroup tree under `root` (`/sys/fs/cgroup`): cgroup id (the folder's inode)
/// -> (uid, session id).
fn scopes(root: &Path, uids: &BTreeSet<u32>) -> BTreeMap<u64, (u32, String)> {
    fn walk(dir: &Path, depth: u32, uid: u32, out: &mut BTreeMap<u64, (u32, String)>) {
        for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if !meta.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            match name
                .strip_prefix("legionctl-session-")
                .and_then(|n| n.strip_suffix(".scope"))
            {
                Some(id) => {
                    out.insert(meta.ino(), (uid, id.to_string()));
                }
                None if depth > 0 => walk(&entry.path(), depth - 1, uid, out),
                None => {}
            }
        }
    }
    let mut out = BTreeMap::new();
    for uid in uids {
        let user = root.join(format!("user.slice/user-{uid}.slice/user@{uid}.service"));
        // the scopes sit in app.slice, or a slice below it
        walk(&user, 2, *uid, &mut out);
    }
    out
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

    /// The events in `ring` now, for the scopes still watched.
    pub fn drain(&self, ring: &mut RingBuf<MapData>) -> Vec<Exec> {
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
            let argv = bytes[HEADER..HEADER + len]
                .split(|b| *b == 0)
                .filter(|a| !a.is_empty())
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect();
            out.push(Exec {
                uid: *uid,
                session: session.clone(),
                pid: word(8),
                argv,
            });
        }
        out
    }
}
