//! Manual screening of durable publication cost, not a production WAL format.
//! The append prototype has one writer and no recovery, consumption or winner
//! adoption. Its timing is a mechanism estimate, not a substitute for Core.

use super::*;
use std::io::{BufRead, BufReader, Write};
use std::time::Instant;

#[test]
#[ignore = "manual durable per-path publication versus append-log screening"]
fn append_wal_screening() {
    screen(true, 1, 5);
}

#[test]
#[ignore = "manual read-observation publication versus append-log screening"]
fn read_observation_append_screening() {
    screen(false, 3, 15);
}

fn screen(durable: bool, warmups: usize, samples: usize) {
    for count in [64, 2048] {
        for round in 0..warmups + samples {
            let temp = tempfile::tempdir().unwrap();
            let target = temp.path().join("target");
            fs::create_dir(&target).unwrap();
            let journal = temp.path().join("preimages");
            let core = OverlayCore::new_with_exclusions_and_preimages(
                vec![target.clone()],
                temp.path().join("upper"),
                Some(temp.path().join("work")),
                Vec::new(),
                Some(journal.clone()),
            )
            .unwrap();
            // Initialize and publish this empty log before timing appends.
            // Each later record is fully synced before its simulated mutation.
            let wal_path = temp.path().join("prototype.jsonl");
            let mut wal = OpenOptions::new()
                .create_new(true)
                .write(true)
                .mode(0o600)
                .open(&wal_path)
                .unwrap();
            wal.sync_all().unwrap();
            File::open(temp.path()).unwrap().sync_all().unwrap();
            let paths = (0..count)
                .map(|index| PathBuf::from(format!("file-{index:04}")))
                .collect::<Vec<_>>();
            if !durable {
                for path in &paths {
                    fs::write(target.join(path), [0x5a; 4096]).unwrap();
                }
            }
            let compact_directory = temp.path().join("compact-preimages");
            let framed = OverlayCore::new_with_exclusions_and_preimages(
                vec![target.clone()],
                temp.path().join("compact-upper"),
                None,
                Vec::new(),
                Some(compact_directory.clone()),
            )
            .unwrap()
            .with_compact_preimages()
            .unwrap();
            let framed_path = compact_directory.join(PREIMAGE_LOG_NAME);
            let mut elapsed = [0u128; 3];
            let mut order = [0, 1, 2];
            order.rotate_left(round % 3);
            if round % 2 != 0 {
                order.reverse();
            }
            for mechanism in order {
                let started = Instant::now();
                for path in &paths {
                    if mechanism == 1 {
                        // Include the same baseline fingerprint work; the
                        // read case also includes the complete content scan.
                        let preimage = PathPreimage {
                            path: path.as_os_str().as_bytes().to_vec(),
                            state: fingerprint_at(&target, path).unwrap(),
                        };
                        let mut bytes = serde_json::to_vec(&preimage).unwrap();
                        bytes.push(b'\n');
                        wal.write_all(&bytes).unwrap();
                        if durable {
                            wal.sync_all().unwrap();
                        }
                    } else if mechanism == 2 {
                        framed.capture_preimage(path, durable).unwrap();
                    } else {
                        core.capture_preimage(path, durable).unwrap();
                    }
                }
                elapsed[mechanism] = started.elapsed().as_nanos();
            }
            let actual = load_preimages(&journal).unwrap();
            let prototype = BufReader::new(File::open(&wal_path).unwrap())
                .lines()
                .map(|line| serde_json::from_str::<PathPreimage>(&line.unwrap()).unwrap())
                .collect::<Vec<_>>();
            let framed = crate::preimage_log::PreimageLog::read(&framed_path).unwrap();
            assert_eq!(actual.len(), count);
            assert_eq!(prototype.len(), count);
            assert_eq!(framed.len(), count);
            for ((record, candidate), framed) in actual.iter().zip(&prototype).zip(&framed) {
                assert_eq!(record.path, candidate.path);
                assert_eq!(record.state, candidate.state);
                assert_eq!(record.path, framed.path);
                assert_eq!(record.state, framed.state);
                if durable {
                    assert_eq!(candidate.state, PathFingerprint::Absent);
                } else {
                    assert!(matches!(candidate.state, PathFingerprint::File { .. }));
                }
            }
            if round >= warmups {
                println!(
                    "PVISOR_JOURNAL_APPEND_SCREEN {}",
                    serde_json::json!({
                        "paths": count, "round": round - warmups,
                        "mode": if durable { "mutation" } else { "read_observation" },
                        "source_bytes_per_file": if durable { 0 } else { 4096 },
                        "per_path_ns": elapsed[0], "append_ns": elapsed[1], "framed_ns": elapsed[2],
                        "order": order, "framed_implementation": "OverlayCore compact journal integration",
                        "wal_bytes": wal.metadata().unwrap().len(),
                        "framed_bytes": fs::metadata(&framed_path).unwrap().len(),
                        "prototype": "single-writer; no recovery/consume/concurrent adoption",
                    })
                );
            }
        }
    }
}

