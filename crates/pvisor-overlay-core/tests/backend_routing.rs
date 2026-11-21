//! Attachment selection, relative-path validation and in-flight ownership.
use proptest::prelude::*;
use pvisor_overlay_core::backend::{self, BackendAttachment, FileAttr, ReadOnlyBackend};
use std::{
    ffi::OsStr,
    fs, io,
    os::unix::ffi::OsStrExt,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};

struct ReadGate {
    entered: mpsc::Sender<()>,
    resume: Mutex<mpsc::Receiver<()>>,
}

struct Probe {
    id: u8,
    reads: AtomicUsize,
    gate: Option<ReadGate>,
}

impl Probe {
    fn new(id: u8) -> Self {
        Self {
            id,
            reads: AtomicUsize::new(0),
            gate: None,
        }
    }
}

impl ReadOnlyBackend for Probe {
    fn prepare_metadata(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))
    }
    fn prepare_directory(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))
    }
    fn attributes(&self, _: &Path) -> io::Result<FileAttr> {
        Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))
    }
    fn read_at(&self, relative: &Path, offset: u64, size: u32) -> io::Result<Vec<u8>> {
        assert_eq!(offset, 0);
        self.reads.fetch_add(1, Ordering::SeqCst);
        if let Some(gate) = &self.gate {
            gate.entered.send(()).map_err(io::Error::other)?;
            gate.resume
                .lock()
                .map_err(|_| io::Error::other("read gate poisoned"))?
                .recv_timeout(Duration::from_secs(2))
                .map_err(io::Error::other)?;
        }
        let mut bytes = vec![self.id];
        bytes.extend(relative.as_os_str().as_bytes());
        assert!(bytes.len() <= size as usize);
        Ok(bytes)
    }
    fn materialize_file(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))
    }
    fn materialize_tree(&self, _: &Path) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(libc::EOPNOTSUPP))
    }
}

fn expected(id: u8, relative: &Path) -> Vec<u8> {
    let mut bytes = vec![id];
    bytes.extend(relative.as_os_str().as_bytes());
    bytes
}

fn assert_routing(relative: &Path, child_first: bool) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("root");
    let child = root.join("nested");
    fs::create_dir_all(&child).unwrap();
    let parent_backend = Arc::new(Probe::new(1));
    let child_backend = Arc::new(Probe::new(2));
    let (parent_owner, child_owner) = if child_first {
        let child_owner = BackendAttachment::new(&child, child_backend.clone()).unwrap();
        let parent_owner = BackendAttachment::new(&root, parent_backend.clone()).unwrap();
        (parent_owner, child_owner)
    } else {
        let parent_owner = BackendAttachment::new(&root, parent_backend.clone()).unwrap();
        let child_owner = BackendAttachment::new(&child, child_backend.clone()).unwrap();
        (parent_owner, child_owner)
    };
    for (path, id, relative) in [
        (root.clone(), 1, PathBuf::new()),
        (child.clone(), 2, PathBuf::new()),
        (child.join(relative), 2, relative.to_path_buf()),
        (
            root.join("nested-sibling").join(relative),
            1,
            Path::new("nested-sibling").join(relative),
        ),
    ] {
        assert_eq!(
            backend::read_at(&path, 0, 4096).unwrap(),
            Some(expected(id, &relative))
        );
    }
    let mut streamed = Vec::new();
    let bytes = expected(2, relative);
    assert_eq!(
        backend::read_into(&child.join(relative), 0, 4096, &mut streamed).unwrap(),
        Some(bytes.len())
    );
    assert_eq!(streamed, bytes);
    let before = (
        parent_backend.reads.load(Ordering::SeqCst),
        child_backend.reads.load(Ordering::SeqCst),
    );
    assert!(
        backend::read_at(&root.with_file_name("root-suffix").join(relative), 0, 4096)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        (
            parent_backend.reads.load(Ordering::SeqCst),
            child_backend.reads.load(Ordering::SeqCst)
        ),
        before
    );
    drop(child_owner);
    assert_eq!(
        backend::read_at(&child.join(relative), 0, 4096).unwrap(),
        Some(expected(1, &Path::new("nested").join(relative)))
    );
    drop(parent_owner);
    assert!(
        backend::read_at(&child.join(relative), 0, 4096)
            .unwrap()
            .is_none()
    );
}

#[rstest::rstest]
#[case::leaf(b"file")]
#[case::nested(b"dir/file")]
#[case::unicode("目录/café".as_bytes())]
#[case::raw_bytes(b"dir/\xff-file")]
fn longest_component_prefix_routes_without_losing_path_bytes(
    #[case] relative: &[u8],
    #[values(false, true)] child_first: bool,
) {
    assert_routing(Path::new(OsStr::from_bytes(relative)), child_first);
}

proptest! {
    #[test]
    fn generated_routes_preserve_selection_and_owner_retirement(
        components in prop::collection::vec(
            prop_oneof!["[a-zA-Z0-9_-]{1,12}", Just("目录".to_owned()), Just("café".to_owned())],
            1..5
        ),
        child_first in any::<bool>(),
    ) {
        let relative: PathBuf = components.into_iter().collect();
        assert_routing(&relative, child_first);
    }
}

#[rstest::rstest]
#[case::parent("../escape")]
#[case::embedded_parent("dir/../../escape")]
#[case::terminal_parent("dir/..")]
fn invalid_selected_relative_paths_never_fall_back_or_call_a_backend(#[case] relative: &str) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let child = root.join("child");
    fs::create_dir(&child).unwrap();
    let parent_backend = Arc::new(Probe::new(1));
    let child_backend = Arc::new(Probe::new(2));
    let _parent = BackendAttachment::new(&root, parent_backend.clone()).unwrap();
    let _child = BackendAttachment::new(&child, child_backend.clone()).unwrap();
    assert_eq!(
        backend::read_at(&child.join(relative), 0, 4096)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::EINVAL)
    );
    assert_eq!(parent_backend.reads.load(Ordering::SeqCst), 0);
    assert_eq!(child_backend.reads.load(Ordering::SeqCst), 0);
}

#[rstest::rstest]
fn an_inflight_read_pins_its_attachment_without_holding_the_registry_lock() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap();
    let (entered, started) = mpsc::channel();
    let (resume, proceed) = mpsc::channel();
    let mut probe = Probe::new(1);
    probe.gate = Some(ReadGate {
        entered,
        resume: Mutex::new(proceed),
    });
    let owner = BackendAttachment::new(&root, Arc::new(probe)).unwrap();
    let path = root.join("file");
    let (done, response) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        done.send(backend::read_at(&path, 0, 4096)).unwrap();
    });
    started.recv_timeout(Duration::from_secs(2)).unwrap();
    drop(owner);
    let registration_root = root.clone();
    let (done, checked) = mpsc::channel();
    let registrar = std::thread::spawn(move || {
        let result = BackendAttachment::new(&registration_root, Arc::new(Probe::new(2)));
        assert!(done.send(result).is_ok());
    });
    let registration = checked.recv_timeout(Duration::from_secs(1));
    resume.send(()).unwrap();
    assert_eq!(
        registration.unwrap().err().unwrap().raw_os_error(),
        Some(libc::EEXIST)
    );
    assert_eq!(
        response
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap(),
        Some(expected(1, Path::new("file")))
    );
    reader.join().unwrap();
    registrar.join().unwrap();
    let _replacement = BackendAttachment::new(&root, Arc::new(Probe::new(2))).unwrap();
    assert_eq!(
        backend::read_at(&root.join("file"), 0, 4096).unwrap(),
        Some(expected(2, Path::new("file")))
    );
}
