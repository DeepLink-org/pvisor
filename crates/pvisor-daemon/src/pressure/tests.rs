use super::{
    api::*,
    policy::{Action, Policy},
};
use std::collections::BTreeMap;

fn id(n: u128) -> String {
    uuid::Uuid::from_u128(n).to_string()
}
fn config() -> Config {
    Config {
        memory_bytes: 20,
        high_bytes: 18,
        low_bytes: 14,
        max_writebacks: 16,
        resident_memory_accounting: ResidentMemoryAccounting::Reserved,
        waiting_memory_margin_bytes: None,
        cgroup_root: "/sys/fs/cgroup/test".into(),
    }
}
fn pair() -> Policy {
    let mut p = Policy::new(config()).unwrap();
    for n in 1..=2 {
        p.reserve(id(n), 8).unwrap();
    }
    assert!(p.plan(0, &BTreeMap::new()).unwrap().is_empty());
    for n in 1..=2 {
        p.bound(&id(n)).unwrap();
        p.wait(&id(n), None).unwrap();
    }
    p
}
fn charges(a: u64, b: u64) -> BTreeMap<String, u64> {
    [(id(1), a), (id(2), b)].into()
}

#[test]
fn abundant_memory_never_offloads_model_wait() {
    let mut p = pair();
    assert!(p.plan(12, &charges(6, 6)).unwrap().is_empty());
    assert_eq!(p.snapshot.writebacks, 0);
}

#[test]
fn blocked_admission_reclaims_then_resamples_and_keeps_restore_headroom() {
    let mut p = pair();
    p.reserve(id(3), 8).unwrap();
    let actions = p.plan(16, &charges(8, 8)).unwrap();
    assert_eq!(actions, [Action::Offload(id(1))]);
    p.finish(actions[0].clone(), Ok(()));
    // A successful control is NOT an 8-byte credit: current charge still high.
    let actions = p.plan(16, &charges(8, 8)).unwrap();
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    assert_eq!(actions, [Action::Offload(id(2))]);
    p.finish(actions[0].clone(), Ok(()));
    assert!(p.plan(2, &charges(1, 1)).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(3)].state, State::Admitted);
    // Two offloaded + new 8 + one full 8-byte restore fit in 20.
}

#[test]
fn in_flight_relief_prevents_repeated_waves() {
    let mut p = pair();
    p.reserve(id(3), 8).unwrap();
    assert_eq!(p.plan(16, &charges(8, 8)).unwrap().len(), 1);
    assert!(p.plan(16, &charges(8, 8)).unwrap().is_empty());
    assert_eq!(p.snapshot.writebacks, 1);
}

#[test]
fn low_charge_admission_selects_one_reservation_not_every_waiter() {
    let mut p = pair();
    p.reserve(id(3), 8).unwrap();
    // Parent charge is only 2, but resident commitments are 16. The new
    // reservation needs 4 more: one 8-byte commitment is enough to select.
    let actions = p.plan(2, &charges(1, 1)).unwrap();
    assert_eq!(actions, [Action::Offload(id(1))]);
    for _ in 0..3 {
        assert!(p.plan(2, &charges(1, 1)).unwrap().is_empty());
        assert_eq!(p.snapshot.writebacks, 1);
        assert_eq!(p.snapshot.entries[&id(2)].state, State::Waiting);
        assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    }
    p.finish(actions[0].clone(), Ok(()));
    // Completion introduces full restore headroom; estimates must not bypass it.
    assert_eq!(p.plan(2, &charges(1, 1)).unwrap(), [Action::Offload(id(2))]);
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
}

#[test]
fn logical_deficit_can_require_multiple_low_charge_victims() {
    let mut c = config();
    c.memory_bytes = 40;
    c.high_bytes = 36;
    c.low_bytes = 30;
    let mut p = Policy::new(c).unwrap();
    for n in 1..=7 {
        p.reserve(id(n), 5).unwrap();
    }
    p.plan(0, &BTreeMap::new()).unwrap();
    for n in 1..=7 {
        p.bound(&id(n)).unwrap();
        p.wait(&id(n), None).unwrap();
    }
    p.reserve(id(8), 16).unwrap();
    let charges = (1..=7).map(|n| (id(n), 1)).collect();
    // 35 + 16 - 40 = 11: select three 5-byte commitments, not all seven.
    assert_eq!(
        p.plan(7, &charges).unwrap(),
        [
            Action::Offload(id(1)),
            Action::Offload(id(2)),
            Action::Offload(id(3))
        ]
    );
    assert!(p.plan(7, &charges).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(8)].state, State::AdmissionQueued);
}

#[test]
fn in_flight_reservation_cannot_satisfy_physical_watermark() {
    let mut p = pair();
    p.config.memory_bytes = 100;
    p.config.max_writebacks = 1;
    // Physical deficit is 5. One VM has an 8-byte reservation but only 3 charged.
    assert_eq!(
        p.plan(19, &charges(3, 3)).unwrap(),
        [Action::Offload(id(1))]
    );
    assert!(p.plan(19, &charges(3, 3)).unwrap().is_empty());
    p.config.max_writebacks = 16;
    assert_eq!(
        p.plan(19, &charges(3, 3)).unwrap(),
        [Action::Offload(id(2))]
    );
    assert!(p.plan(19, &charges(3, 3)).unwrap().is_empty());
}

