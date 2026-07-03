use aya_build::{build_ebpf, Package, Toolchain};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    build_ebpf(
        [Package {
            name: "haptics-probe-ebpf",
            root_dir: "../haptics-probe-ebpf",
            no_default_features: false,
            features: &[],
        }],
        Toolchain::default(),
    )?;
    Ok(())
}
