use pvisor_core::memory::{
    MemoryObservation, NativeVmMemory, NodeMemorySample, ResidentMemory, RunMemorySample,
};
use serde_json::json;

fn node() -> NodeMemorySample {
    serde_json::from_value(json!({
        "sampled_at_unix_ms": 1,
        "supervisor": {"status": "measured", "usage": {
            "pid": 1, "start_time_ticks": 1,
            "process": ResidentMemory { mapped_bytes: 1, ..Default::default() }
        }},
        "system": {"status": "measured", "usage": {
            "host_boot_id": "00000000-0000-0000-0000-000000000001",
            "total_bytes": 1, "available_bytes": 1, "free_bytes": 1,
            "cached_bytes": 1, "buffers_bytes": 1, "slab_bytes": 1,
            "swap_total_bytes": 0, "swap_free_bytes": 0
        }},
        "cgroup": {"status": "measured", "usage": {
            "directory": "/sys/fs/cgroup/example", "device": 0, "inode": 1,
            "current_bytes": 2, "peak_bytes": 0, "max": {"bytes": 0},
            "high": "unlimited", "swap_current_bytes": null,
            "stat": {"anon": 1, "file": 1}, "events": {"high": 0}
        }}
    }))
    .unwrap()
}

#[test]
fn node_memory_roundtrips_all_partial_failure_combinations_without_zero_fallbacks() {
    for length in [0, 1, 341, 342, 1025] {
        for unavailable in 0..8 {
            let mut sample = node();
            let error = "界".repeat(length);
            if unavailable & 1 != 0 {
                sample.supervisor =
                    MemoryObservation::from_result(Err(anyhow::anyhow!(error.clone())));
            }
            if unavailable & 2 != 0 {
                sample.system = MemoryObservation::from_result(Err(anyhow::anyhow!(error.clone())));
            }
            if unavailable & 4 != 0 {
                sample.cgroup = MemoryObservation::from_result(Err(anyhow::anyhow!(error)));
            }
            sample.validate().unwrap();
            let bytes = serde_json::to_vec(&sample).unwrap();
            let restored: NodeMemorySample = serde_json::from_slice(&bytes).unwrap();
            restored.validate().unwrap();
            assert_eq!(sample, restored);
            let wire = serde_json::to_value(restored).unwrap();
            for (bit, scope) in ["supervisor", "system", "cgroup"].into_iter().enumerate() {
                if unavailable & (1 << bit) != 0 {
                    assert_eq!(wire[scope]["status"], "unavailable");
                    assert!(wire[scope].get("usage").is_none());
                    let error = wire[scope]["error"].as_str().unwrap();
                    assert!(!error.is_empty() && error.len() <= 1024);
                } else {
                    assert_eq!(wire[scope]["status"], "measured");
                    assert!(wire[scope].get("error").is_none());
                }
            }
        }
    }
}

#[test]
fn node_memory_rejects_ambiguous_missing_and_invalid_failure_facts() {
    for scope in ["supervisor", "system", "cgroup"] {
        for invalid in [
            json!({"status": "measured"}),
            json!({"status": "measured", "usage": null}),
            json!({"status": "unavailable"}),
            json!({"status": "unavailable", "error": null}),
            json!({"status": "unavailable", "error": "failed", "usage": null}),
            json!({"status": "unsupported", "error": "unsupported"}),
            json!({"error": "failed"}),
        ] {
            let mut wire = serde_json::to_value(node()).unwrap();
            wire[scope] = invalid;
            assert!(serde_json::from_value::<NodeMemorySample>(wire).is_err());
        }
        for error in ["".to_owned(), "界".repeat(342)] {
            let mut wire = serde_json::to_value(node()).unwrap();
            wire[scope] = json!({"status": "unavailable", "error": error});
            let sample = serde_json::from_value::<NodeMemorySample>(wire).unwrap();
            assert!(sample.validate().is_err());
        }
        let mut wire = serde_json::to_value(node()).unwrap();
        wire[scope]["error"] = "failed".into();
        assert!(serde_json::from_value::<NodeMemorySample>(wire).is_err());
        let mut wire = serde_json::to_value(node()).unwrap();
        wire.as_object_mut().unwrap().remove(scope);
        assert!(serde_json::from_value::<NodeMemorySample>(wire).is_err());
    }
}

#[test]
fn native_memory_partitions_and_failures_roundtrip_at_integer_boundaries() {
    let mut seed = 0xc2_2026_u64;
    for index in 0..32 {
        seed = seed.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1);
        let bytes = match index {
            0 => 1,
            1 => u64::MAX / 2,
            _ => 1 + seed / 2,
        };
        let partition = ResidentMemory {
            mapped_bytes: bytes,
            rss_bytes: bytes,
            pss_bytes: bytes,
            private_dirty_bytes: bytes,
            ..Default::default()
        };
        partition.validate().unwrap();
        let mut sample = RunMemorySample {
            run_id: "run".into(),
            execution_id: "execution".into(),
            sampled_at_unix_ms: 1,
            usage: Some(NativeVmMemory {
                pid: 1,
                start_time_ticks: 1,
                process: partition.checked_add(&partition).unwrap(),
                guest_ram: partition.clone(),
                non_ram: partition,
            }),
            error: None,
        };
        sample.validate().unwrap();
        let wire = serde_json::to_vec(&sample).unwrap();
        let restored: RunMemorySample = serde_json::from_slice(&wire).unwrap();
        restored.validate().unwrap();
        assert_eq!(sample, restored);
        let mut ambiguous = restored;
        ambiguous.error = Some("failed".into());
        assert!(ambiguous.validate().is_err());
        ambiguous.usage = None;
        ambiguous.error = None;
        let absent: RunMemorySample =
            serde_json::from_slice(&serde_json::to_vec(&ambiguous).unwrap()).unwrap();
        assert!(absent.validate().is_err());
        let usage = sample.usage.as_mut().unwrap();
        usage.non_ram.mapped_bytes += 1;
        assert!(sample.validate().is_err());
        sample.usage = None;
        sample.error = Some("CAPABILITY_UNSUPPORTED: attribution unavailable".into());
        sample.validate().unwrap();
        let restored: RunMemorySample =
            serde_json::from_slice(&serde_json::to_vec(&sample).unwrap()).unwrap();
        assert_eq!(restored, sample);
        restored.validate().unwrap();
        let mut wire = serde_json::to_value(restored).unwrap();
        wire["usage"] = json!({});
        assert!(serde_json::from_value::<RunMemorySample>(wire).is_err());
    }
    let maximum = ResidentMemory {
        mapped_bytes: u64::MAX,
        rss_bytes: u64::MAX,
        pss_bytes: u64::MAX,
        private_dirty_bytes: u64::MAX,
        ..Default::default()
    };
    maximum.validate().unwrap();
    assert!(maximum.checked_add(&maximum).is_none());
    let mut overflow = maximum.clone();
    overflow.shared_clean_bytes = 1;
    assert!(overflow.validate().is_err());
    let mut overflow = maximum;
    overflow.swap_bytes = 1;
    assert!(overflow.validate().is_err());
}