#[test]
fn physical_and_commitment_deficits_must_both_be_covered() {
    for (a, b, expected) in [
        (6, 1, vec![Action::Offload(id(1))]),
        (1, 6, vec![Action::Offload(id(1)), Action::Offload(id(2))]),
    ] {
        let mut p = pair();
        // Physical deficit 5, commitment deficit 8. An 8-byte reservation
        // covers only the latter unless its charge also covers the former.
        assert_eq!(p.plan(19, &charges(a, b)).unwrap(), expected);
        assert!(p.plan(19, &charges(a, b)).unwrap().is_empty());
    }
}

#[test]
fn pressure_hysteresis_uses_physical_charge() {
    let mut p = pair();
    let actions = p.plan(19, &charges(8, 8)).unwrap();
    assert_eq!(actions.len(), 1);
    p.finish(actions[0].clone(), Ok(()));
    assert!(p.plan(10, &charges(1, 8)).unwrap().is_empty());
}

#[test]
fn wake_during_writeback_waits_for_commit_and_resume_ack() {
    let mut p = pair();
    p.reserve(id(3), 8).unwrap();
    let action = p.plan(16, &charges(8, 8)).unwrap().remove(0);
    p.acquire(&id(1), id(100)).unwrap();
    assert!(p.plan(16, &charges(8, 8)).unwrap().is_empty());
    p.finish(action, Ok(()));
    let actions = p.plan(9, &charges(1, 8)).unwrap();
    assert_eq!(actions, [Action::Resume(id(1))]);
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Restoring);
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    p.finish(actions[0].clone(), Ok(()));
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Ready);
}

#[test]
fn fifo_resident_follower_can_be_reclaimed_but_not_granted_ahead_of_head() {
    let mut p = pair();
    p.reserve(id(3), 8).unwrap();
    let action = p.plan(16, &charges(8, 8)).unwrap().remove(0);
    p.finish(action, Ok(()));
    p.acquire(&id(1), id(100)).unwrap();
    p.acquire(&id(2), id(101)).unwrap();
    // External parent usage consumes the head's restoration budget.
    let actions = p.plan(14, &charges(1, 8)).unwrap();
    assert_eq!(actions, [Action::Offload(id(2))]);
    p.finish(actions[0].clone(), Ok(()));
    assert_eq!(p.plan(7, &charges(1, 1)).unwrap(), [Action::Resume(id(1))]);
    assert_eq!(p.snapshot.entries[&id(2)].state, State::RestoreQueued);
}

#[test]
fn uncertain_control_retains_slot_and_capacity_until_confirmed_release() {
    let mut p = pair();
    p.reserve(id(3), 8).unwrap();
    let action = p.plan(16, &charges(8, 8)).unwrap().remove(0);
    p.finish(action, Err(anyhow::anyhow!("lost acknowledgement")));
    assert_eq!(p.snapshot.writebacks, 1);
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Failed);
    assert!(p.plan(1, &charges(0, 1)).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    p.release(&id(1)).unwrap();
    assert_eq!(p.snapshot.writebacks, 0);
    p.plan(1, &[(id(2), 1)].into()).unwrap();
    assert_eq!(p.snapshot.entries[&id(3)].state, State::Admitted);
}

#[test]
fn active_and_ready_never_become_victims() {
    let mut p = pair();
    p.acquire(&id(1), id(100)).unwrap();
    p.plan(10, &charges(5, 5)).unwrap();
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Ready);
    assert_eq!(
        p.plan(19, &charges(8, 8)).unwrap(),
        [Action::Offload(id(2))]
    );
    assert!(p.wait(&id(1), Some(id(99))).is_err());
    p.wait(&id(1), Some(id(100))).unwrap();
}

#[test]
fn missing_accounting_never_grants_or_reclaims() {
    let mut p = pair();
    p.reserve(id(3), 8).unwrap();
    assert!(p.plan(0, &BTreeMap::new()).is_err());
    assert_eq!(p.snapshot.writebacks, 0);
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
}

#[test]
fn ceiling_sixteen_is_global_not_a_target() {
    let mut c = config();
    c.memory_bytes = 400;
    c.high_bytes = 390;
    c.low_bytes = 200;
    let mut p = Policy::new(c).unwrap();
    for n in 1..=40 {
        p.reserve(id(n), 8).unwrap();
    }
    p.plan(0, &BTreeMap::new()).unwrap();
    for n in 1..=40 {
        p.bound(&id(n)).unwrap();
        p.wait(&id(n), None).unwrap();
    }
    let charges = (1..=40).map(|n| (id(n), 8)).collect();
    assert_eq!(p.plan(399, &charges).unwrap().len(), 16);
    assert!(p.plan(399, &charges).unwrap().is_empty());
    assert_eq!(p.snapshot.writebacks, 16);
}

#[test]
fn disabled_offload_keeps_conservative_admission_same_request_path() {
    let mut p = pair();
    p.config.max_writebacks = 0;
    p.reserve(id(3), 8).unwrap();
    assert!(p.plan(19, &charges(8, 8)).unwrap().is_empty());
    p.acquire(&id(1), id(100)).unwrap();
    p.plan(19, &charges(8, 8)).unwrap();
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Ready);
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
}

