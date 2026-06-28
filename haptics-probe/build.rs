use aya_build::cargo_metadata;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let programs = &["haptics-probe-ebpf"];
    aya_build::build_ebpf(programs)?;
    Ok(())
}