#[test]
#[ignore = "manual immutable content receipt versus source scan measurement"]
fn immutable_content_receipt_screening() {
    for count in [64, 2048] {
        let temp = tempfile::tempdir().unwrap();
        let baseline = temp.path().join("baseline");
        fs::create_dir(&baseline).unwrap();
        let paths = (0..count)
            .map(|i| PathBuf::from(format!("file-{i:04}")))
            .collect::<Vec<_>>();
        let content = [0x5a; 4096];
        let content_sha = sha256_hex(&content);
        for path in &paths {
            fs::write(baseline.join(path), content).unwrap();
        }
        let index = temp.path().join("content-index");
        let bytes = crate::encode_content_index(
            paths
                .iter()
                .map(|path| (path.as_path(), content_sha.as_str())),
        )
        .unwrap();
        fs::write(&index, &bytes).unwrap();
        let digest = sha256_hex(&bytes);
        for round in 0..18 {
            let mut cores = Vec::new();
            for indexed in [false, true] {
                let core = OverlayCore::new_with_exclusions_and_preimages(
                    vec![baseline.clone()],
                    temp.path().join(format!("upper-{round}-{indexed}")),
                    None,
                    vec![],
                    Some(temp.path().join(format!("journal-{round}-{indexed}"))),
                )
                .unwrap()
                .with_compact_preimages()
                .unwrap();
                cores.push(if indexed {
                    core.with_immutable_content_index(&baseline, index.clone(), &digest)
                        .unwrap()
                } else {
                    core
                });
            }
            let order = if round % 2 == 0 { [0, 1] } else { [1, 0] };
            let mut elapsed = [0; 2];
            for mechanism in order {
                let started = Instant::now();
                for path in &paths {
                    cores[mechanism].observe_read(path).unwrap();
                }
                elapsed[mechanism] = started.elapsed().as_nanos();
            }
            let left = load_preimages(cores[0].preimage_dir.as_ref().unwrap()).unwrap();
            let right = load_preimages(cores[1].preimage_dir.as_ref().unwrap()).unwrap();
            assert_eq!(left.len(), count);
            assert_eq!(right.len(), count);
            for (left, right) in left.iter().zip(&right) {
                assert_eq!(left.path, right.path);
                assert_eq!(left.state, right.state);
            }
            if round >= 3 {
                println!(
                    "PVISOR_CONTENT_RECEIPT_SCREEN {}",
                    serde_json::json!({
                        "files":count,"round":round-3,"source_bytes_per_file":4096,
                        "scan_ns":elapsed[0],"receipt_ns":elapsed[1],"index_bytes":bytes.len(),"order":order,
                        "lazy_index_load_included":true,"correctness":"passed"
                    })
                );
            }
        }
    }
}
