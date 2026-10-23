// Compile the dependency-free vsock adapter for the Linux guest, including on
// hosts whose ordinary Rust target uses a different C library.
fn main() {
    println!("cargo:rerun-if-changed=src/sandbox_init.rs");
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    let target = format!("{arch}-unknown-linux-musl");
    let output =
        std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap()).join("pvisor-sandbox-init");
    let status = std::process::Command::new(std::env::var_os("RUSTC").unwrap())
        .args([
            "src/sandbox_init.rs",
            "--edition=2024",
            "--target",
            &target,
            "-C",
            "linker=rust-lld",
            "-C",
            "opt-level=s",
            "-C",
            "strip=symbols",
            "-o",
        ])
        .arg(&output)
        .status()
        .expect("compile sandbox guest adapter");
    assert!(
        status.success(),
        "install guest stdlib: rustup target add {target}"
    );
    println!("cargo:rustc-env=PVISOR_SANDBOX_INIT={}", output.display());
}
