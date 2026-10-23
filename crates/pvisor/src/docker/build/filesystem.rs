use anyhow::{bail, ensure};
use pvisor_overlay_core::{OverlayCore, sys};
use std::{
    collections::{BTreeMap, VecDeque},
    ffi::{CString, OsStr, OsString},
    fs::{self, File, OpenOptions},
    io,
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{
            ffi::{OsStrExt, OsStringExt},
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        },
    },
    path::{Component, Path, PathBuf},
};

pub(super) struct Ignore(Vec<(bool, globset::GlobMatcher)>);
impl Ignore {
    pub fn new(text: &str) -> anyhow::Result<Self> {
        let mut patterns = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') || line == "." {
                continue;
            }
            let (include, pattern) = match line.strip_prefix('!') {
                Some(p) => (true, p),
                None => (false, line),
            };
            let pattern = pattern.trim_matches('/');
            ensure!(!pattern.is_empty(), "empty .dockerignore pattern");
            patterns.push((
                include,
                globset::GlobBuilder::new(pattern)
                    .literal_separator(true)
                    .backslash_escape(true)
                    .build()?
                    .compile_matcher(),
            ));
        }
        Ok(Self(patterns))
    }
    fn excluded(&self, path: &Path) -> bool {
        let mut excluded = false;
        for (include, pattern) in &self.0 {
            if path.ancestors().any(|p| pattern.is_match(p)) {
                excluded = !*include;
            }
        }
        excluded
    }
}

fn names(directory: &File) -> anyhow::Result<Vec<OsString>> {
    let fd = unsafe { libc::dup(directory.as_raw_fd()) };
    ensure!(fd >= 0, "{}", io::Error::last_os_error());
    let dir = unsafe { libc::fdopendir(fd) };
    if dir.is_null() {
        unsafe { libc::close(fd) };
        bail!("{}", io::Error::last_os_error());
    }
    struct Guard(*mut libc::DIR);
    impl Drop for Guard {
        fn drop(&mut self) {
            unsafe { libc::closedir(self.0) };
        }
    }
    let _guard = Guard(dir);
    let mut result = Vec::new();
    // Readdir errors are detected by checking the directory binding again below;
    // errno is reset through the platform-specific libc accessor.
    loop {
        #[cfg(target_os = "macos")]
        unsafe {
            *libc::__error() = 0;
        }
        #[cfg(target_os = "linux")]
        unsafe {
            *libc::__errno_location() = 0;
        }
        let entry = unsafe { libc::readdir(dir) };
        if entry.is_null() {
            let e = io::Error::last_os_error();
            if e.raw_os_error() != Some(0) {
                return Err(e.into());
            }
            break;
        }
        let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) }.to_bytes();
        if name != b"." && name != b".." {
            result.push(OsString::from_vec(name.to_vec()));
        }
    }
    result.sort();
    Ok(result)
}

