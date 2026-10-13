//! The run-queue latency sensor (note 04): loads `bpf/runq.bpf.c` and reads its per-CPU log2 histogram.

use anyhow::{Context, Result};
use aya::maps::{MapData, PerCpuArray};
use aya::programs::BtfTracePoint;
use aya::{Btf, Ebpf};

pub struct Runq {
    /// Keeps the programs attached.
    _bpf: Ebpf,
    hist: PerCpuArray<MapData, u64>,
}

impl Runq {
    /// Load and attach the programs.
    ///
    /// # Errors
    /// No BPF permission, no BTF, or the verifier refuses a program (its message is in the error).
    pub fn start() -> Result<Runq> {
        let mut bpf = Ebpf::load(aya::include_bytes_aligned!(concat!(
            env!("OUT_DIR"),
            "/runq.bpf.o"
        )))
        .context("loading the runq object")?;
        let btf = Btf::from_sys_fs().context("reading the kernel BTF")?;
        for (program, tracepoint) in [
            ("wakeup", "sched_wakeup"),
            ("wakeup_new", "sched_wakeup_new"),
            ("switch_to", "sched_switch"),
        ] {
            let p: &mut BtfTracePoint = bpf
                .program_mut(program)
                .with_context(|| format!("no program {program}"))?
                .try_into()?;
            p.load(tracepoint, &btf)
                .with_context(|| format!("loading {program}"))?;
            p.attach()
                .with_context(|| format!("attaching {program} to {tracepoint}"))?;
        }
        let hist = PerCpuArray::try_from(bpf.take_map("runq_hist").context("no runq_hist map")?)?;
        Ok(Runq { _bpf: bpf, hist })
    }

    /// The cumulative histogram of each CPU: `[cpu][bucket]`, bucket i counting waits of 2^i to 2^(i+1) µs.
    ///
    /// # Errors
    /// The map cannot be read.
    pub fn per_cpu(&self) -> Result<Vec<Vec<u64>>> {
        let mut cpus: Vec<Vec<u64>> = Vec::new();
        for (bucket, values) in self.hist.iter().enumerate() {
            for (cpu, count) in values?.iter().enumerate() {
                if cpus.len() <= cpu {
                    cpus.resize(cpu + 1, Vec::new());
                }
                cpus[cpu].resize(bucket + 1, 0);
                cpus[cpu][bucket] = *count;
            }
        }
        Ok(cpus)
    }
}
