//! Embed the Rust guest supervisor. Its build never needs a C cross-compiler.
use std::{env, path::PathBuf, process::Command};

fn embed_guest() {
    if env::var_os("CARGO_FEATURE_INIT_BLOB").is_none() {
        return;
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    for source in ["Cargo.toml", "Cargo.lock", "crates/pvisor-guest"] {
        println!("cargo:rerun-if-changed={}", root.join(source).display());
    }
    let arch = env::var("CARGO_CFG_TARGET_ARCH").unwrap();
    assert!(
        matches!(arch.as_str(), "aarch64" | "x86_64"),
        "unsupported guest architecture {arch}"
    );
    let target = format!("{arch}-unknown-linux-musl");
    // A separate target directory avoids the outer Cargo build's artifact lock.
    let target_dir = root.join("target/pvisor-guest");
    let status = Command::new(env::var_os("CARGO").unwrap())
        .current_dir(&root)
        .args([
            "build",
            "--locked",
            "--release",
            "-p",
            "pvisor-guest",
            "--bin",
            "pvisor-guest",
            "--target",
            &target,
        ])
        .arg("--target-dir")
        .arg(&target_dir)
        .arg("--config")
        // rustc resolves rust-lld from its own toolchain, including on macOS.
        .arg(format!("target.{target}.linker=\"rust-lld\""))
        .env_remove("CARGO_ENCODED_RUSTFLAGS")
        .env_remove("RUSTFLAGS")
        .env_remove("RUSTC_WORKSPACE_WRAPPER")
        .status()
        .expect("build Rust guest supervisor");
    assert!(
        status.success(),
        "guest build failed; install its stdlib with: rustup target add {target}"
    );
    let binary = target_dir.join(&target).join("release/pvisor-guest");
    println!("cargo:rerun-if-changed={}", binary.display());
    println!("cargo:rustc-env=PVISOR_GUEST_BINARY={}", binary.display());
}

fn platform() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64")
        && std::env::var_os("CARGO_FEATURE_EFI").is_some()
    {
        let edk2_binary_path = std::env::var("KRUN_EDK2_BINARY_PATH").unwrap_or_else(|_| {
            format!(
                "{}/edk2/KRUN_EFI.silent.fd",
                std::env::var("CARGO_MANIFEST_DIR").unwrap()
            )
        });
        println!("cargo:rustc-env=KRUN_EDK2_BINARY_PATH={edk2_binary_path}");
        println!("cargo:rerun-if-env-changed=KRUN_EDK2_BINARY_PATH");
    }

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos") {
        println!("cargo:rustc-link-lib=framework=Hypervisor");
    }
}

fn simd() {
    println!("cargo:rerun-if-changed=src/hvf/snapshot_simd.c");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64")
    {
        cc::Build::new()
            .file("src/hvf/snapshot_simd.c")
            .compile("krun_snapshot_simd");
    }
}

#[path = "build_kernel.rs"]
mod kernel;

fn main() {
    kernel::embed_kernel();
    platform();
    simd();
    embed_guest();
}