fn open_at(directory: &File, name: &OsStr, directory_only: bool) -> anyhow::Result<File> {
    let name = CString::new(name.as_bytes())?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY
                | libc::O_NOFOLLOW
                | libc::O_NONBLOCK
                | libc::O_CLOEXEC
                | if directory_only { libc::O_DIRECTORY } else { 0 },
        )
    };
    if fd < 0 {
        return Err(io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

/// Copy through directory descriptors. A swapped source symlink cannot read host secrets.
pub(super) fn snapshot(source: &Path, destination: &Path, ignore: &Ignore) -> anyhow::Result<()> {
    let root = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW)
        .open(source)?;
    fs::create_dir(destination)?;
    fn walk(
        source: &File,
        dest: &Path,
        rel: &Path,
        ignore: &Ignore,
        links: &mut BTreeMap<(u64, u64), PathBuf>,
    ) -> anyhow::Result<()> {
        for name in names(source)? {
            let child = rel.join(&name);
            let target = dest.join(&name);
            let cname = CString::new(name.as_bytes())?;
            let mut st: libc::stat = unsafe { std::mem::zeroed() };
            ensure!(
                unsafe {
                    libc::fstatat(
                        source.as_raw_fd(),
                        cname.as_ptr(),
                        &mut st,
                        libc::AT_SYMLINK_NOFOLLOW,
                    )
                } == 0,
                "context changed: {}",
                io::Error::last_os_error()
            );
            let kind = st.st_mode & libc::S_IFMT;
            if kind == libc::S_IFDIR {
                if ignore.excluded(&child) && !ignore.0.iter().any(|(include, _)| *include) {
                    continue;
                }
                let dir = open_at(source, &name, true)?;
                fs::create_dir(&target)?;
                walk(&dir, &target, &child, ignore, links)?;
                if ignore.excluded(&child) && fs::read_dir(&target)?.next().is_none() {
                    fs::remove_dir(&target)?;
                }
            } else if !ignore.excluded(&child) {
                if kind == libc::S_IFREG {
                    let mut input = open_at(source, &name, false)?;
                    let before = input.metadata()?;
                    ensure!(before.is_file(), "context source changed type");
                    let key = (before.dev(), before.ino());
                    if let Some(previous) = links.get(&key) {
                        fs::hard_link(previous, &target)?;
                        continue;
                    }
                    let mut output = File::create(&target)?;
                    io::copy(&mut input, &mut output)?;
                    let after = input.metadata()?;
                    ensure!(
                        (
                            before.dev(),
                            before.ino(),
                            before.len(),
                            before.mtime(),
                            before.mtime_nsec(),
                            before.ctime(),
                            before.ctime_nsec()
                        ) == (
                            after.dev(),
                            after.ino(),
                            after.len(),
                            after.mtime(),
                            after.mtime_nsec(),
                            after.ctime(),
                            after.ctime_nsec()
                        ),
                        "context file changed while copying: {}",
                        child.display()
                    );
                    set_identity(&target, 0, 0, before.mode() & 0o7777)?;
                    output.set_times(fs::FileTimes::new().set_modified(before.modified()?))?;
                    if before.nlink() > 1 {
                        links.insert(key, target);
                    }
                } else if kind == libc::S_IFLNK {
                    let mut bytes = vec![0; 65536];
                    let n = unsafe {
                        libc::readlinkat(
                            source.as_raw_fd(),
                            cname.as_ptr(),
                            bytes.as_mut_ptr().cast(),
                            bytes.len(),
                        )
                    };
                    ensure!(
                        n >= 0 && (n as usize) < bytes.len(),
                        "invalid context symlink"
                    );
                    bytes.truncate(n as usize);
                    std::os::unix::fs::symlink(OsString::from_vec(bytes), &target)?;
                } else {
                    bail!("unsupported context special file: {}", child.display());
                }
            }
        }
        set_identity(dest, 0, 0, source.metadata()?.mode() & 0o7777)?;
        Ok(())
    }
    walk(
        &root,
        destination,
        Path::new(""),
        ignore,
        &mut BTreeMap::new(),
    )
}

pub(super) fn path(value: &str, cwd: &Path) -> anyhow::Result<PathBuf> {
    ensure!(!value.contains('\0'), "path contains NUL");
    let input = if value.starts_with('/') {
        PathBuf::from(value)
    } else {
        cwd.join(value)
    };
    let mut clean = PathBuf::new();
    for component in input.components() {
        match component {
            Component::Normal(n) => {
                ensure!(
                    !OverlayCore::is_whiteout_name(n),
                    "reserved overlay/OCI path"
                );
                clean.push(n);
            }
            Component::ParentDir => {
                clean.pop();
            }
            Component::RootDir | Component::CurDir => {}
            _ => bail!("invalid guest path"),
        }
    }
    Ok(clean)
}

pub(super) fn resolve(
    core: &OverlayCore,
    path: &Path,
    follow_last: bool,
) -> anyhow::Result<PathBuf> {
    let mut pending: VecDeque<_> = path
        .components()
        .map(|c| c.as_os_str().to_owned())
        .collect();
    let mut resolved = PathBuf::new();
    let mut links = 0;
    while let Some(name) = pending.pop_front() {
        if name == "." {
            continue;
        }
        if name == ".." {
            resolved.pop();
            continue;
        }
        ensure!(
            !OverlayCore::is_whiteout_name(&name),
            "reserved overlay path"
        );
        resolved.push(&name);
        if (!pending.is_empty() || follow_last)
            && let Some(entry) = core.resolve_checked(&resolved)?
        {
            let meta = fs::symlink_metadata(&entry.path)?;
            if meta.file_type().is_symlink() {
                links += 1;
                ensure!(links <= 40, "too many guest symlinks");
                let target = fs::read_link(&entry.path)?;
                resolved.pop();
                if target.is_absolute() {
                    resolved.clear();
                }
                let mut next: VecDeque<_> = target
                    .components()
                    .filter_map(|c| match c {
                        Component::RootDir => None,
                        _ => Some(c.as_os_str().to_owned()),
                    })
                    .collect();
                next.append(&mut pending);
                pending = next;
            } else {
                ensure!(
                    pending.is_empty() || meta.is_dir(),
                    "guest path traverses non-directory"
                );
            }
        }
    }
    Ok(resolved)
}

pub(super) fn identity(path: &Path, metadata: &fs::Metadata) -> anyhow::Result<(u32, u32, u32)> {
    let mut result = (metadata.uid(), metadata.gid(), metadata.mode());
    match sys::get_xattr(path, OsStr::new("user.containers.override_stat")) {
        Ok(bytes) => {
            let text = std::str::from_utf8(&bytes)?;
            let fields: Vec<_> = text.split(':').collect();
            ensure!(fields.len() == 3, "invalid virtual image metadata");
            if fields[0] != "x" {
                result.0 = fields[0].parse()?;
            }
            if fields[1] != "x" {
                result.1 = fields[1].parse()?;
            }
            if fields[2] != "x" {
                let mode = u32::from_str_radix(fields[2], 8)?;
                result.2 = if mode & 0o170000 == 0 {
                    (metadata.mode() & 0o170000) | mode
                } else {
                    mode
                };
            }
        }
        Err(e) if matches!(e.raw_os_error(), Some(libc::ENODATA | libc::ENOTSUP)) => {}
        #[cfg(target_os = "macos")]
        Err(e) if e.raw_os_error() == Some(libc::ENOATTR) => {}
        Err(e) => return Err(e.into()),
    }
    Ok(result)
}

pub(super) fn set_identity(path: &Path, uid: u32, gid: u32, mode: u32) -> anyhow::Result<()> {
    let m = fs::symlink_metadata(path)?;
    let full = (m.mode() & 0o170000) | (mode & 0o7777);
    if !m.file_type().is_symlink() {
        fs::set_permissions(
            path,
            fs::Permissions::from_mode((mode & 0o777) | if m.is_dir() { 0o700 } else { 0o600 }),
        )?;
    }
    let result = sys::set_xattr(
        path,
        OsStr::new("user.containers.override_stat"),
        format!("{uid}:{gid}:0{full:o}").as_bytes(),
        0,
    );
    // Linux cannot put user xattrs on symlinks. Preserve native ownership when possible.
    #[cfg(target_os = "linux")]
    if m.file_type().is_symlink() && result.is_err() {
        let p = CString::new(path.as_os_str().as_bytes())?;
        ensure!(
            unsafe { libc::lchown(p.as_ptr(), uid, gid) } == 0 || m.uid() == uid && m.gid() == gid,
            "cannot preserve symlink ownership on this host"
        );
        return Ok(());
    }
    result?;
    Ok(())
}

pub(super) fn mkdir(core: &OverlayCore, path: &Path) -> anyhow::Result<PathBuf> {
    let resolved = resolve(core, path, true)?;
    let mut parent = PathBuf::new();
    for c in resolved.components() {
        parent.push(c.as_os_str());
        match core.resolve_checked(&parent)? {
            Some(p) => ensure!(
                fs::symlink_metadata(p.path)?.is_dir(),
                "destination is not a directory"
            ),
            None => {
                core.create_dir(&parent, 0o755)?;
                set_identity(&core.upper_path(&parent), 0, 0, 0o755)?;
            }
        }
    }
    Ok(resolved)
}

pub(super) fn copy(
    core: &OverlayCore,
    source: &Path,
    destination: &Path,
    owner: (u32, u32),
    mode: Option<u32>,
    links: &mut BTreeMap<(u64, u64), PathBuf>,
) -> anyhow::Result<()> {
    let m = fs::symlink_metadata(source)?;
    if m.is_dir() {
        let target = mkdir(core, destination)?;
        for e in fs::read_dir(source)? {
            let e = e?;
            copy(
                core,
                &e.path(),
                &target.join(e.file_name()),
                owner,
                mode,
                links,
            )?;
        }
        let p = core.prepare_metadata_change(&target)?;
        set_identity(
            &p,
            owner.0,
            owner.1,
            mode.unwrap_or(identity(source, &m)?.2 & 0o7777),
        )?;
        return Ok(());
    }
    let target = resolve(core, destination, true)?;
    mkdir(core, target.parent().unwrap_or(Path::new("")))?;
    // A later source may overwrite the first name we recorded for an inode.
    links.retain(|_, previous| previous != &target);
    if let Some(existing) = core.resolve_checked(&target)? {
        ensure!(
            !fs::symlink_metadata(existing.path)?.is_dir(),
            "COPY cannot replace directory with file"
        );
        core.remove(&target, false)?;
    }
    if m.file_type().is_symlink() {
        core.create_symlink(&target, &fs::read_link(source)?)?;
    } else {
        ensure!(m.is_file(), "COPY special files unsupported");
        let key = (m.dev(), m.ino());
        if let Some(previous) = links.get(&key) {
            core.hard_link(previous, &target)?;
            return Ok(());
        }
        let mut input = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(source)?;
        let mut output = core.create_file(&target, 0o600, libc::O_WRONLY | libc::O_TRUNC)?;
        io::copy(&mut input, &mut output)?;
        output.set_times(fs::FileTimes::new().set_modified(m.modified()?))?;
        if m.nlink() > 1 {
            links.insert(key, target.clone());
        }
    }
    set_identity(
        &core.upper_path(&target),
        owner.0,
        owner.1,
        mode.unwrap_or(identity(source, &m)?.2 & 0o7777),
    )?;
    Ok(())
}

pub(super) fn export(core: &OverlayCore, output: &Path) -> anyhow::Result<()> {
    let mut archive = tar::Builder::new(File::create(output)?);
    let mut links = BTreeMap::new();
    fn walk(
        core: &OverlayCore,
        archive: &mut tar::Builder<File>,
        rel: &Path,
        links: &mut BTreeMap<(u64, u64), PathBuf>,
    ) -> anyhow::Result<()> {
        let resolved = core.metadata_resolved(rel)?;
        let m = resolved.metadata;
        let backing = resolved.resolved.path;
        let (uid, gid, mode) = identity(&backing, &m)?;
        ensure!(
            matches!(mode & 0o170000, 0o100000 | 0o040000 | 0o120000),
            "unsupported image special file: {}",
            rel.display()
        );
        let mut header = tar::Header::new_gnu();
        header.set_uid(uid as u64);
        header.set_gid(gid as u64);
        header.set_mode(mode & 0o7777);
        header.set_mtime(m.mtime().max(0) as u64);
        let name = if rel.as_os_str().is_empty() {
            Path::new(".")
        } else {
            rel
        };
        let mut pax = Vec::new();
        for key in sys::list_xattrs(&backing)? {
            let key = OsStr::from_bytes(&key);
            let text = key.to_string_lossy();
            ensure!(
                text != "security.capability",
                "file capabilities cannot yet be safely exported"
            );
            if text.starts_with("user.")
                && text != "user.containers.override_stat"
                && !text.starts_with("user.overlay.")
            {
                pax.push((
                    format!("SCHILY.xattr.{text}"),
                    sys::get_xattr(&backing, key)?,
                ));
            }
        }
        if !pax.is_empty() {
            archive.append_pax_extensions(pax.iter().map(|(k, v)| (k.as_str(), v.as_slice())))?;
        }
        if m.is_dir() {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_cksum();
            archive.append_data(&mut header, name, io::empty())?;
            for child in core.list_names(rel)? {
                walk(core, archive, &rel.join(child), links)?;
            }
        } else if m.file_type().is_symlink() {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            archive.append_link(&mut header, name, fs::read_link(&backing)?)?;
        } else if m.is_file() {
            let identity = (m.dev(), m.ino());
            if m.nlink() > 1 && links.contains_key(&identity) {
                header.set_entry_type(tar::EntryType::Link);
                header.set_size(0);
                archive.append_link(&mut header, name, &links[&identity])?;
            } else {
                links.insert(identity, rel.to_path_buf());
                header.set_size(m.len());
                header.set_entry_type(tar::EntryType::Regular);
                header.set_cksum();
                archive.append_data(
                    &mut header,
                    name,
                    OpenOptions::new()
                        .read(true)
                        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
                        .open(&backing)?,
                )?;
            }
        } else {
            bail!("cannot export special image file: {}", rel.display());
        }
        Ok(())
    }
    walk(core, &mut archive, Path::new(""), &mut links)?;
    archive.finish()?;
    archive.into_inner()?.sync_all()?;
    Ok(())
}
