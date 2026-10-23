//! Operator-side preparation: publish a pinned compressed template, never pull
//! or unpack a registry image on the sandbox creation path.
use anyhow::{Context, ensure};
use clap::Args;
use pvisor_daemon::runtime::PreparedImage;
use std::{
    collections::BTreeMap, fs, io::Write, os::unix::fs::PermissionsExt, path::PathBuf,
    time::Duration,
};

#[derive(Args)]
pub struct PrepareImage {
    /// Local image key used by OpenSandbox create requests (not a registry URI).
    #[arg(long)]
    name: String,
    /// Registry base image, preferably pinned by sha256 manifest digest.
    #[arg(long)]
    image: String,
    /// Trusted statically linked Linux execd from the pinned OpenSandbox release.
    #[arg(long)]
    execd: PathBuf,
    /// Directory containing prepared manifests and the shared compressed store.
    #[arg(long)]
    images_dir: PathBuf,
    /// Dynamic host firmware directory; omitted for the packaged static daemon.
    #[arg(long)]
    library_dir: Option<PathBuf>,
}
impl PrepareImage {
    pub fn run(self) -> anyhow::Result<()> {
        ensure!(
            !self.name.is_empty()
                && self.name.len() <= 128
                && self
                    .name
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || b"-_.".contains(&c))
                && !self.name.starts_with('.'),
            "invalid local image key"
        );
        ensure!(
            !self.image.contains(['\n', '\r', '\0']) && !self.image.contains(char::is_whitespace),
            "invalid base image reference"
        );
        let execd = fs::read(&self.execd).context("read trusted execd binary")?;
        ensure!(
            execd.len() >= 64
                && &execd[..4] == b"\x7fELF"
                && execd[4] == 2
                && execd[5] == 1
                && u16::from_le_bytes([execd[18], execd[19]]) == 62,
            "execd must be a Linux x86-64 ELF binary"
        );
        // Reject an interpreter-dependent ELF before publishing a template that
        // may use a different distro/libc. No supplied executable runs on host.
        let phoff = usize::try_from(u64::from_le_bytes(execd[32..40].try_into()?))?;
        let phsize = usize::from(u16::from_le_bytes(execd[54..56].try_into()?));
        let phcount = usize::from(u16::from_le_bytes(execd[56..58].try_into()?));
        ensure!(
            phsize >= 56
                && phcount > 0
                && phoff
                    .checked_add(
                        phsize
                            .checked_mul(phcount)
                            .context("invalid ELF program headers")?
                    )
                    .is_some_and(|end| end <= execd.len()),
            "invalid execd ELF program headers"
        );
        ensure!(
            (0..phcount).all(|i| {
                let at = phoff + i * phsize;
                u32::from_le_bytes(execd[at..at + 4].try_into().unwrap()) != 3
            }),
            "execd must be statically linked (build with CGO_ENABLED=0)"
        );
        fs::create_dir_all(&self.images_dir)?;
        let images = self.images_dir.canonicalize()?;
        let store_root = images.join("store");
        let store = pvisor::docker::Store::open(store_root.clone())?;
        let context = tempfile::tempdir()?;
        for (name, bytes) in [
            ("execd", execd.as_slice()),
            (
                "sandbox-init",
                include_bytes!(env!("PVISOR_SANDBOX_INIT")).as_slice(),
            ),
        ] {
            let path = context.path().join(name);
            fs::write(&path, bytes)?;
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755))?;
        }
        let dockerfile = context.path().join("Dockerfile");
        fs::write(
            &dockerfile,
            format!(
                "FROM {}\nCOPY execd /opt/pvisor/execd\nCOPY sandbox-init /opt/pvisor/sandbox-init\nENTRYPOINT [\"/opt/pvisor/sandbox-init\"]\nCMD [\"/bin/sleep\",\"infinity\"]\n",
                self.image
            ),
        )?;
        let image = pvisor::docker::build::build(
            &store,
            &pvisor::docker::build::BuildOptions {
                context: context.path().into(),
                file: dockerfile,
                tags: vec![],
                build_args: BTreeMap::new(),
                target: None,
                platform: Some("linux/amd64".into()),
                pull: false,
                cpus: 1,
                memory_mib: 512,
                network: false,
                timeout: Duration::from_secs(300),
                quiet: true,
                output: None,
            },
        )?;
        ensure!(
            image.cache.is_some(),
            "prepared template must use the compressed image pool"
        );
        let manifest = PreparedImage {
            rootfs: store_root,
            entrypoint: vec!["/opt/pvisor/sandbox-init".into()],
            cmd: vec!["/bin/sleep".into(), "infinity".into()],
            env: image.env.clone(),
            library_dir: self.library_dir.map(|p| p.canonicalize()).transpose()?,
            e2b: None,
            pooled_image: Some(image),
            native_opensandbox: true,
        };
        // Atomic publication leaves the previous manifest intact on failure.
        let mut file = tempfile::NamedTempFile::new_in(&images)?;
        serde_json::to_writer_pretty(file.as_file_mut(), &manifest)?;
        file.as_file_mut().write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(images.join(format!("{}.json", self.name)))?;
        fs::File::open(&images)?.sync_all()?;
        println!("{}", self.name);
        Ok(())
    }
}
