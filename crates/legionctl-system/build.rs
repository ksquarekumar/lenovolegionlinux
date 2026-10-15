//! Compile the eBPF programs (`bpf/*.bpf.c`) with clang; the daemon embeds the objects.

use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("cargo sets OUT_DIR"));
    for name in ["runq", "exec", "workload", "power", "io", "thermal"] {
        let source = format!("bpf/{name}.bpf.c");
        println!("cargo::rerun-if-changed={source}");
        let status = Command::new("clang")
            .args([
                "-target", "bpf", "-O2", "-g", "-Wall", "-Werror", "-c", &source, "-o",
            ])
            .arg(out.join(format!("{name}.bpf.o")))
            .status()
            .expect("clang is needed to build the eBPF programs");
        assert!(status.success(), "clang failed on {source}");
    }
}
