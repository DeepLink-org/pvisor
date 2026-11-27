// SPDX-License-Identifier: MPL-2.0

//! Virtio-fs filesystem wiring.
//!
//! This module defines the filesystem type and objects for virtio-fs.

use aster_fuse::{
    EntryReply, FUSE_ROOT_ID, FuseFileHandle, FuseNodeId, GetattrFlags, GetattrOperation,
    GetattrReq, Kstatfs, StatfsOperation,
};
use aster_virtio::device::filesystem::device::{self, AttrVersion, FileSystemDevice, FuseSession};
use device_id::DeviceId;

use super::{
    dir_index::DirIndex,
    inode::{InodeCache, VirtioFsInode},
};
use crate::{
    fs::{
        pseudofs::AnonDeviceId,
        utils::NAME_MAX,
        vfs::{
            file_system::{FileSystem, FsEventSubscriberStats, SuperBlock},
            inode::{Inode, Metadata},
            registry::{FsCache, FsCreationCtx, FsProperties, FsType},
        },
    },
    prelude::*,
    thread::work_queue::{self, WorkPriority},
};

/// Filesystem magic reported for virtio-fs in `statfs`.
const VIRTIOFS_MAGIC: u64 = 0x6573_5546;

/// Block size reported to `statfs` for virtio-fs.
const BLOCK_SIZE: usize = 4096;

/// The `virtiofs` filesystem type.
pub(super) struct VirtioFsType {
    cache: FsCache<String>,
}

pub(super) static VIRTIOFS_TYPE: VirtioFsType = VirtioFsType {
    cache: FsCache::new(),
};

impl FsType for VirtioFsType {
    type Key = String;

    fn name(&self) -> &'static str {
        "virtiofs"
    }

    fn properties(&self) -> FsProperties {
        FsProperties::empty()
    }

    fn create(&self, fs_creation_ctx: &mut FsCreationCtx) -> Result<Arc<dyn FileSystem>> {
        let tag = fs_creation_ctx
            .source()
            .ok_or_else(|| Error::with_message(Errno::EINVAL, "virtiofs source(tag) is required"))?
            .to_string();

        let device = device::find_device_by_tag(&tag)
            .ok_or_else(|| Error::with_message(Errno::ENODEV, "virtiofs device is not found"))?;

        Ok(VirtioFs::new(device, tag)? as Arc<dyn FileSystem>)
    }

    fn obtain_key_and_cache(
        &self,
        fs_creation_ctx: &mut FsCreationCtx,
    ) -> Option<(String, &FsCache<String>)> {
        let key = fs_creation_ctx.source().map(|tag| tag.to_string())?;

        Some((key, &self.cache))
    }

    fn sysnode(&self) -> Option<Arc<dyn aster_systree::SysNode>> {
        None
    }
}

/// A mounted virtio-fs filesystem.
pub(super) struct VirtioFs {
    sb: SuperBlock,
    root: Arc<VirtioFsInode>,
    tag: String,
    session: Arc<FuseSession>,
    dir_index: DirIndex,
    inode_cache: InodeCache,
    pending_forgets: Mutex<PendingForgets>,
    fs_event_subscriber_stats: FsEventSubscriberStats,
}

type ForgetEntries = Vec<(FuseNodeId, u64)>;

#[derive(Default)]
struct PendingForgets {
    // Some means exactly one worker owns the right to take the tail, even if empty.
    tail: Option<ForgetEntries>,
}

enum ForgetWork {
    TakeTail,
    Full(ForgetEntries),
}

impl PendingForgets {
    fn push(&mut self, entry: (FuseNodeId, u64)) -> Option<ForgetWork> {
        let Some(tail) = &mut self.tail else {
            self.tail = Some(vec![entry]);
            return Some(ForgetWork::TakeTail);
        };
        tail.push(entry);
        (tail.len() == FuseSession::FORGET_BATCH_SIZE)
            .then(|| ForgetWork::Full(core::mem::take(tail)))
    }
}

impl VirtioFs {
    fn new(device: Arc<FileSystemDevice>, tag: String) -> Result<Arc<Self>> {
        let session = FuseSession::new(device)
            .map_err(|_| Error::with_message(Errno::EIO, "virtiofs FUSE_INIT failed"))?;

        let anon_device_id =
            AnonDeviceId::acquire().expect("no device ID is available for virtiofs");
        let container_dev_id = anon_device_id.id();
        let statfs = session.do_fuse_op(FUSE_ROOT_ID, StatfsOperation)?.st();

        // TODO: Update the super block fields based on `statfs` reply.
        // For now, we set only the fields required by VFS when mounting the filesystem.
        // No update is made for these fields later.
        let sb = SuperBlock::from((container_dev_id, statfs));

        let root_attr = session.do_fuse_op(
            FUSE_ROOT_ID,
            GetattrOperation::new(GetattrReq::new(
                GetattrFlags::empty(),
                FuseFileHandle::new(0),
            )),
        )?;

        Ok(Arc::new_cyclic(|weak_fs| {
            let root = VirtioFsInode::new_root(
                root_attr,
                weak_fs.clone(),
                container_dev_id,
                session.bump_attr_version(),
            );
            let inode_cache = InodeCache::new(&root);

            Self {
                sb,
                root,
                tag,
                dir_index: DirIndex::new(session.clone()),
                session,
                inode_cache,
                pending_forgets: Mutex::new(PendingForgets::default()),
                fs_event_subscriber_stats: FsEventSubscriberStats::new(),
            }
        }))
    }

