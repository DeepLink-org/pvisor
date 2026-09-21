use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-env-changed=PERSISTING_KRUNFW_PATH");
    println!("cargo:rerun-if-env-changed=PERSISTING_KRUNFW_KERNEL_BUNDLE");

    if env::var("TARGET").as_deref() != Ok("x86_64-unknown-linux-musl") {
        return;
    }

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo did not set OUT_DIR"));
    let bundle_dir = env::var_os("PERSISTING_KRUNFW_KERNEL_BUNDLE").map(PathBuf::from);
    let (kernel, guest_addr, entry_addr) = if let Some(directory) = bundle_dir {
        println!(
            "cargo:rerun-if-changed={}",
            directory.join("kernel.bin").display()
        );
        println!(
            "cargo:rerun-if-changed={}",
            directory.join("kernel.json").display()
        );
        let kernel = fs::read(directory.join("kernel.bin")).unwrap_or_else(|error| {
            panic!(
                "read embedded libkrun kernel bundle {}: {error}",
                directory.join("kernel.bin").display()
            )
        });
        let metadata = fs::read_to_string(directory.join("kernel.json")).unwrap_or_else(|error| {
            panic!(
                "read embedded libkrun kernel metadata {}: {error}",
                directory.join("kernel.json").display()
            )
        });
        let guest_addr = metadata_value(&metadata, "guest_addr");
        let entry_addr = metadata_value(&metadata, "entry_addr");
        (kernel, guest_addr, entry_addr)
    } else {
        let path = env::var_os("PERSISTING_KRUNFW_PATH").unwrap_or_else(|| {
            panic!(
                "building x86_64 Linux musl requires \
                 PERSISTING_KRUNFW_PATH pointing to libkrunfw.so.5, or \
                 PERSISTING_KRUNFW_KERNEL_BUNDLE pointing to a directory containing \
                 kernel.bin and kernel.json"
            )
        });
        println!("cargo:rerun-if-changed={}", PathBuf::from(&path).display());
        extract_kernel_bundle(&PathBuf::from(path))
    };

    assert!(!kernel.is_empty(), "libkrun kernel bundle is empty");
    let kernel_path = out_dir.join("embedded-libkrun-kernel.bin");
    fs::write(&kernel_path, &kernel).unwrap_or_else(|error| {
        panic!(
            "write embedded libkrun kernel {}: {error}",
            kernel_path.display()
        )
    });
    let generated = out_dir.join("embedded_kernel.rs");
    let kernel_literal = format!("{:?}", kernel_path.to_string_lossy());
    let source = format!(
        "pub static KERNEL: &[u8] = include_bytes!({kernel_literal});\n\
         pub const GUEST_ADDR: u64 = {guest_addr};\n\
         pub const ENTRY_ADDR: u64 = {entry_addr};\n"
    );
    fs::write(&generated, source)
        .unwrap_or_else(|error| panic!("write generated kernel module: {error}"));
}

fn extract_kernel_bundle(path: &std::path::Path) -> (Vec<u8>, u64, u64) {
    type GetKernel = unsafe extern "C" fn(*mut u64, *mut u64, *mut usize) -> *mut std::ffi::c_char;

    let library = unsafe { libloading::Library::new(path) }.unwrap_or_else(|error| {
        panic!(
            "load libkrunfw {} while extracting kernel bundle: {error}",
            path.display()
        )
    });
    let get_kernel = unsafe { library.get::<GetKernel>(b"krunfw_get_kernel\0") }
        .unwrap_or_else(|error| panic!("resolve krunfw_get_kernel: {error}"));
    let mut guest_addr = 0;
    let mut entry_addr = 0;
    let mut size = 0;
    let pointer = unsafe { get_kernel(&mut guest_addr, &mut entry_addr, &mut size) };
    if pointer.is_null() || size == 0 {
        panic!("krunfw_get_kernel returned an empty kernel bundle");
    }
    let kernel = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), size) }.to_vec();
    (kernel, guest_addr, entry_addr)
}

fn metadata_value(metadata: &str, key: &str) -> u64 {
    let marker = format!("\"{key}\"");
    let token = metadata
        .split_once(&marker)
        .and_then(|(_, rest)| rest.split_once(':'))
        .and_then(|(_, rest)| {
            let value = rest
                .chars()
                .skip_while(|character| !character.is_ascii_hexdigit() && *character != 'x')
                .take_while(|character| character.is_ascii_hexdigit() || *character == 'x')
                .collect::<String>();
            (!value.is_empty()).then_some(value)
        })
        .unwrap_or_else(|| panic!("kernel metadata is missing numeric {key}"));
    if let Some(hex) = token.strip_prefix("0x") {
        u64::from_str_radix(hex, 16)
    } else {
        token.parse()
    }
    .unwrap_or_else(|error| panic!("kernel metadata {key} is not a valid integer: {error}"))
}
