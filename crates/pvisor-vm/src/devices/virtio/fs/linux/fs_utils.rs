use std::io;

pub fn ebadf() -> io::Error {
    io::Error::from_raw_os_error(libc::EBADF)
}

pub fn einval() -> io::Error {
    io::Error::from_raw_os_error(libc::EINVAL)
}

/// Native POSIX ACLs cannot enforce guest IDs or synchronize virtual mode.
/// Virtual-owner backings must reject these, allowing mode-preserving tools
/// to take their documented chmod fallback instead of returning false success.
pub(in crate::devices::virtio::fs) fn is_posix_acl_xattr(name: &[u8]) -> bool {
    name == b"system.posix_acl_access" || name == b"system.posix_acl_default"
}