#[test]
fn stale_requests_and_early_release_cannot_discard_ownership() {
    let mut p = pair();
    p.reserve(id(3), 8).unwrap();
    p.plan(16, &charges(8, 8)).unwrap();
    assert!(p.release(&id(1)).is_err());
    assert!(p.wait(&id(1), None).is_err());
    p.acquire(&id(1), id(100)).unwrap();
    p.acquire(&id(1), id(100)).unwrap();
    assert!(p.acquire(&id(1), id(101)).is_err());
    let mut c = config();
    c.max_writebacks = 17;
    assert!(Policy::new(c).is_err());
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn ordinary_files_cannot_impersonate_cgroup_accounting() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("cgroup.controllers"), "memory").unwrap();
    let mut config = config();
    config.cgroup_root = root.path().canonicalize().unwrap();
    let state = root.path().join("state");
    let error = super::serve(state.clone(), config).await.unwrap_err();
    assert!(error.to_string().contains("not a cgroup v2 filesystem"));
    assert!(!state.exists(), "invalid accounting must not create state");
}

fn elastic_pair() -> Policy {
    let mut p = pair();
    p.config.waiting_memory_margin_bytes = Some(1);
    p
}

#[test]
fn waiting_margin_is_explicit_validated_and_roundtrips() {
    let mut json = serde_json::to_value(config()).unwrap();
    json.as_object_mut()
        .unwrap()
        .remove("waiting_memory_margin_bytes");
    let legacy: Config = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(legacy.waiting_memory_margin_bytes, None);
    for margin in [0, 21] {
        json["waiting_memory_margin_bytes"] = margin.into();
        assert!(Policy::new(serde_json::from_value(json.clone()).unwrap()).is_err());
    }
    json["waiting_memory_margin_bytes"] = 1.into();
    let p = Policy::new(serde_json::from_value(json).unwrap()).unwrap();
    let restored: Config = serde_json::from_slice(&serde_json::to_vec(&p.config).unwrap()).unwrap();
    assert_eq!(restored.waiting_memory_margin_bytes, Some(1));
}

#[test]
fn low_charge_waiters_admit_more_without_offload_in_either_arm() {
    for writebacks in [0, 16] {
        let mut p = elastic_pair();
        p.config.max_writebacks = writebacks;
        p.reserve(id(3), 8).unwrap();
        assert!(p.plan(2, &charges(1, 1)).unwrap().is_empty());
        assert_eq!(p.snapshot.entries[&id(3)].state, State::Admitted);
        assert_eq!(p.snapshot.entries[&id(1)].state, State::Waiting);
        p.bound(&id(3)).unwrap();
        p.wait(&id(3), None).unwrap();
        p.reserve(id(4), 8).unwrap();
        let samples = [(id(1), 1), (id(2), 1), (id(3), 1)].into();
        assert!(p.plan(3, &samples).unwrap().is_empty());
        assert_eq!(p.snapshot.entries[&id(4)].state, State::Admitted);
        assert_eq!(p.snapshot.writebacks, 0);
        // 3*(charge 1 + margin 1) + startup 8 + wake extra 6 = 20.
    }
}

fn four_waiters() -> (Policy, BTreeMap<String, u64>) {
    let mut p = elastic_pair();
    p.config.max_writebacks = 0;
    let mut samples = charges(1, 1);
    for n in 3..=4 {
        p.reserve(id(n), 8).unwrap();
        p.plan(samples.len() as u64, &samples).unwrap();
        p.bound(&id(n)).unwrap();
        p.wait(&id(n), None).unwrap();
        samples.insert(id(n), 1);
    }
    (p, samples)
}

#[test]
fn concurrent_resident_wakes_reserve_full_capacity_and_keep_fifo() {
    let (mut p, samples) = four_waiters();
    for n in 1..=4 {
        p.acquire(&id(n), id(100 + n)).unwrap();
    }
    assert!(p.plan(4, &samples).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Ready);
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Ready);
    assert_eq!(p.snapshot.entries[&id(3)].state, State::Queued);
    assert_eq!(p.snapshot.entries[&id(4)].state, State::Queued);
    // Repeated polls cannot credit a Ready VM's low physical charge.
    for _ in 0..3 {
        p.plan(4, &samples).unwrap();
        assert_eq!(p.snapshot.entries[&id(3)].state, State::Queued);
    }
    assert!(p.wait(&id(1), Some(id(999))).is_err());
    p.wait(&id(1), Some(id(101))).unwrap();
    p.plan(4, &samples).unwrap();
    assert_eq!(p.snapshot.entries[&id(3)].state, State::Ready);
    assert_eq!(p.snapshot.entries[&id(4)].state, State::Queued);
    p.wait(&id(2), Some(id(102))).unwrap();
    p.plan(4, &samples).unwrap();
    assert_eq!(p.snapshot.entries[&id(4)].state, State::Ready);
}

#[test]
fn new_admission_preserves_one_resident_wake_and_requests_take_priority() {
    let (mut p, samples) = four_waiters();
    p.reserve(id(5), 8).unwrap();
    p.plan(4, &samples).unwrap();
    // Actual + waiting margins + new startup = 16 fits, but + wake 6 does not.
    assert_eq!(p.snapshot.entries[&id(5)].state, State::AdmissionQueued);
    p.acquire(&id(1), id(101)).unwrap();
    p.plan(4, &samples).unwrap();
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Ready);
    assert_eq!(p.snapshot.entries[&id(5)].state, State::AdmissionQueued);
}

#[test]
fn waiting_margin_is_capped_and_active_growth_stays_reserved() {
    let mut p = elastic_pair();
    p.config.waiting_memory_margin_bytes = Some(20);
    p.reserve(id(3), 8).unwrap();
    p.config.max_writebacks = 0;
    p.plan(2, &charges(1, 1)).unwrap();
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    p.config.waiting_memory_margin_bytes = Some(1);
    p.acquire(&id(1), id(101)).unwrap();
    p.plan(2, &charges(1, 1)).unwrap();
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Ready);
    // Writing shared pages may grow charge from 1 to 8 without new permission.
    // The full execution allowance was already held, not a UKSM savings credit.
    p.plan(9, &charges(8, 1)).unwrap();
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
}

