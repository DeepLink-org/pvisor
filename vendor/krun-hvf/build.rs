fn main() {
    println!("cargo:rerun-if-changed=src/snapshot_simd.c");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("macos")
        && std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("aarch64")
    {
        cc::Build::new()
            .file("src/snapshot_simd.c")
            .compile("krun_snapshot_simd");
    }
}
