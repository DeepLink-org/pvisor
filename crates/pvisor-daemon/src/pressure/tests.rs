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