#[test]
fn blocked_resident_head_reclaims_followers_not_itself_then_resamples() {
    let mut p = elastic_pair();
    p.acquire(&id(1), id(101)).unwrap();
    p.acquire(&id(2), id(102)).unwrap();
    // Parent includes 5 bytes of node overhead: 5 + 7 + 8 = 20, with 1 top-up.
    let actions = p.plan(20, &charges(7, 8)).unwrap();
    assert_eq!(actions, [Action::Offload(id(2))]);
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Queued);
    p.finish(actions[0].clone(), Ok(()));
    // Native completion alone is no memory credit.
    assert!(p.plan(20, &charges(7, 8)).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Queued);
    let actions = p.plan(13, &charges(7, 1)).unwrap();
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Ready);
    assert_eq!(p.snapshot.entries[&id(2)].state, State::RestoreQueued);
    assert!(actions.is_empty()); // 5 overhead + 8 executing + 8 restore > 20.
    p.wait(&id(1), Some(id(101))).unwrap();
    assert_eq!(p.plan(7, &charges(1, 1)).unwrap(), [Action::Resume(id(2))]);
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Restoring);
    p.finish(Action::Resume(id(2)), Ok(()));
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Ready);
}

#[test]
fn elastic_writeback_ceiling_pending_relief_and_uncertain_failure() {
    let mut p = elastic_pair();
    p.config.max_writebacks = 1;
    let actions = p.plan(19, &charges(8, 8)).unwrap();
    assert_eq!(actions, [Action::Offload(id(1))]);
    p.acquire(&id(1), id(101)).unwrap();
    assert!(p.plan(19, &charges(8, 8)).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Offloading);
    p.finish(actions[0].clone(), Err(anyhow::anyhow!("lost reply")));
    p.reserve(id(3), 8).unwrap();
    assert!(p.plan(2, &charges(1, 1)).unwrap().is_empty());
    assert_eq!(p.snapshot.writebacks, 1);
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Failed);
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    p.release(&id(1)).unwrap();
    assert_eq!(p.snapshot.writebacks, 0);
}

#[test]
fn restoring_keeps_full_reservation_until_ack_and_never_grants_followers_early() {
    let mut p = elastic_pair();
    let action = p.plan(19, &charges(8, 8)).unwrap().remove(0);
    p.finish(action, Ok(()));
    p.acquire(&id(1), id(101)).unwrap();
    p.acquire(&id(2), id(102)).unwrap();
    assert_eq!(p.plan(2, &charges(1, 1)).unwrap(), [Action::Resume(id(1))]);
    p.reserve(id(3), 8).unwrap();
    assert!(p.plan(2, &charges(1, 1)).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Queued);
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    p.finish(Action::Resume(id(1)), Ok(()));
    p.plan(2, &charges(1, 1)).unwrap();
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Ready);
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
}

#[test]
fn elastic_missing_sample_cannot_grant_resident_permit() {
    let mut p = elastic_pair();
    p.acquire(&id(1), id(101)).unwrap();
    assert!(p.plan(2, &[(id(2), 1)].into()).is_err());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Queued);
    assert_eq!(p.snapshot.writebacks, 0);
}

#[test]
fn sixteen_low_charge_waiters_fit_forty_gib_without_changing_guest_capacity() {
    let gib = 1u64 << 30;
    let mut c = config();
    c.memory_bytes = 40 * gib;
    c.high_bytes = 36 * gib;
    c.low_bytes = 30 * gib;
    c.waiting_memory_margin_bytes = Some(gib / 4);
    let mut p = Policy::new(c).unwrap();
    let mut samples = BTreeMap::new();
    for n in 1..=16 {
        p.reserve(id(n), 5 * gib + gib / 4).unwrap();
        let parent = samples.values().sum();
        assert!(p.plan(parent, &samples).unwrap().is_empty());
        assert_eq!(p.snapshot.entries[&id(n)].state, State::Admitted);
        p.bound(&id(n)).unwrap();
        p.wait(&id(n), None).unwrap();
        samples.insert(id(n), 320 * (1 << 20));
    }
    assert!(p.plan(5 * gib, &samples).unwrap().is_empty());
    assert!(
        p.snapshot
            .entries
            .values()
            .all(|e| e.state == State::Waiting)
    );
    assert_eq!(p.snapshot.writebacks, 0);
    // Synthetic accounting example: 5 GiB actual + 4 GiB wait margins,
    // not sixteen permanent reservations totalling 84 GiB. No UKSM credit.
}

#[test]
fn actual_memory_growth_without_safe_wait_never_releases_execution_allowance() {
    let mut p = Policy::new(Config {
        waiting_memory_margin_bytes: Some(1),
        ..config()
    })
    .unwrap();
    for n in 1..=3 {
        p.reserve(id(n), 8).unwrap();
    }
    p.plan(0, &BTreeMap::new()).unwrap();
    p.bound(&id(1)).unwrap();
    p.bound(&id(2)).unwrap();
    for sample in [charges(8, 8), charges(1, 1), charges(7, 7)] {
        assert!(p.plan(sample.values().sum(), &sample).unwrap().is_empty());
        assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    }
    // Only the owner's safe Wait transition makes unused capacity available.
    p.wait(&id(1), None).unwrap();
    p.wait(&id(2), None).unwrap();
    p.plan(2, &charges(1, 1)).unwrap();
    assert_eq!(p.snapshot.entries[&id(3)].state, State::Admitted);
}

