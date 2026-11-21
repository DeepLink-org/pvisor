//! Process-local selection of a kernel already embedded in the executable.

use std::{ffi::OsStr, io};

pub(crate) const SELECTOR: &str = "PVISOR_NATIVE_KERNEL";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Selection {
    Linux,
    RustNative,
}

fn parse(value: Option<&OsStr>) -> io::Result<Selection> {
    match value {
        None => Ok(Selection::Linux),
        Some(value) if value == "linux" => Ok(Selection::Linux),
        Some(value) if value == "rust" => Ok(Selection::RustNative),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{SELECTOR} must be linux or rust"),
        )),
    }
}

pub(crate) fn selection() -> io::Result<Selection> {
    let selection = parse(std::env::var_os(SELECTOR).as_deref())?;
    if selection == Selection::RustNative
        && (!cfg!(any(
            all(
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64")
            ),
            all(target_os = "macos", target_arch = "aarch64")
        )) || cfg!(any(feature = "tee", feature = "efi", feature = "aws-nitro")))
    {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Rust native kernel requires plain Linux x86_64/aarch64 KVM or macOS aarch64 HVF",
        ));
    }
    Ok(selection)
}

pub(crate) fn fits_memory(
    guest_address: u64,
    size: u64,
    ram_start: u64,
    memory: u64,
    reserved: u64,
) -> bool {
    guest_address
        .checked_sub(ram_start)
        .and_then(|offset| offset.checked_add(size))
        .zip(memory.checked_sub(reserved))
        .is_some_and(|(end, limit)| end <= limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rstest::rstest]
    #[case::arm_offset(0x8020_0000, 0x4000, 0x8000_0000, 0x1000_0000, 0x20_0000, true)]
    #[case::arm_fdt_boundary(0x8020_0000, 0x0fc0_0000, 0x8000_0000, 0x1000_0000, 0x20_0000, true)]
    #[case::arm_fdt_overlap(0x8020_0000, 0x0fc0_0001, 0x8000_0000, 0x1000_0000, 0x20_0000, false)]
    #[case::below_ram(0x4020_0000, 0x4000, 0x8000_0000, 0x1000_0000, 0x20_0000, false)]
    #[case::address_wrap(u64::MAX, 0x4000, 0, 0x1000_0000, 0, false)]
    #[case::reservation_exceeds_ram(0, 4096, 0, 4096, 8192, false)]
    #[case::x86_boundary(0x0800_0000, 0x0800_0000, 0, 0x1000_0000, 0, true)]
    #[case::x86_overflow(0x0800_0000, 0x0800_0001, 0, 0x1000_0000, 0, false)]
    fn kernel_and_reserved_fdt_fit_ram(
        #[case] address: u64,
        #[case] size: u64,
        #[case] base: u64,
        #[case] memory: u64,
        #[case] reserved: u64,
        #[case] expected: bool,
    ) {
        assert_eq!(fits_memory(address, size, base, memory, reserved), expected);
    }

    #[rstest::rstest]
    #[case::default(None, Ok(Selection::Linux))]
    #[case::linux(Some("linux"), Ok(Selection::Linux))]
    #[case::rust_native(Some("rust"), Ok(Selection::RustNative))]
    #[case::old_value(Some("rust-native"), Err(io::ErrorKind::InvalidInput))]
    #[case::empty(Some(""), Err(io::ErrorKind::InvalidInput))]
    #[case::typo(Some("native"), Err(io::ErrorKind::InvalidInput))]
    fn selector_is_explicit(
        #[case] value: Option<&str>,
        #[case] expected: Result<Selection, io::ErrorKind>,
    ) {
        assert_eq!(
            parse(value.map(OsStr::new)).map_err(|error| error.kind()),
            expected
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_unicode_selector_is_not_linux() {
        use std::os::unix::ffi::OsStrExt;
        assert_eq!(
            parse(Some(OsStr::from_bytes(b"\xff"))).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }
}
