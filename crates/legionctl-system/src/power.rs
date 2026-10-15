//! Power attribution and throttle triggers (note 04): loads `bpf/power.bpf.c`, gives each CPU its clock at the
//! start, reads the cumulative weight of each process (CPU time × clock), and wakes the event loop at each trigger (a
//! CPU's clock drops, or a thermal interrupt), and each second while a throttle reason is active, so that the MSR is
//! read then and not only at the next event.

use std::time::Duration;

use anyhow::{Context, Result};
use aya::maps::{Array, HashMap, MapData, RingBuf};
use legionctl_machine::sysroot::SysRoot;
use legionctl_machine::telemetry::ProcessWeight;
use tokio::io::unix::AsyncFd;

pub struct Power {
    energy: HashMap<MapData, u32, [u8; 24]>,
}

/// The triggers of the power sensor (none when it does not run), and the look each second while throttled.
pub struct Triggers {
    ring: Option<AsyncFd<RingBuf<MapData>>>,
    recheck: tokio::time::Interval,
}

impl Power {
    /// The pinned programs of this build, else load, attach and pin them; then each CPU's clock now (`struct cpu`:
    /// start time, kHz).
    ///
    /// # Errors
    /// No BPF permission, no BTF, the verifier refuses a program (its message is in the error), or a pin fails.
    pub fn start(root: &SysRoot) -> Result<(Power, AsyncFd<RingBuf<MapData>>)> {
        let object = aya::include_bytes_aligned!(concat!(env!("OUT_DIR"), "/power.bpf.o"));
        let [cpus, energy, triggers] = crate::pins::attach(
            root,
            "power",
            object,
            &[
                ("switch_to", "sched_switch"),
                ("frequency", "cpu_frequency"),
                ("thermal", "thermal_apic_entry"),
            ],
            &["cpus", "energy", "triggers"],
        )?
        .try_into()
        .ok()
        .context("three maps")?;
        let mut cpus: Array<MapData, [u8; 16]> = Array::try_from(cpus)?;
        // the tracepoint gives each change only; a CPU without cpufreq keeps 0 (by time alone)
        for policy in root.children("/sys/devices/system/cpu", "cpu") {
            let (Some(cpu), Some(khz)) = (
                policy
                    .file_name()
                    .and_then(|n| n.to_str()?.strip_prefix("cpu")?.parse::<u32>().ok()),
                root.read(policy.join("cpufreq/scaling_cur_freq"))
                    .and_then(|k| k.parse::<u32>().ok()),
            ) else {
                continue;
            };
            let mut value = [0u8; 16];
            value[8..12].copy_from_slice(&khz.to_le_bytes());
            cpus.set(cpu, value, 0)
                .with_context(|| format!("setting the clock of CPU {cpu}"))?;
        }
        Ok((
            Power {
                energy: HashMap::try_from(energy)?,
            },
            AsyncFd::new(RingBuf::try_from(triggers)?)?,
        ))
    }

    /// The cumulative weight of each process the map holds now.
    ///
    /// # Errors
    /// The map cannot be read.
    pub fn processes(&self) -> Result<Vec<ProcessWeight>> {
        self.energy
            .iter()
            .map(|entry| {
                let (pid, value) = entry?;
                let mut weight = [0u8; 8];
                weight.copy_from_slice(&value[..8]);
                let name = &value[8..];
                let name = &name[..name.iter().position(|b| *b == 0).unwrap_or(name.len())];
                Ok(ProcessWeight {
                    pid,
                    name: String::from_utf8_lossy(name).into_owned(),
                    weight: u64::from_le_bytes(weight),
                })
            })
            .collect()
    }
}

impl Triggers {
    #[must_use]
    pub fn new(ring: Option<AsyncFd<RingBuf<MapData>>>) -> Triggers {
        let mut recheck = tokio::time::interval(Duration::from_secs(1));
        recheck.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        Triggers { ring, recheck }
    }

    /// Wait for the next trigger, or the next second while `throttled`.
    pub async fn next(&mut self, throttled: bool) {
        loop {
            tokio::select! {
                ready = async {
                    match self.ring.as_mut() {
                        Some(ring) => ring.readable_mut().await,
                        None => std::future::pending().await,
                    }
                } => {
                    let Ok(mut guard) = ready else {
                        eprintln!("power sensor: the trigger ring buffer failed; stopped");
                        self.ring = None;
                        continue;
                    };
                    while guard.get_inner_mut().next().is_some() {}
                    guard.clear_ready();
                    return;
                }
                _ = self.recheck.tick(), if throttled => return,
            }
        }
    }
}