#[test]
fn resident_grants_never_double_spend_capacity_across_samples() {
    for a in 0..=8 {
        for b in 0..=8 {
            for overhead in [0, 2, 8, 20] {
                let mut p = elastic_pair();
                p.config.max_writebacks = 0;
                p.acquire(&id(1), id(101)).unwrap();
                p.acquire(&id(2), id(102)).unwrap();
                let sample = charges(a, b);
                p.plan(a + b + overhead, &sample).unwrap();
                if p.snapshot.entries.values().any(|e| e.state == State::Ready) {
                    let mut total = overhead;
                    for n in 1..=2 {
                        total += match p.snapshot.entries[&id(n)].state {
                            State::Ready => 8,
                            State::Queued => (sample[&id(n)] + 1).min(8),
                            other => panic!("unexpected state {other:?}"),
                        };
                    }
                    assert!(total <= 20, "a={a} b={b} overhead={overhead}");
                }
                if p.snapshot.entries[&id(2)].state == State::Ready {
                    assert_eq!(p.snapshot.entries[&id(1)].state, State::Ready);
                }
            }
        }
    }
}

#[test]
fn admission_addition_overflow_cannot_turn_into_capacity() {
    let mut c = config();
    c.memory_bytes = u64::MAX;
    c.high_bytes = u64::MAX;
    c.low_bytes = u64::MAX - 1;
    c.waiting_memory_margin_bytes = Some(1);
    let mut p = Policy::new(c).unwrap();
    p.reserve(id(1), 8).unwrap();
    p.plan(u64::MAX - 4, &BTreeMap::new()).unwrap();
    assert_eq!(p.snapshot.entries[&id(1)].state, State::AdmissionQueued);
}

#[test]
fn zero_private_charge_can_relinquish_margin_but_never_physical_bytes() {
    let (mut p, mut samples) = four_waiters();
    p.config.max_writebacks = 16;
    for charge in samples.values_mut() {
        *charge = 0;
    }
    p.acquire(&id(1), id(101)).unwrap();
    // Pool/node charge is 12. Full head allowance 8 fits once the other three
    // waiting margins are relinquished. No private bytes are credited as freed.
    let actions = p.plan(12, &samples).unwrap();
    assert_eq!(
        actions,
        [
            Action::Offload(id(2)),
            Action::Offload(id(3)),
            Action::Offload(id(4))
        ]
    );
    assert!(p.plan(12, &samples).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Queued);
    for action in actions {
        p.finish(action, Ok(()));
    }
    assert!(p.plan(12, &samples).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Ready);

    let mut p = elastic_pair();
    // Only node/pool memory is high, and no admission/wake is waiting.
    // Empty private VM charges cannot satisfy this physical deficit.
    assert!(p.plan(18, &charges(0, 0)).unwrap().is_empty());
}

fn measured_config() -> Config {
    Config {
        resident_memory_accounting: ResidentMemoryAccounting::Measured,
        ..config()
    }
}

#[test]
fn measured_config_is_explicit_and_rejects_ambiguous_wait_margin() {
    let mut json = serde_json::to_value(config()).unwrap();
    json.as_object_mut()
        .unwrap()
        .remove("resident_memory_accounting");
    let legacy: Config = serde_json::from_value(json.clone()).unwrap();
    assert_eq!(
        legacy.resident_memory_accounting,
        ResidentMemoryAccounting::Reserved
    );
    json["resident_memory_accounting"] = "measured".into();
    let measured: Config = serde_json::from_value(json.clone()).unwrap();
    Policy::new(measured).unwrap();
    json["waiting_memory_margin_bytes"] = 1.into();
    assert!(Policy::new(serde_json::from_value(json.clone()).unwrap()).is_err());
    json["resident_memory_accounting"] = "typo".into();
    assert!(serde_json::from_value::<Config>(json).is_err());
}

#[test]
fn measured_binding_keeps_startup_capacity_until_successful_identity_probe() {
    let mut p = Policy::new(measured_config()).unwrap();
    for n in 1..=3 {
        p.reserve(id(n), 8).unwrap();
    }
    p.plan(0, &BTreeMap::new()).unwrap();
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    for n in 1..=2 {
        p.snapshot.entries.get_mut(&id(n)).unwrap().state = State::Binding;
    }
    p.plan(2, &charges(1, 1)).unwrap();
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    for n in 1..=2 {
        p.snapshot.entries.get_mut(&id(n)).unwrap().state = State::Active;
    }
    p.plan(2, &charges(1, 1)).unwrap();
    assert_eq!(p.snapshot.entries[&id(3)].state, State::Admitted);
    // No Wait transition was necessary to release the startup allowance.
}

