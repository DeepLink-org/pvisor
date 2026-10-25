use std::env;
use std::fs;
use std::path::PathBuf;

#[path = "src/kernel_bundle.rs"]
mod kernel_bundle;

pub fn embed_kernel() {
    println!("cargo:rerun-if-env-changed=PVISOR_KRUNFW_PATH");
    println!("cargo:rerun-if-env-changed=PVISOR_KRUNFW_KERNEL_BUNDLE");

    println!("cargo:rustc-check-cfg=cfg(pvisor_embedded_kernel)");
    let target = env::var("TARGET").expect("Cargo did not set TARGET");
    let static_linux = target == "x86_64-unknown-linux-musl";
    let macos = target == "aarch64-apple-darwin";
    if !static_linux && !macos {
        return;
    }
    // Product builds always select their firmware before invoking Cargo. Keep
    // standalone macOS crate tests and explicit dynamic-firmware users working
    // when neither build-time input was requested.
    if macos
        && env::var_os("PVISOR_KRUNFW_PATH").is_none()
        && env::var_os("PVISOR_KRUNFW_KERNEL_BUNDLE").is_none()
    {
        return;
    }
    println!("cargo:rustc-cfg=pvisor_embedded_kernel");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo did not set OUT_DIR"));
    let bundle_dir = env::var_os("PVISOR_KRUNFW_KERNEL_BUNDLE").map(PathBuf::from);
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
        let path = env::var_os("PVISOR_KRUNFW_PATH").unwrap_or_else(|| {
            panic!(
                "building an embedded guest kernel requires \
                 PVISOR_KRUNFW_PATH pointing to a host-loadable libkrunfw library, or \
                 PVISOR_KRUNFW_KERNEL_BUNDLE pointing to a directory containing \
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
    let packed_path = out_dir.join("embedded-libkrun-kernel.packed");
    fs::write(&packed_path, kernel_bundle::pack(&kernel))
        .unwrap_or_else(|error| panic!("write packed kernel bundle: {error}"));
    let decoder_path = out_dir.join("kernel_bundle_decoder.rs");
    fs::write(&decoder_path, include_str!("src/kernel_bundle.rs"))
        .unwrap_or_else(|error| panic!("write shared kernel decoder: {error}"));
    let generated = out_dir.join("embedded_kernel.rs");
    let kernel_literal = format!("{:?}", packed_path.to_string_lossy());
    let decoder_literal = format!("{:?}", decoder_path.to_string_lossy());
    let source = format!(
        "mod packed {{ include!({decoder_literal}); }}\n\
         pub static KERNEL: std::sync::LazyLock<std::sync::Arc<[u8]>> = std::sync::LazyLock::new(|| {{\n\
             let mut span = crate::startup_profile::Span::new(\"kernel_unpack\", \"runtime\");\n\
             let kernel = packed::unpack(include_bytes!({kernel_literal})).expect(\"invalid embedded kernel bundle\");\n\
             span.complete(Some(kernel.len()));\n\
             kernel\n\
         }});\n\
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

#[cfg(test)]
mod tests {
    #[test]
    fn build_script_child() {
        if std::env::var_os("PVISOR_TEST_KERNEL_BUILD_CHILD").is_some() {
            super::embed_kernel();
        }
    }

    fn generate(
        target: &str,
        bundle: Option<&std::path::Path>,
        output: &std::path::Path,
    ) -> std::process::Output {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "build_kernel::tests::build_script_child",
                "--nocapture",
            ])
            .env("PVISOR_TEST_KERNEL_BUILD_CHILD", "1")
            .env("TARGET", target)
            .env("OUT_DIR", output)
            .env_remove("PVISOR_KRUNFW_PATH")
            .env_remove("PVISOR_KRUNFW_KERNEL_BUNDLE");
        if let Some(bundle) = bundle {
            command.env("PVISOR_KRUNFW_KERNEL_BUNDLE", bundle);
        }
        command.output().unwrap()
    }

    #[test]
    fn both_product_targets_embed_exact_kernel_bytes_and_boot_addresses() {
        let root = tempfile::tempdir().unwrap();
        let bundle = root.path().join("bundle");
        std::fs::create_dir(&bundle).unwrap();
        let bytes = [vec![0; 65536], vec![0xab; 16384], (0..=255).collect()].concat();
        std::fs::write(bundle.join("kernel.bin"), &bytes).unwrap();
        std::fs::write(
            bundle.join("kernel.json"),
            r#"{"guest_addr": 2148007936, "entry_addr": 2148012032}"#,
        )
        .unwrap();
        for target in ["x86_64-unknown-linux-musl", "aarch64-apple-darwin"] {
            let output = root.path().join(target);
            std::fs::create_dir(&output).unwrap();
            let result = generate(target, Some(&bundle), &output);
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
            assert!(String::from_utf8_lossy(&result.stdout)
                .contains("cargo:rustc-cfg=pvisor_embedded_kernel"));
            let packed = std::fs::read(output.join("embedded-libkrun-kernel.packed")).unwrap();
            assert_eq!(&*super::kernel_bundle::unpack(&packed).unwrap(), bytes);
            let generated = std::fs::read_to_string(output.join("embedded_kernel.rs")).unwrap();
            assert!(generated.contains("GUEST_ADDR: u64 = 2148007936"));
            assert!(generated.contains("ENTRY_ADDR: u64 = 2148012032"));
        }
    }

    #[test]
    fn invalid_selected_macos_bundle_fails_instead_of_falling_back_to_a_dylib() {
        let root = tempfile::tempdir().unwrap();
        let result = generate("aarch64-apple-darwin", Some(root.path()), root.path());
        assert!(!result.status.success());
        assert!(!root.path().join("embedded_kernel.rs").exists());
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("read embedded libkrun kernel bundle")
        );
    }
}
