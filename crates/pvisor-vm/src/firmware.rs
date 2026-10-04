//! The sole guest-kernel ABI boundary. This is firmware data loading, not the
//! removed libkrun VM control ABI. The owner outlives every guest mapping.
use crate::vmm::vmm_config::kernel_bundle::KernelBundle;
use std::io;

#[derive(Default)]
pub(crate) struct KernelOwner {
    mapping: Option<Mapping>,
    #[cfg(not(target_env = "musl"))]
    library: Option<libloading::Library>,
}
struct Mapping {
    address: *mut libc::c_void,
    size: usize,
}
// SAFETY: only the owner releases this allocation; moving it cannot invalidate
// the address, and writes occur solely through the running VM after installation.
unsafe impl Send for Mapping {}
impl Drop for Mapping {
    fn drop(&mut self) {
        // SAFETY: mmap allocated this exact address and length.
        unsafe {
            libc::munmap(self.address, self.size);
        }
    }
}
impl KernelOwner {
    pub(crate) fn embedded(
        &mut self,
        bytes: &[u8],
        guest_addr: u64,
        entry_addr: u64,
    ) -> io::Result<KernelBundle> {
        if bytes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "empty guest kernel",
            ));
        }
        // SAFETY: sysconf takes a constant selector and no pointers.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if page <= 0 || !(page as usize).is_power_of_two() {
            return Err(io::Error::other("invalid host page size"));
        }
        let page = page as usize;
        let size = bytes
            .len()
            .checked_add(page - 1)
            .ok_or_else(|| io::Error::other("kernel size overflow"))?
            & !(page - 1);
        // SAFETY: new anonymous private allocation; no borrowed host pointer.
        let address = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if address == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: the mapping is writable and at least bytes.len() bytes long.
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), address.cast(), bytes.len());
        }
        self.mapping = Some(Mapping { address, size });
        Ok(KernelBundle {
            host_addr: address as u64,
            guest_addr,
            entry_addr,
            size: bytes.len(),
        })
    }
    #[cfg(not(target_env = "musl"))]
    pub(crate) fn load(&mut self) -> io::Result<KernelBundle> {
        let name = crate::firmware_store::firmware_name();
        // SAFETY: trusted pVisor firmware implements this versioned kernel ABI.
        let library = unsafe { libloading::Library::new(name) }.map_err(io::Error::other)?;
        let mut guest_addr = 0;
        let mut entry_addr = 0;
        let mut size = 0;
        // SAFETY: symbol signature matches libkrunfw v5; out parameters are valid.
        let address = unsafe {
            let get: libloading::Symbol<
                unsafe extern "C" fn(*mut u64, *mut u64, *mut usize) -> *mut u8,
            > = library
                .get(b"krunfw_get_kernel")
                .map_err(io::Error::other)?;
            get(&mut guest_addr, &mut entry_addr, &mut size)
        };
        if address.is_null() || size == 0 {
            return Err(io::Error::other("firmware returned an empty kernel"));
        }
        self.library = Some(library);
        Ok(KernelBundle {
            host_addr: address as u64,
            guest_addr,
            entry_addr,
            size,
        })
    }
}

#[cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]
mod built_in {
    include!(concat!(env!("OUT_DIR"), "/embedded_kernel.rs"));
}

pub(crate) fn embedded_kernel() -> Option<crate::api::KernelImage> {
    #[cfg(all(target_os = "linux", target_env = "musl", target_arch = "x86_64"))]
    {
        Some(crate::api::KernelImage {
            bytes: built_in::KERNEL.clone(),
            guest_address: built_in::GUEST_ADDR,
            entry_address: built_in::ENTRY_ADDR,
        })
    }
    #[cfg(not(all(target_os = "linux", target_env = "musl", target_arch = "x86_64")))]
    {
        None
    }
}
