use aya_build::{build_ebpf, Package, Toolchain};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // src/ebpf.rs's include_bytes_aligned! needs a file at this path to
    // exist, but doesn't need it to be a real eBPF program under coverage
    // instrumentation: cargo-llvm-cov's -C instrument-coverage flags leak
    // into this cross-compile and profiler_builtins isn't available for the
    // build-std=core/bpfel-unknown-none combo (E0463). Skip the real
    // cross-compile and stub the expected output file instead.
    if std::env::var_os("SKIP_EBPF_BUILD").is_some() {
        let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
        std::fs::write(out_dir.join("linux-game-haptics-router-ebpf"), [])?;
        return Ok(());
    }

    build_ebpf(
        [Package {
            name: "linux-game-haptics-router-ebpf",
            root_dir: "../linux-game-haptics-router-ebpf",
            no_default_features: false,
            features: &[],
        }],
        Toolchain::default(),
    )?;
    Ok(())
}
