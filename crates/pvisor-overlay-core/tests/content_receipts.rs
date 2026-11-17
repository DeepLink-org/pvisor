//! Public receipt encoding and its first-touch publication contract.
use proptest::prelude::*;
use proptest::test_runner::FileFailurePersistence;
use pvisor_overlay_core::{
    OverlayCore, PathFingerprint, encode_content_index, fingerprint_at, load_preimages,
};
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
};

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn encode(inventory: &[(PathBuf, String)]) -> Vec<u8> {
    encode_content_index(
        inventory
            .iter()
            .map(|(path, digest)| (path.as_path(), digest.as_str())),
    )
    .unwrap()
}

struct ReceiptFixture {
    _root: tempfile::TempDir,
    lower: PathBuf,
    upper: PathBuf,
    journal: PathBuf,
    receipt: PathBuf,
    digest: String,
    baseline: PathFingerprint,
}

impl ReceiptFixture {
    fn new(payload: &[u8]) -> Self {
        let root = tempfile::tempdir().unwrap();
        let lower = root.path().join("lower");
        fs::create_dir(&lower).unwrap();
        fs::write(lower.join("file"), payload).unwrap();
        let baseline = fingerprint_at(&lower, Path::new("file")).unwrap();
        let bytes =
            encode_content_index([(Path::new("file"), hex(&Sha256::digest(payload)).as_str())])
                .unwrap();
        let receipt = root.path().join("index");
        fs::write(&receipt, &bytes).unwrap();
        Self {
            lower,
            upper: root.path().join("upper"),
            journal: root.path().join("journal"),
            receipt,
            digest: hex(&Sha256::digest(&bytes)),
            baseline,
            _root: root,
        }
    }

    fn open(&self, compact: bool) -> OverlayCore {
        let core = OverlayCore::new_with_exclusions_and_preimages(
            vec![self.lower.clone()],
            self.upper.clone(),
            None,
            vec![],
            Some(self.journal.clone()),
        )
        .unwrap();
        let core = if compact {
            core.with_compact_preimages().unwrap()
        } else {
            core
        };
        core.with_immutable_content_index(&self.lower, self.receipt.clone(), &self.digest)
            .unwrap()
    }
}

proptest! {
    #![proptest_config(ProptestConfig {
        failure_persistence: Some(Box::new(FileFailurePersistence::WithSource("proptest-regressions"))),
        ..ProptestConfig::default()
    })]

    #[test]
    fn encoding_is_independent_of_inventory_order(
        inventory in prop::collection::btree_map("[a-z][a-z0-9]{0,15}", any::<[u8; 32]>(), 0..33),
        rotation in any::<usize>(),
    ) {
        let mut records: Vec<_> = inventory.into_iter()
            .map(|(name, digest)| (PathBuf::from(name), hex(&digest))).collect();
        let expected = encode(&records);
        records.reverse();
        if !records.is_empty() {
            let offset = rotation % records.len();
            records.rotate_left(offset);
        }
        prop_assert_eq!(encode(&records), expected);
    }

    #[test]
    fn duplicate_paths_are_rejected_for_arbitrary_digests(
        name in "[a-z][a-z0-9]{0,15}",
        first in any::<[u8; 32]>(),
        second in any::<[u8; 32]>(),
    ) {
        let first = hex(&first);
        let second = hex(&second);
        let error = encode_content_index([
            (Path::new(&name), first.as_str()),
            (Path::new(&name), second.as_str()),
        ]).unwrap_err();
        prop_assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
    }

    #[test]
    fn verified_receipts_preserve_native_baselines_and_copied_bytes(
        payload in prop::collection::vec(any::<u8>(), 0..4097),
        compact in any::<bool>(),
    ) {
        let fixture = ReceiptFixture::new(&payload);
        let core = fixture.open(compact);
        let copied = core.copy_up(Path::new("file")).unwrap();
        let copied_bytes = fs::read(copied).unwrap();
        prop_assert_eq!(&copied_bytes, &payload);
        drop(core);
        let observations = load_preimages(&fixture.journal).unwrap();
        let observed = observations.iter()
            .find(|entry| entry.relative_path() == Path::new("file")).unwrap();
        prop_assert_eq!(&observed.state, &fixture.baseline);
        prop_assert_eq!(fs::read(fixture.lower.join("file")).unwrap(), payload);
        prop_assert_eq!(fingerprint_at(&fixture.lower, Path::new("file")).unwrap(), fixture.baseline);
    }

    #[test]
    fn corrupt_receipts_fail_before_file_observation_or_upper_publication(
        payload in prop::collection::vec(any::<u8>(), 0..4097),
        offset in any::<usize>(),
        compact in any::<bool>(),
    ) {
        let fixture = ReceiptFixture::new(&payload);
        let mut bytes = fs::read(&fixture.receipt).unwrap();
        let index = offset % bytes.len();
        bytes[index] ^= 1;
        fs::write(&fixture.receipt, bytes).unwrap();
        let core = fixture.open(compact);
        let error = core.copy_up(Path::new("file")).unwrap_err();
        prop_assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        prop_assert!(!fixture.upper.join("file").exists());
        prop_assert_eq!(fs::read_dir(&fixture.upper).unwrap().count(), 0);
        drop(core);
        prop_assert!(!load_preimages(&fixture.journal).unwrap().iter()
            .any(|entry| entry.relative_path() == Path::new("file")));
        prop_assert_eq!(fs::read(fixture.lower.join("file")).unwrap(), payload);
        prop_assert_eq!(fingerprint_at(&fixture.lower, Path::new("file")).unwrap(), fixture.baseline);
    }
}
