//! Revalidate persisted suspension evidence using Core alone. The files below
//! are fake caller-owned storage, not the runtime's completion/outbox protocol.
use pvisor_core::{
    RunResult,
    operation::{ExecutionSuspension, SnapshotRamStorage},
};
use serde_json::{Value, json};
use std::{
    fs::{self, File},
    io::Write,
    path::{Path, PathBuf},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

#[derive(Clone, Copy, Debug)]
enum Expected {
    Accepted,
    ReceiptRejected,
    DecodeRejected,
}

// Counts are subscenarios, not nextest test counts.
#[derive(Default, Debug)]
struct Counts {
    accepted: usize,
    receipt_rejected: usize,
    decode_rejected: usize,
    truncated_prefixes: usize,
    persisted_cuts: usize,
}

fn check(bytes: &[u8], expected: Expected, oracle: &Value, label: &str, counts: &mut Counts) {
    let decoded = serde_json::from_slice::<RunResult>(bytes);
    if matches!(expected, Expected::DecodeRejected) {
        assert!(decoded.is_err(), "{label}: malformed RunResult accepted");
        counts.decode_rejected += 1;
        return;
    }
    let result =
        decoded.unwrap_or_else(|error| panic!("{label}: unexpected decode error: {error}"));
    let before = serde_json::to_vec(&result).unwrap();
    let receipt = ExecutionSuspension::from_result(&result);
    assert_eq!(
        serde_json::to_vec(&result).unwrap(),
        before,
        "{label}: validation modified its input"
    );
    match expected {
        Expected::Accepted => {
            let receipt = receipt.unwrap_or_else(|error| panic!("{label}: {error:#}"));
            // The oracle was authored as JSON, not by serializing Core objects
            // or calling the same validation routine to compute expectations.
            assert_eq!(serde_json::to_value(&receipt).unwrap(), oracle["value"]);
            assert_eq!(result.run_id.as_str(), oracle["run_id"].as_str().unwrap());
            assert_eq!(
                result.execution_id.as_str(),
                oracle["execution_id"].as_str().unwrap()
            );
            assert_eq!(
                receipt.checkpoint.ram_storage,
                if oracle["value"]["checkpoint"]["ram_storage"] == "raw" {
                    SnapshotRamStorage::Raw
                } else {
                    SnapshotRamStorage::Compressed
                }
            );
            let restored = serde_json::to_vec(&result).unwrap();
            let again: RunResult = serde_json::from_slice(&restored).unwrap();
            assert_eq!(ExecutionSuspension::from_result(&again).unwrap(), receipt);
            counts.accepted += 1;
        }
        Expected::ReceiptRejected => {
            assert!(receipt.is_err(), "{label}: invalid suspension accepted");
            counts.receipt_rejected += 1;
        }
        Expected::DecodeRejected => unreachable!(),
    }
}

fn write_synced(path: &Path, bytes: &[u8]) {
    let mut file = File::create(path).unwrap();
    file.write_all(bytes).unwrap();
    file.sync_all().unwrap();
}

fn identity(seed: u64, bytes: usize) -> String {
    // Exact byte lengths, including multibyte identities at the 256-byte bound.
    let suffix = format!("-{seed:x}");
    let prefix = bytes.saturating_sub(suffix.len());
    if bytes < suffix.len() {
        return "r".repeat(bytes);
    }
    format!(
        "{}{}{suffix}",
        "界".repeat(prefix / 3),
        "x".repeat(prefix % 3)
    )
}

fn valid_wire(seed: u64, fixture: &Path) -> Value {
    let bytes = [1, 255, 256][seed as usize % 3];
    let run = identity(seed, bytes);
    let execution = format!("execution-{seed:x}-\"\\\n\u{0}界");
    let mut state = seed;
    let snapshot: String = (0..64)
        .map(|_| {
            // SplitMix64: no seed|1 alias, bounded deterministic content.
            state = state.wrapping_add(0x9e3779b97f4a7c15);
            let mut value = state;
            value = (value ^ (value >> 30)).wrapping_mul(0xbf58476d1ce4e5b9);
            value = (value ^ (value >> 27)).wrapping_mul(0x94d049bb133111eb);
            char::from(b"0123456789abcdef"[((value ^ (value >> 31)) & 15) as usize])
        })
        .collect();
    // This is a string-only path contract; never create the snapshot store.
    let store = fixture.join(format!("not-a-real-store-{seed:x}-界"));
    json!({
        "run_id": run, "execution_id": execution, "state": "hibernated",
        "started_at_unix_ms": seed, "finished_at_unix_ms": seed.saturating_add(1),
        "value": {
            "request_id": identity(seed.rotate_left(17), bytes),
            "checkpoint": {
                "snapshot_id": snapshot, "store": store,
                "source_run_id": run, "source_execution_id": execution,
                "created_at_unix_ms": 1 + seed % 1000000,
                "ram_storage": if seed.is_multiple_of(2) { "raw" } else { "compressed" }
            }
        }
    })
}

fn mutations(good: &Value) -> Vec<(String, Value, Expected)> {
    use Expected::{Accepted, DecodeRejected, ReceiptRejected};
    // Core uses the local host's absolute-path rules. Keep the root/4096-byte
    // positive controls native rather than assuming "/" is absolute on Windows.
    let store_root = Path::new(good["value"]["checkpoint"]["store"].as_str().unwrap())
        .ancestors()
        .last()
        .unwrap()
        .to_str()
        .unwrap();
    let mut cases = Vec::new();
    let mut change = |name: &str, pointer: &str, value: Value, expected| {
        let mut wire = good.clone();
        *wire.pointer_mut(pointer).unwrap() = value;
        cases.push((name.to_owned(), wire, expected));
    };
    for state in [
        "created",
        "starting",
        "running",
        "checkpointing",
        "suspended",
        "cancelling",
        "completed",
        "failed",
        "cancelled",
    ] {
        change(state, "/state", json!(state), ReceiptRejected);
    }
    change("unknown state", "/state", json!("invented"), DecodeRejected);
    for exit in [0, 7, -1, i32::MAX, i32::MIN] {
        let mut wire = good.clone();
        wire["exit_code"] = json!(exit);
        cases.push((format!("guest exit {exit}"), wire, ReceiptRejected));
    }
    let mut wire = good.clone();
    wire["failure"] = json!({"kind": "infrastructure", "message": "fake interruption"});
    cases.push(("native failure".into(), wire, ReceiptRejected));
    let mut change = |name: &str, pointer: &str, value: Value, expected| {
        let mut wire = good.clone();
        *wire.pointer_mut(pointer).unwrap() = value;
        cases.push((name.to_owned(), wire, expected));
    };
    change("null receipt", "/value", Value::Null, ReceiptRejected);
    change("wrong receipt shape", "/value", json!([]), ReceiptRejected);
    for request in [
        "".to_owned(),
        " \t\n".into(),
        "\u{2003}".into(),
        "界".repeat(86),
    ] {
        change(
            "bad request",
            "/value/request_id",
            json!(request),
            ReceiptRejected,
        );
    }
    for field in ["source_run_id", "source_execution_id"] {
        for bad in [
            "".to_owned(),
            " \t".into(),
            "x".repeat(257),
            "different".into(),
        ] {
            change(
                field,
                &format!("/value/checkpoint/{field}"),
                json!(bad),
                ReceiptRejected,
            );
        }
    }
    // Full mismatches rather than merely empty values: replay from another
    // execution must not become authority for the current Run.
    change(
        "outer Run mismatch",
        "/run_id",
        json!("another-run"),
        ReceiptRejected,
    );
    change(
        "outer Execution mismatch",
        "/execution_id",
        json!("another-execution"),
        ReceiptRejected,
    );
    for digest in [
        "a".repeat(63),
        "a".repeat(65),
        "A".repeat(64),
        "g".repeat(64),
    ] {
        change(
            "bad snapshot digest",
            "/value/checkpoint/snapshot_id",
            json!(digest),
            ReceiptRejected,
        );
    }
    for store in ["", "relative/store"] {
        change(
            "relative store",
            "/value/checkpoint/store",
            json!(store),
            ReceiptRejected,
        );
    }
    change(
        "oversized store",
        "/value/checkpoint/store",
        json!(format!("{store_root}{}", "x".repeat(4096))),
        ReceiptRejected,
    );
    change(
        "zero timestamp",
        "/value/checkpoint/created_at_unix_ms",
        json!(0),
        ReceiptRejected,
    );
    change(
        "bad RAM storage",
        "/value/checkpoint/ram_storage",
        json!("invented"),
        ReceiptRejected,
    );
    // Positive controls prevent stricter-than-contract expectations: Core does
    // not resolve stores or require a particular request identifier alphabet.
    for store in [
        store_root.to_owned(),
        format!("{store_root}{}", "x".repeat(4096 - store_root.len())),
    ] {
        change(
            "valid absolute store",
            "/value/checkpoint/store",
            json!(store),
            Accepted,
        );
    }
    for request in [
        "x".repeat(256),
        format!("{}x", "界".repeat(85)),
        "request-\0\n".into(),
    ] {
        change(
            "valid request bytes",
            "/value/request_id",
            json!(request),
            Accepted,
        );
    }
    change(
        "max timestamp",
        "/value/checkpoint/created_at_unix_ms",
        json!(u64::MAX),
        Accepted,
    );
    for pointer in ["/value", "/value/checkpoint"] {
        let mut wire = good.clone();
        wire.pointer_mut(pointer).unwrap()["unknown"] = json!(true);
        cases.push((format!("unknown field {pointer}"), wire, ReceiptRejected));
    }
    for field in ["request_id", "checkpoint"] {
        let mut wire = good.clone();
        wire["value"].as_object_mut().unwrap().remove(field);
        cases.push((format!("missing {field}"), wire, ReceiptRejected));
    }
    let mut wire = good.clone();
    wire.as_object_mut().unwrap().remove("value");
    cases.push(("missing receipt".into(), wire, ReceiptRejected));
    let mut wire = good.clone();
    wire["exit_code"] = Value::Null;
    wire["failure"] = Value::Null;
    cases.push(("explicit null exit/failure".into(), wire, Accepted));
    cases
}

fn seed_case(seed: u64, root: &Path, counts: &mut Counts) {
    let fixture = root.join(seed.to_string());
    fs::create_dir(&fixture).unwrap();
    let good = valid_wire(seed, &fixture);
    let original = serde_json::to_vec(&good).unwrap();
    let committed = fixture.join("committed.json");
    let pending = fixture.join("interrupted.json");
    write_synced(&committed, &original);
    check(
        &fs::read(&committed).unwrap(),
        Expected::Accepted,
        &good,
        "committed",
        counts,
    );

    // Cover every byte boundary, including cuts within escaped Unicode/NUL and
    // JSON delimiters. A proper prefix cannot be a complete outer object.
    for cut in 0..original.len() {
        assert!(
            serde_json::from_slice::<RunResult>(&original[..cut]).is_err(),
            "seed={seed} cut={cut}: truncated result decoded"
        );
        counts.truncated_prefixes += 1;
    }
    // Exercise selected cuts through actual close/reopen. No automatic repair
    // or atomic publication guarantee is attributed to Core's value types.
    let mut cuts = vec![0, 1, original.len() / 2, original.len() - 1];
    for token in *b"\\{:}" {
        let positions: Vec<_> = original
            .iter()
            .enumerate()
            .filter_map(|(index, byte)| (*byte == token).then_some(index))
            .collect();
        cuts.push(positions[seed as usize % positions.len()]);
    }
    cuts.sort_unstable();
    cuts.dedup();
    for cut in cuts {
        write_synced(&pending, &original[..cut]);
        let reopened = fs::read(&pending).unwrap();
        assert_eq!(reopened, original[..cut]);
        check(
            &reopened,
            Expected::DecodeRejected,
            &good,
            "persisted cut",
            counts,
        );
        assert_eq!(fs::read(&pending).unwrap(), reopened);
        assert_eq!(fs::read(&committed).unwrap(), original);
        counts.persisted_cuts += 1;
    }
    for (name, wire, expected) in mutations(&good) {
        let bytes = serde_json::to_vec(&wire).unwrap();
        write_synced(&pending, &bytes);
        let reopened = fs::read(&pending).unwrap();
        assert_eq!(reopened, bytes);
        check(
            &reopened,
            expected,
            &wire,
            &format!("seed={seed} {name}"),
            counts,
        );
        assert_eq!(fs::read(&pending).unwrap(), bytes, "{name}: input changed");
        assert_eq!(
            fs::read(&committed).unwrap(),
            original,
            "{name}: committed changed"
        );
    }
    // Explicit caller repair, not claimed automatic recovery. Repeated parsing
    // must retain the exact request/checkpoint and leave both files unchanged.
    write_synced(&pending, &original);
    for _ in 0..2 {
        check(
            &fs::read(&pending).unwrap(),
            Expected::Accepted,
            &good,
            "repaired",
            counts,
        );
        assert_eq!(fs::read(&pending).unwrap(), original);
    }
    assert_eq!(fs::read(&committed).unwrap(), original);
    fs::remove_dir_all(&fixture).unwrap(); // Only after all success assertions.
}

fn parameter(name: &str, default: u64) -> u64 {
    std::env::var(name).map_or(default, |value| value.parse().expect(name))
}

#[test]
fn suspension_receipts_reject_torn_and_misbound_persisted_evidence() {
    let first = parameter("PVISOR_CHAOS_FIRST_SEED", 202611120001);
    let count = parameter("PVISOR_CHAOS_SEED_COUNT", 8);
    assert!((1..=65536).contains(&count), "seed count must be 1..65536");
    first.checked_add(count).expect("seed interval overflow");
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root: PathBuf =
        std::env::temp_dir().join(format!("core-receipts-{}-{nonce}", std::process::id()));
    fs::create_dir(&root).unwrap();
    // Deliberately no Drop cleanup: a failed case retains the fixture and
    // exact input bytes for diagnosis. Success removes only this owned root.
    eprintln!(
        "Core receipt fixture={} first={first} count={count}",
        root.display()
    );
    let start = Instant::now();
    let mut counts = Counts::default();
    for seed in first..first + count {
        seed_case(seed, &root, &mut counts);
    }
    assert_eq!(fs::read_dir(&root).unwrap().count(), 0);
    fs::remove_dir(&root).unwrap();
    eprintln!(
        "Core receipt seeds={count} elapsed={:?} counts={counts:?}",
        start.elapsed()
    );
}
