//! Windows configuration owns its kernel and shares until the runner exits.
use crate::devices::virtio::fs::passthrough::{Config, SyntheticFile};
use crate::vmm::vmm_config::{
    fs::FsDeviceConfig,
    kernel_cmdline::{KernelCmdlineConfig, DEFAULT_KERNEL_CMDLINE},
    machine_config,
};
use crate::{api::*, firmware::KernelOwner, vmm::resources::VmResources};
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicI32},
        Arc, Mutex,
    },
};

pub(crate) struct Builder {
    resources: VmResources,
    kernel: KernelOwner,
    firmware: Option<PathBuf>,
    shares: BTreeMap<String, Config>,
    implicit_init: bool,
}
fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
fn unsupported(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}
impl Builder {
    pub(crate) fn new(cpus: u8, memory_mib: u32) -> io::Result<Self> {
        if cfg!(any(
            feature = "tee",
            feature = "aws-nitro",
            feature = "efi",
            feature = "gpu",
            feature = "snd",
            feature = "input"
        )) {
            return Err(unsupported(
                "this device profile is unsupported on Windows/WHP",
            ));
        }
        if !cfg!(target_arch = "x86_64") {
            return Err(unsupported("pVisor WHP currently supports x86_64"));
        }
        if cpus == 0 || cpus > 64 || memory_mib == 0 || memory_mib > 65536 {
            return Err(invalid("WHP requires 1..=64 CPUs and 1..=65536 MiB RAM"));
        }
        let mut resources = VmResources::default();
        resources
            .set_vm_config(&machine_config::VmConfig {
                vcpu_count: Some(cpus),
                mem_size_mib: Some(memory_mib as usize),
                ..Default::default()
            })
            .map_err(|e| invalid(&e.to_string()))?;
        resources.enable_msb_metrics = false;
        resources.request_vsock = false;
        Ok(Self {
            resources,
            kernel: KernelOwner::default(),
            firmware: None,
            shares: BTreeMap::new(),
            implicit_init: cfg!(feature = "init-blob"),
        })
    }
    pub(crate) fn set_firmware_path(&mut self, path: PathBuf) -> io::Result<()> {
        if !path.is_absolute() {
            return Err(invalid("firmware path must be absolute"));
        }
        self.firmware = Some(crate::firmware_store::canonical_file(&path)?);
        Ok(())
    }
    pub(crate) fn embedded_kernel(
        &mut self,
        bytes: &[u8],
        guest_addr: u64,
        entry_addr: u64,
    ) -> io::Result<()> {
        if bytes.is_empty()
            || guest_addr & 4095 != 0
            || guest_addr
                .checked_add(bytes.len() as u64)
                .is_none_or(|end| entry_addr < guest_addr || entry_addr >= end)
        {
            return Err(invalid("invalid kernel geometry"));
        }
        let mut owner = KernelOwner::default();
        let bundle = owner.embedded(bytes, guest_addr, entry_addr)?;
        self.resources
            .set_kernel_bundle(bundle)
            .map_err(|e| invalid(&e.to_string()))?;
        self.kernel = owner;
        Ok(())
    }
    pub(crate) fn disable_implicit_init(&mut self) -> io::Result<()> {
        if self.shares.contains_key("/dev/root") {
            return Err(invalid("select init before attaching root"));
        }
        self.implicit_init = false;
        Ok(())
    }
    pub(crate) fn filesystem(
        &mut self,
        tag: &str,
        path: &Path,
        shm_size: usize,
        read_only: bool,
    ) -> io::Result<()> {
        if shm_size != 0 {
            return Err(unsupported(
                "WHP DAX is not part of the pVisor Windows profile",
            ));
        }
        if tag.is_empty() || tag.len() > 36 || tag.contains('\0') || self.shares.contains_key(tag) {
            return Err(invalid("invalid or duplicate filesystem tag"));
        }
        if !path.is_absolute() || !path.is_dir() {
            return Err(invalid("filesystem must be an existing absolute directory"));
        }
        let root = path.canonicalize()?;
        let root_dir = root
            .to_str()
            .ok_or_else(|| invalid("filesystem path must be UTF-8"))?
            .to_owned();
        self.shares.insert(
            tag.to_owned(),
            Config {
                root_dir,
                read_only,
                implicit_init: self.implicit_init && tag == "/dev/root",
                ..Default::default()
            },
        );
        Ok(())
    }
    pub(crate) fn virtual_file(
        &mut self,
        tag: &str,
        path: &str,
        data: Arc<[u8]>,
        mode: u32,
        one_shot: bool,
    ) -> io::Result<()> {
        let share = self
            .shares
            .get_mut(tag)
            .ok_or_else(|| invalid("unknown filesystem tag"))?;
        let name = path.strip_prefix('/').unwrap_or(path);
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.contains('\0')
            || name.contains('\\')
            || mode & !0o7777 != 0
        {
            return Err(invalid("invalid virtual file path or mode"));
        }
        if name.contains('/') {
            return Err(unsupported(
                "WHP virtual files currently require a root-level name",
            ));
        }
        if share.synthetic.contains_key(name) || (name == "init.krun" && share.implicit_init) {
            return Err(invalid("duplicate virtual file"));
        }
        share.synthetic.insert(
            name.to_owned(),
            SyntheticFile {
                bytes: data,
                mode,
                one_shot,
            },
        );
        Ok(())
    }
    pub(crate) fn run(
        mut self,
        on_ready: impl FnOnce(VmmHandle) -> io::Result<()>,
    ) -> io::Result<()> {
        if self.resources.kernel_bundle.is_none() {
            let bundle = if let Some(image) = crate::firmware::embedded_kernel() {
                self.kernel
                    .embedded(&image.bytes, image.guest_address, image.entry_address)?
            } else {
                let path = match &self.firmware {
                    Some(path) => path.clone(),
                    None => crate::firmware_store::resolve(None)?,
                };
                self.kernel.load(&path)?
            };
            self.resources
                .set_kernel_bundle(bundle)
                .map_err(|e| io::Error::other(e.to_string()))?;
        }
        if !self.shares.contains_key("/dev/root") {
            return Err(invalid("WHP boot requires a /dev/root filesystem"));
        }
        self.resources
            .set_kernel_cmdline(KernelCmdlineConfig {
                prolog: Some(format!("{DEFAULT_KERNEL_CMDLINE} init=/init.krun")),
                krun_env: Some("     ".into()),
                epilog: Some(" -- ".into()),
            })
            .map_err(|e| io::Error::other(e.to_string()))?;
        for (fs_id, config) in self.shares {
            self.resources.fs.push(FsDeviceConfig {
                fs_id,
                shared_dir: config.root_dir.clone(),
                passthrough: config,
                shm_size: None,
            });
        }
        self.resources.virtio_consoles.push(
            crate::vmm::resources::VirtioConsoleConfigMode::Explicit(vec![
                crate::vmm::resources::PortConfig::HostConsole,
            ]),
        );
        let mut events = crate::polly::event_manager::EventManager::new()
            .map_err(|e| io::Error::other(format!("event manager: {e:?}")))?;
        let (sender, _receiver) = crossbeam_channel::unbounded();
        let exit = crate::utils::eventfd::EventFd::new(crate::utils::eventfd::EFD_NONBLOCK)?;
        let vm = crate::vmm::builder::build_microvm(
            &mut self.resources,
            &mut events,
            None,
            sender,
            exit,
            Arc::new(AtomicI32::new(i32::MAX)),
        )
        .map_err(|e| io::Error::other(format!("WHP boot: {e:?}")))?;
        // Devices own their configuration now; release duplicate synthetic bytes.
        self.resources.fs.clear();
        on_ready(VmmHandle {
            idle: Arc::new(crate::guest_idle::IdleGate),
            vmm: Arc::downgrade(&vm),
            transition: Arc::new(Mutex::new(())),
            cold_pager_started: Arc::new(AtomicBool::new(false)),
        })?;
        vm.lock()
            .map_err(|_| io::Error::other("poisoned VMM"))?
            .set_paused(false)
            .map_err(|e| io::Error::other(e.to_string()))?;
        loop {
            events
                .run()
                .map_err(|e| io::Error::other(format!("WHP event loop: {e:?}")))?;
        }
    }
}