    pub(super) fn session(&self) -> &Arc<FuseSession> {
        &self.session
    }

    pub(super) fn defer_forget(self: &Arc<Self>, nodeid: FuseNodeId, count: u64) {
        if count == 0 || nodeid == FUSE_ROOT_ID {
            return;
        }
        let Some(work) = self.pending_forgets.lock().push((nodeid, count)) else {
            return;
        };
        let fs = self.clone();
        work_queue::submit_work_func(
            move || {
                let result = match &work {
                    ForgetWork::Full(entries) => fs.session.forget_many(entries),
                    ForgetWork::TakeTail => {
                        let tail = fs.pending_forgets.lock().tail.take().unwrap();
                        fs.session.forget_many(&tail)
                    }
                };
                if let Err(err) = result {
                    warn!("virtiofs forget submission failed: {:?}", err);
                }
            },
            WorkPriority::Normal,
        );
    }

    pub(super) fn dir_index(&self) -> &DirIndex {
        &self.dir_index
    }

    /// Returns the device ID of this virtio-fs mount.
    pub(super) fn container_device_id(&self) -> DeviceId {
        self.sb.container_dev_id
    }

    /// Returns the inode and any accepted attributes from the entry reply.
    pub(super) fn lookup_inode_observed(
        self: &Arc<Self>,
        lookup_reply: EntryReply,
        request_attr_version: AttrVersion,
    ) -> Result<(Arc<VirtioFsInode>, Option<Metadata>)> {
        self.inode_cache
            .lookup_inode(lookup_reply, request_attr_version, self)
    }

    /// Inserts a newly created inode into the inode cache.
    pub(super) fn insert_inode_to_cache(&self, inode: &Arc<VirtioFsInode>) {
        self.inode_cache.insert_inode(inode);
    }

    /// Removes an inode from the cache if it is still the cached entry.
    pub(super) fn remove_inode_from_cache(
        &self,
        inode: &VirtioFsInode,
    ) -> Option<Weak<VirtioFsInode>> {
        self.inode_cache.remove_inode(inode)
    }
}

impl From<(DeviceId, Kstatfs)> for SuperBlock {
    fn from((container_dev_id, statfs): (DeviceId, Kstatfs)) -> Self {
        let mut sb = SuperBlock::new(VIRTIOFS_MAGIC, BLOCK_SIZE, NAME_MAX, container_dev_id);

        sb.blocks = statfs.blocks() as usize;
        sb.bfree = statfs.bfree() as usize;
        sb.bavail = statfs.bavail() as usize;
        sb.files = statfs.files() as usize;
        sb.ffree = statfs.ffree() as usize;
        sb.bsize = statfs.bsize() as usize;
        sb.namelen = statfs.namelen() as usize;
        sb.frsize = statfs.frsize() as usize;
        sb
    }
}

impl FileSystem for VirtioFs {
    fn name(&self) -> &'static str {
        "virtiofs"
    }

    fn source(&self) -> Option<&str> {
        Some(&self.tag)
    }

    fn sync(&self) -> Result<()> {
        match self.session.syncfs().map_err(Error::from) {
            // Older servers may not implement a backing barrier. This is not
            // an acknowledgement of durable host state.
            Err(err) if err.error() == Errno::ENOSYS => Ok(()),
            result => result,
        }
    }

    fn root_inode(&self) -> Arc<dyn Inode> {
        self.root.clone()
    }

    fn sb(&self) -> SuperBlock {
        self.sb.clone()
    }

    fn fs_event_subscriber_stats(&self) -> &FsEventSubscriberStats {
        &self.fs_event_subscriber_stats
    }
}

#[cfg(ktest)]
mod tests {
    use aster_fuse::{FuseOperation, ops::forget::BatchForgetOperation};
    use ostd::prelude::ktest;

    use super::*;

    #[ktest]
    fn reclaim_forget_ownership_and_counts() {
        let mut pending = PendingForgets::default();
        let entry = |i: usize| (FuseNodeId::new(2 + i as u64 % 3), (1u64 << 40) + i as u64);
        let n = FuseSession::FORGET_BATCH_SIZE;
        for i in 0..2 * n + 1 {
            match pending.push(entry(i)) {
                Some(ForgetWork::TakeTail) => assert_eq!(i, 0),
                Some(ForgetWork::Full(batch)) => {
                    assert_eq!((i + 1) % n, 0);
                    let mut op = BatchForgetOperation(&batch);
                    let mut bytes = vec![0xff; op.body_len()];
                    op.write_body(&mut VmWriter::from(bytes.as_mut_slice()))
                        .unwrap();
                    assert_eq!(bytes.len(), 8 + 16 * n);
                    assert_eq!(bytes[..4], (n as u32).to_ne_bytes());
                    assert_eq!(bytes[4..8], [0; 4]);
                    for (j, record) in bytes[8..].chunks_exact(16).enumerate() {
                        let (node, count) = entry(i + 1 - n + j);
                        assert_eq!(record[..8], node.as_u64().to_ne_bytes());
                        assert_eq!(record[8..], count.to_ne_bytes());
                    }
                }
                None => assert!(i != 0 && (i + 1) % n != 0),
            }
        }
        assert_eq!(pending.tail.take().unwrap(), [entry(2 * n)]);
        assert!(matches!(pending.push(entry(0)), Some(ForgetWork::TakeTail)));
        assert_eq!(pending.tail.take().unwrap(), [entry(0)]);
    }
}