#[test]
fn measured_sixteen_resident_vms_execute_in_forty_gib_without_offload() {
    let gib = 1u64 << 30;
    let mut p = Policy::new(Config {
        memory_bytes: 40 * gib,
        high_bytes: 36 * gib,
        low_bytes: 30 * gib,
        ..measured_config()
    })
    .unwrap();
    let mut samples = BTreeMap::new();
    for n in 1..=16 {
        p.reserve(id(n), 5 * gib + gib / 4).unwrap();
        assert!(p.plan(samples.values().sum(), &samples).unwrap().is_empty());
        assert_eq!(p.snapshot.entries[&id(n)].state, State::Admitted);
        p.bound(&id(n)).unwrap();
        samples.insert(id(n), 320 * (1 << 20));
    }
    for n in 1..=16 {
        p.wait(&id(n), None).unwrap();
        p.acquire(&id(n), id(100 + n)).unwrap();
    }
    assert!(p.plan(5 * gib, &samples).unwrap().is_empty());
    assert!(p.snapshot.entries.values().all(|e| e.state == State::Ready));
    assert_eq!(p.snapshot.writebacks, 0);
    for n in 1..=16 {
        assert_eq!(p.snapshot.entries[&id(n)].memory_bytes, 5 * gib + gib / 4);
    }
    // Full guest capacity is unchanged; this is measured overcommit, not a
    // claim that all sixteen guests can allocate five GiB simultaneously.
}

#[test]
fn measured_same_sample_startup_grants_cannot_double_spend() {
    let mut p = Policy::new(measured_config()).unwrap();
    for n in 1..=5 {
        p.reserve(id(n), 8).unwrap();
    }
    for _ in 0..3 {
        assert!(p.plan(0, &BTreeMap::new()).unwrap().is_empty());
        assert_eq!(
            p.snapshot
                .entries
                .values()
                .filter(|e| e.state == State::Admitted)
                .count(),
            2
        );
    }
}

#[test]
fn measured_growth_offloads_only_safe_waiters_and_honors_limit() {
    let mut c = measured_config();
    c.max_writebacks = 1;
    let mut p = Policy::new(c).unwrap();
    for n in 1..=3 {
        p.reserve(id(n), 8).unwrap();
        let samples = (1..n).map(|i| (id(i), 1)).collect();
        p.plan(n as u64 - 1, &samples).unwrap();
        p.bound(&id(n)).unwrap();
    }
    p.wait(&id(2), None).unwrap();
    p.wait(&id(3), None).unwrap();
    let samples = [(id(1), 8), (id(2), 6), (id(3), 6)].into();
    assert_eq!(p.plan(20, &samples).unwrap(), [Action::Offload(id(2))]);
    assert!(p.plan(20, &samples).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Active);
    assert_eq!(p.snapshot.entries[&id(3)].state, State::Waiting);
    assert_eq!(p.snapshot.writebacks, 1);
}

#[test]
fn measured_restore_keeps_capacity_and_fifo_until_ack_then_releases_top_up() {
    let mut p = Policy::new(measured_config()).unwrap();
    for n in 1..=2 {
        p.reserve(id(n), 8).unwrap();
    }
    p.plan(0, &BTreeMap::new()).unwrap();
    for n in 1..=2 {
        p.bound(&id(n)).unwrap();
        p.wait(&id(n), None).unwrap();
    }
    let actions = p.plan(18, &charges(9, 9)).unwrap();
    assert_eq!(actions, [Action::Offload(id(1))]);
    p.finish(actions[0].clone(), Ok(()));
    p.acquire(&id(1), id(101)).unwrap();
    p.acquire(&id(2), id(102)).unwrap();
    p.reserve(id(3), 8).unwrap();
    assert_eq!(p.plan(2, &charges(1, 1)).unwrap(), [Action::Resume(id(1))]);
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Restoring);
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Queued);
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    assert!(p.plan(2, &charges(1, 1)).unwrap().is_empty());
    p.finish(Action::Resume(id(1)), Ok(()));
    assert!(p.plan(2, &charges(1, 1)).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Ready);
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Ready);
    assert_eq!(p.snapshot.entries[&id(3)].state, State::Admitted);
}

#[test]
fn measured_admission_preserves_one_offloaded_wakeup_and_actual_cache_charge() {
    let mut p = Policy::new(measured_config()).unwrap();
    for n in 1..=2 {
        p.reserve(id(n), 8).unwrap();
    }
    p.plan(0, &BTreeMap::new()).unwrap();
    for n in 1..=2 {
        p.bound(&id(n)).unwrap();
        p.wait(&id(n), None).unwrap();
    }
    let action = p.plan(18, &charges(9, 9)).unwrap().remove(0);
    p.finish(action, Ok(()));
    p.reserve(id(3), 8).unwrap();
    p.config.max_writebacks = 0;
    p.plan(10, &charges(1, 1)).unwrap(); // +8 startup +7 recovery >20
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    p.plan(4, &charges(1, 1)).unwrap();
    assert_eq!(p.snapshot.entries[&id(3)].state, State::Admitted);
}

#[test]
fn measured_missing_sample_and_uncertain_restore_never_grant_followers() {
    let mut p = Policy::new(measured_config()).unwrap();
    for n in 1..=2 {
        p.reserve(id(n), 8).unwrap();
    }
    p.plan(0, &BTreeMap::new()).unwrap();
    for n in 1..=2 {
        p.bound(&id(n)).unwrap();
        p.wait(&id(n), None).unwrap();
    }
    let action = p.plan(18, &charges(9, 9)).unwrap().remove(0);
    p.finish(action, Ok(()));
    p.acquire(&id(1), id(101)).unwrap();
    p.acquire(&id(2), id(102)).unwrap();
    assert!(p.plan(2, &[(id(2), 1)].into()).is_err());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::RestoreQueued);
    let actions = p.plan(2, &charges(1, 1)).unwrap();
    p.finish(
        actions[0].clone(),
        Err(anyhow::anyhow!("lost resume reply")),
    );
    assert!(p.plan(2, &charges(1, 1)).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Failed);
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Queued);
}

