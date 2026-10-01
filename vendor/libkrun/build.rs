//! Embed the Rust guest supervisor. Its build never needs a C cross-compiler.
use std::{env, path::PathBuf, process::Command};

fn main() {
    if env::var_os("CARGO_FEATURE_INIT_BLOB").is_none() {
        return;
    }
    let root = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap())
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf();
    for source in ["Cargo.toml", "Cargo.lock", "crates/persisting-guest"] {
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
            "persisting-guest",
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
    println!(
        "cargo:rustc-env=PERSISTING_GUEST_BINARY={}",
        binary.display()
    );
}