#[test]
fn measured_resident_request_does_not_manufacture_reclaim_deficit() {
    let mut p = Policy::new(measured_config()).unwrap();
    for n in 1..=2 {
        p.reserve(id(n), 8).unwrap();
    }
    p.plan(0, &BTreeMap::new()).unwrap();
    for n in 1..=2 {
        p.bound(&id(n)).unwrap();
        p.wait(&id(n), None).unwrap();
    }
    p.acquire(&id(1), id(101)).unwrap();
    // Parent overhead is real and must not be subtracted as a sharing credit.
    // Reclaim against actual pressure, not a fictional full resident allowance.
    assert_eq!(
        p.plan(21, &charges(1, 1)).unwrap(),
        [Action::Offload(id(2))]
    );
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Queued);
}

#[test]
fn measured_high_watermark_stops_admission_until_low_even_without_offload() {
    let mut c = measured_config();
    c.max_writebacks = 0;
    let mut p = Policy::new(c).unwrap();
    p.reserve(id(1), 1).unwrap();
    // Startup would fit below the budget (20), but pressure must fence it.
    assert!(p.plan(18, &BTreeMap::new()).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::AdmissionQueued);
    assert!(p.plan(17, &BTreeMap::new()).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::AdmissionQueued);
    assert!(p.plan(14, &BTreeMap::new()).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Admitted);
    p.bound(&id(1)).unwrap();
    p.wait(&id(1), None).unwrap();
    p.acquire(&id(1), id(101)).unwrap();
    assert!(p.plan(14, &[(id(1), 1)].into()).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Ready);
}

#[test]
fn measured_blocked_startup_can_reclaim_below_high_but_needs_real_relief() {
    let mut p = Policy::new(measured_config()).unwrap();
    for n in 1..=2 {
        p.reserve(id(n), 8).unwrap();
    }
    p.plan(0, &BTreeMap::new()).unwrap();
    for n in 1..=2 {
        p.bound(&id(n)).unwrap();
        p.wait(&id(n), None).unwrap();
    }
    p.reserve(id(3), 8).unwrap();
    // Below high (18), but 15 actual + 8 startup does not fit.
    assert_eq!(
        p.plan(15, &charges(7, 7)).unwrap(),
        [Action::Offload(id(1))]
    );
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    p.finish(Action::Offload(id(1)), Ok(()));
    p.config.max_writebacks = 0;
    // An ACK alone is insufficient: parent cache is still charged.
    p.plan(15, &charges(0, 7)).unwrap();
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    // Retain 8 recovery + 8 startup, then use actual parent charge.
    p.plan(4, &charges(0, 3)).unwrap();
    assert_eq!(p.snapshot.entries[&id(3)].state, State::Admitted);
}

fn measured_startup_burst() -> Policy {
    let mut p = Policy::new(measured_config()).unwrap();
    p.reserve(id(1), 8).unwrap();
    p.plan(0, &BTreeMap::new()).unwrap();
    p.bound(&id(1)).unwrap();
    p.wait(&id(1), None).unwrap();
    for n in 2..=4 {
        p.reserve(id(n), 8).unwrap();
    }
    p
}

#[test]
fn measured_startup_burst_waits_for_bind_instead_of_offloading_peer() {
    let mut p = measured_startup_burst();
    // Two new startups fit, the third must wait. Do not unload the one bound
    // peer merely to fund that third startup on this very same plan.
    assert!(p.plan(2, &[(id(1), 1)].into()).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Waiting);
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Admitted);
    assert_eq!(p.snapshot.entries[&id(3)].state, State::Admitted);
    assert_eq!(p.snapshot.entries[&id(4)].state, State::AdmissionQueued);
    p.snapshot.entries.get_mut(&id(2)).unwrap().state = State::Binding;
    for _ in 0..3 {
        assert!(p.plan(4, &charges(1, 1)).unwrap().is_empty());
        assert_eq!(p.snapshot.entries[&id(4)].state, State::AdmissionQueued);
        assert_eq!(p.snapshot.writebacks, 0);
    }
    // Successful Bind releases only its own startup top-up; no invented credit.
    p.snapshot.entries.get_mut(&id(2)).unwrap().state = State::Active;
    p.bound(&id(3)).unwrap();
    assert!(
        p.plan(4, &[(id(1), 1), (id(2), 1), (id(3), 1)].into())
            .unwrap()
            .is_empty()
    );
    assert_eq!(p.snapshot.entries[&id(4)].state, State::Admitted);
}

#[test]
fn measured_startup_wait_does_not_mask_real_memory_deficit_or_high_watermark() {
    for parent in [15, 18] {
        let mut p = measured_startup_burst();
        p.plan(2, &[(id(1), 1)].into()).unwrap();
        // Even if all startup top-ups disappear, parent + next 8 does not fit.
        // High-watermark reclamation must also work while startup is pending.
        assert_eq!(
            p.plan(parent, &[(id(1), 5)].into()).unwrap(),
            [Action::Offload(id(1))]
        );
        assert_eq!(p.snapshot.entries[&id(4)].state, State::AdmissionQueued);
    }
}

#[test]
fn measured_failed_binding_never_releases_capacity_or_grants_followers() {
    let mut p = measured_startup_burst();
    p.plan(2, &[(id(1), 1)].into()).unwrap();
    p.snapshot.entries.get_mut(&id(2)).unwrap().state = State::Failed;
    assert!(p.plan(2, &charges(1, 1)).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(4)].state, State::AdmissionQueued);
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Waiting);
}

fn measured_offloading_head() -> Policy {
    let mut p = Policy::new(measured_config()).unwrap();
    for n in 1..=2 {
        p.reserve(id(n), 8).unwrap();
    }
    p.plan(0, &BTreeMap::new()).unwrap();
    for n in 1..=2 {
        p.bound(&id(n)).unwrap();
        p.wait(&id(n), None).unwrap();
    }
    assert_eq!(
        p.plan(18, &charges(9, 9)).unwrap(),
        [Action::Offload(id(1))]
    );
    p.acquire(&id(1), id(101)).unwrap();
    p.acquire(&id(2), id(102)).unwrap();
    p
}

#[test]
fn measured_resident_passes_offloading_head_without_resume_or_new_admission() {
    let mut p = measured_offloading_head();
    p.reserve(id(3), 8).unwrap();
    assert!(p.plan(2, &charges(1, 1)).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Offloading);
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Ready);
    assert_eq!(p.snapshot.entries[&id(2)].token, Some(id(102)));
    assert_eq!(p.snapshot.entries[&id(3)].state, State::AdmissionQueued);
    assert_eq!(p.snapshot.writebacks, 1);
    p.finish(Action::Offload(id(1)), Ok(()));
    assert_eq!(p.plan(2, &charges(1, 1)).unwrap(), [Action::Resume(id(1))]);
    p.finish(Action::Resume(id(1)), Ok(()));
    assert_eq!(p.snapshot.entries[&id(1)].state, State::Ready);
    assert!(p.wait(&id(2), Some(id(999))).is_err());
    p.wait(&id(2), Some(id(102))).unwrap();
}

#[test]
fn measured_resident_bypass_requires_recovery_headroom_and_complete_samples() {
    let mut p = measured_offloading_head();
    assert!(p.plan(2, &[(id(2), 1)].into()).is_err());
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Queued);
    // Resident itself needs no top-up, but 15 + 7 head recovery exceeds20.
    p.plan(15, &charges(1, 1)).unwrap();
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Queued);
    assert!(p.plan(13, &charges(1, 1)).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Ready);
}

#[test]
fn measured_resident_bypass_counts_outstanding_startup_reservations() {
    let mut p = Policy::new(measured_config()).unwrap();
    for n in 1..=2 {
        p.reserve(id(n), 8).unwrap();
    }
    p.plan(0, &BTreeMap::new()).unwrap();
    for n in 1..=2 {
        p.bound(&id(n)).unwrap();
        p.wait(&id(n), None).unwrap();
    }
    // Grant a startup before pressure rises; leave its Bind outstanding.
    p.reserve(id(3), 8).unwrap();
    assert!(p.plan(2, &charges(1, 1)).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(3)].state, State::Admitted);
    assert_eq!(
        p.plan(18, &charges(9, 9)).unwrap(),
        [Action::Offload(id(1))]
    );
    p.acquire(&id(1), id(101)).unwrap();
    p.acquire(&id(2), id(102)).unwrap();
    p.plan(6, &charges(1, 1)).unwrap(); // 6 + 8 startup + 7 recovery >20
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Queued);
}

#[test]
fn measured_offload_failure_still_stops_resident_bypass() {
    let mut p = measured_offloading_head();
    p.finish(
        Action::Offload(id(1)),
        Err(anyhow::anyhow!("uncertain offload")),
    );
    assert!(p.plan(2, &charges(1, 1)).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Queued);
    assert_eq!(p.snapshot.writebacks, 1);
}

#[test]
fn measured_bypass_preserves_native_restore_fifo_and_acknowledgements() {
    let mut p = Policy::new(measured_config()).unwrap();
    let mut samples = BTreeMap::new();
    for n in 1..=4 {
        p.reserve(id(n), 8).unwrap();
        p.plan(n as u64 - 1, &samples).unwrap();
        p.bound(&id(n)).unwrap();
        p.wait(&id(n), None).unwrap();
        samples.insert(id(n), 1);
    }
    let high = (1..=4).map(|n| (id(n), 5)).collect();
    assert_eq!(
        p.plan(20, &high).unwrap(),
        [Action::Offload(id(1)), Action::Offload(id(2))]
    );
    p.finish(Action::Offload(id(2)), Ok(()));
    for n in 1..=4 {
        p.acquire(&id(n), id(100 + n)).unwrap();
    }
    assert!(p.plan(4, &samples).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(2)].state, State::RestoreQueued);
    for n in 3..=4 {
        assert_eq!(p.snapshot.entries[&id(n)].state, State::Ready);
    }
    p.finish(Action::Offload(id(1)), Ok(()));
    assert_eq!(p.plan(4, &samples).unwrap(), [Action::Resume(id(1))]);
    assert!(p.plan(4, &samples).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(2)].state, State::RestoreQueued);
    p.finish(Action::Resume(id(1)), Ok(()));
    assert_eq!(p.plan(4, &samples).unwrap(), [Action::Resume(id(2))]);
    p.finish(Action::Resume(id(2)), Ok(()));
    assert!(p.snapshot.entries.values().all(|e| e.state == State::Ready));
}

#[test]
fn reserved_mode_keeps_strict_fifo_behind_offloading_head() {
    let mut p = pair();
    assert_eq!(
        p.plan(18, &charges(9, 9)).unwrap(),
        [Action::Offload(id(1))]
    );
    p.acquire(&id(1), id(101)).unwrap();
    p.acquire(&id(2), id(102)).unwrap();
    assert!(p.plan(2, &charges(1, 1)).unwrap().is_empty());
    assert_eq!(p.snapshot.entries[&id(2)].state, State::Queued);
}
