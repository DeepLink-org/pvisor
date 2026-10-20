//! Single-vCPU opt-in guest idle handoff. All time and wake state lives on host.
//! PIO probe anchors a host timestamp BEFORE the guest reads its timer delta.
//! The arm timeout bounds acquisition only. ABI v2 reports a real timer or no timer.
#![cfg_attr(
    not(all(target_os = "linux", target_arch = "x86_64")),
    allow(dead_code)
)]
use crate::api::{GuestIdleControl, GuestIdleStatus, VmmHandle};
use std::sync::{
    Condvar, Mutex,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, Instant};

pub(crate) const PORT: u16 = 0x5050;
pub(crate) const MAGIC: u32 = 0x50564902;
const MARGIN: Duration = Duration::from_millis(2);
fn diagnostics_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("PVISOR_DIAG_OFFLOAD").as_deref() == Ok("1"))
}
#[derive(Default)]
struct State {
    epoch: u64,
    offers_seen: u64,
    offer_sequence: Option<u64>,
    sequence: u64,
    until: Option<Instant>,
    probe: Option<Instant>,
    offered: bool,
    claimed: bool,
    offload_started: bool,
    offload_minimum: Duration,
    offload_deadline_skips: u64,
    reason: Option<String>,
    host_reads: u64,
}
#[derive(Default)]
pub(crate) struct IdleGate {
    enabled: AtomicBool,
    state: Mutex<State>,
    changed: Condvar,
}
impl IdleGate {
    pub(crate) fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Acquire)
    }
    /// Optional Host queue scans must not manufacture an external wake.
    /// Real queue kicks/completions still go through the normal RAM gate.
    pub(crate) fn defer_maintenance(&self) -> bool {
        let s = self.state.lock().unwrap();
        s.probe.is_some() || s.offered || s.claimed
    }
    pub(crate) fn host_read(&self, begin: bool) {
        let mut s = self.state.lock().unwrap();
        if begin {
            s.host_reads += 1;
        } else {
            s.host_reads = s.host_reads.checked_sub(1).expect("unbalanced host read");
            Self::invalidate(&mut s, "host_read_ready");
        }
        s.sequence += 1;
        self.changed.notify_all();
    }
    pub(crate) fn set_offload_minimum(&self, minimum: Duration) {
        self.state.lock().unwrap().offload_minimum = minimum;
    }
    pub(crate) fn enable(&self, value: bool) {
        self.enabled.store(value, Ordering::Release);
    }
    fn invalidate(s: &mut State, reason: &str) {
        if s.until.take().is_some() || s.offered {
            s.sequence += 1;
            s.reason = Some(reason.into());
        }
        s.probe = None;
        s.offered = false;
    }
    fn expire(s: &mut State) {
        if s.until.is_some_and(|d| Instant::now() >= d) {
            let reason = if s.offered {
                "guest_timer"
            } else {
                "handoff_timeout"
            };
            Self::invalidate(s, reason);
            Self::diagnose(s, "expired", Some(reason));
        }
    }
    pub(crate) fn arm(&self, maximum: Duration) -> Result<u64, String> {
        if !self.enabled.load(Ordering::Acquire) {
            return Err("guest idle handoff not configured".into());
        }
        if maximum <= MARGIN || maximum > Duration::from_secs(1) {
            return Err("idle lease must be >2 ms and <=1 s".into());
        }
        let mut s = self.state.lock().unwrap();
        Self::expire(&mut s);
        if s.until.is_some() || s.offered || s.claimed {
            return Err("previous idle lease still owned".into());
        }
        s.epoch = s.epoch.checked_add(1).ok_or("idle epoch exhausted")?;
        s.sequence += 1;
        s.until = Some(Instant::now() + maximum);
        s.probe = None;
        s.offered = false;
        s.offload_started = false;
        s.offer_sequence = None;
        s.reason = None;
        Self::diagnose(&s, "arm", None);
        self.changed.notify_all();
        Ok(s.epoch)
    }
    pub(crate) fn resume_arm(&self, epoch: u64, maximum: Duration) -> Result<u64, String> {
        if !self.enabled() || maximum <= MARGIN || maximum > Duration::from_secs(1) {
            return Err("invalid resume acquisition budget or disabled handoff".into());
        }
        let mut s = self.state.lock().unwrap();
        if s.epoch != epoch || !s.claimed || s.offered || s.until.is_some() {
            return Err("resume-rearm requires the retired claimed epoch".into());
        }
        let next = s.epoch.checked_add(1).ok_or("idle epoch exhausted")?;
        s.epoch = next;
        s.sequence += 1;
        s.until = Some(Instant::now() + maximum);
        s.probe = None;
        s.offered = false;
        s.claimed = false;
        s.offload_started = false;
        s.offer_sequence = None;
        s.reason = None;
        Self::diagnose(&s, "resume_arm", None);
        self.changed.notify_all();
        Ok(next)
    }
    fn diagnose(s: &State, event: &str, detail: Option<&str>) {
        static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
        if !*ENABLED.get_or_init(|| std::env::var("PVISOR_DIAG_IDLE").as_deref() == Ok("1")) {
            return;
        }
        use std::io::Write;
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        let record = serde_json::json!({
            "event":event,"detail":detail,"epoch":s.epoch,"sequence":s.sequence,
            "offer_sequence":s.offer_sequence,"offered":s.offered,"claimed":s.claimed,
            "remaining_ns":s.until.map(|d| d.saturating_duration_since(Instant::now()).as_nanos()),
            "wake_reason":s.reason,"host_pid":std::process::id(),
            "mono_ns":START.get_or_init(Instant::now).elapsed().as_nanos(),
            "unix_ns":std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
        });
        let _ = writeln!(std::io::stderr().lock(), "pvisor-idle-diagnostic {record}");
    }
    pub(crate) fn diagnose_transition(&self, event: &str) {
        Self::diagnose(&self.state.lock().unwrap(), event, None);
    }
    pub(crate) fn probe(&self) -> u32 {
        if !self.enabled.load(Ordering::Acquire) {
            return 0;
        }
        let mut s = self.state.lock().unwrap();
        Self::expire(&mut s);
        if s.until.is_some() && !s.claimed && !s.offered {
            s.probe = Some(Instant::now());
            Self::diagnose(&s, "probe", Some("armed"));
            MAGIC
        } else {
            Self::diagnose(&s, "probe", Some("not_armed"));
            0
        }
    }
    /// ABI v2: milliseconds rounded down; MAX explicitly means no clockevent.
    /// Large finite deadlines are conservatively capped at MAX-1 milliseconds.
    pub(crate) fn offer(&self, millis: u32, admissible: bool) {
        let mut s = self.state.lock().unwrap();
        // Count every PIO OUT, including interrupt-raced/rejected offers. This
        // matches the diagnostic guest ordinal without changing the PIO ABI.
        s.offers_seen = s
            .offers_seen
            .checked_add(1)
            .expect("idle offer ordinal overflow");
        if !admissible {
            Self::diagnose(
                &s,
                "offer_rejected",
                Some("pending_interrupt_or_incomplete_pio"),
            );
            s.probe = None;
            return;
        }
        Self::expire(&mut s);
        if let (Some(anchor), Some(_acquisition_limit)) = (s.probe.take(), s.until) {
            if millis <= 2 {
                Self::diagnose(&s, "offer_rejected", Some("short_deadline"));
                return;
            }
            let until = if millis == u32::MAX {
                None
            } else {
                let Some(deadline) = anchor.checked_add(Duration::from_millis(millis.into()))
                else {
                    return;
                };
                let wake_at = deadline - MARGIN;
                if wake_at <= Instant::now() {
                    Self::diagnose(&s, "offer_rejected", Some("elapsed_deadline"));
                    return;
                }
                Some(wake_at)
            };
            s.until = until;
            s.offered = true;
            s.offer_sequence = Some(s.offers_seen);
            s.sequence += 1;
            Self::diagnose(&s, "offer_accepted", None);
            self.changed.notify_all();
        }
    }
    pub(crate) fn wake(&self, reason: &str) {
        if !self.enabled.load(Ordering::Acquire) {
            return;
        }
        let mut s = self.state.lock().unwrap();
        // Before publication, discard only the in-flight probe. The guest can
        // retry its handshake within the original host deadline. No controller
        // has received an offer yet. Once published/claimed, latch the wake and
        // retire the entire lease; never reuse a permission visible to the host.
        if s.offered || s.claimed {
            Self::invalidate(&mut s, reason);
            Self::diagnose(&s, "wake", Some(reason));
            self.changed.notify_all();
        } else {
            s.probe = None;
        }
    }
    pub(crate) fn offered(&self) -> bool {
        if !self.enabled.load(Ordering::Acquire) {
            return false;
        }
        let mut s = self.state.lock().unwrap();
        Self::expire(&mut s);
        s.offered || s.claimed
    }
    pub(crate) fn wait_tick(&self) {
        let s = self.state.lock().unwrap();
        let _ = self
            .changed
            .wait_timeout(s, Duration::from_millis(1))
            .unwrap();
    }
    pub(crate) fn claim(&self, epoch: u64) -> bool {
        let mut s = self.state.lock().unwrap();
        Self::expire(&mut s);
        if s.epoch != epoch || !s.offered || s.claimed {
            return false;
        }
        s.claimed = true;
        s.sequence += 1;
        Self::diagnose(&s, "claimed", None);
        self.changed.notify_all();
        true
    }
    /// Optional engineering observation; emits only after releasing the idle lock.
    pub(crate) fn trace_offload(&self, epoch: u64, reason: &str, elapsed_ns: Option<u128>) {
        if !diagnostics_enabled() {
            return;
        }
        let s = self.state.lock().unwrap();
        let record = serde_json::json!({
            "source": "vmm", "host_pid": std::process::id(), "epoch": epoch,
            "current_epoch": s.epoch, "offer_sequence": s.offer_sequence,
            "reason": reason, "remaining_ns": s.until.map(|d| d.saturating_duration_since(Instant::now()).as_nanos()),
            "minimum_ns": s.offload_minimum.max(MARGIN).as_nanos(),
            "offered": s.offered, "claimed": s.claimed, "elapsed_ns": elapsed_ns,
            "unix_ns": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
        });
        drop(s);
        use std::io::Write;
        let _ = writeln!(
            std::io::stderr().lock(),
            "pvisor-offload-diagnostic {record}"
        );
    }
    /// Admission is linearized against wake/retire under this short lock. Never
    /// retain the lock during backing I/O: notifications must remain serviceable.
    pub(crate) fn begin_offload(&self, epoch: u64) -> bool {
        let mut s = self.state.lock().unwrap();
        Self::expire(&mut s);
        let remaining = s.until.map(|d| d.saturating_duration_since(Instant::now()));
        let reason = if s.epoch != epoch || !s.claimed || !s.offered || s.offload_started {
            "invalid_or_consumed_epoch"
        } else if remaining.is_some_and(|d| d <= s.offload_minimum.max(MARGIN)) {
            s.offload_deadline_skips = s.offload_deadline_skips.saturating_add(1);
            s.sequence += 1;
            self.changed.notify_all();
            "deadline_too_short"
        } else {
            s.offload_started = true;
            "admitted"
        };
        let record = diagnostics_enabled().then(|| serde_json::json!({
            "source": "vmm", "host_pid": std::process::id(), "epoch": epoch,
            "current_epoch": s.epoch, "offer_sequence": s.offer_sequence,
            "reason": reason, "remaining_ns": remaining.map(|d| d.as_nanos()),
            "minimum_ns": s.offload_minimum.max(MARGIN).as_nanos(),
            "offered": s.offered, "claimed": s.claimed,
            "unix_ns": std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos()
        }));
        drop(s);
        if let Some(record) = record {
            use std::io::Write;
            let _ = writeln!(
                std::io::stderr().lock(),
                "pvisor-offload-diagnostic {record}"
            );
        }
        reason == "admitted"
    }
    pub(crate) fn retire(&self, epoch: u64) {
        let mut s = self.state.lock().unwrap();
        if s.epoch == epoch {
            Self::invalidate(&mut s, "cancel");
            Self::diagnose(&s, "retired", None);
            self.changed.notify_all();
        }
    }
    pub(crate) fn resumed(&self) {
        let mut s = self.state.lock().unwrap();
        Self::invalidate(&mut s, "resume");
        s.claimed = false;
        s.sequence += 1;
        self.changed.notify_all();
    }
    pub(crate) fn status(&self) -> GuestIdleStatus {
        let mut s = self.state.lock().unwrap();
        Self::expire(&mut s);
        GuestIdleStatus {
            epoch: s.epoch,
            offer_sequence: s.offer_sequence,
            sequence: s.sequence,
            enabled: self.enabled.load(Ordering::Acquire),
            offered: s.offered,
            claimed: s.claimed,
            remaining_ns: s.until.map_or(if s.offered { u64::MAX } else { 0 }, |d| {
                d.saturating_duration_since(Instant::now()).as_nanos() as u64
            }),
            wake_reason: s.reason.clone(),
            pending_host_reads: s.host_reads,
            offload_minimum_ns: s.offload_minimum.as_nanos() as u64,
            offload_deadline_skips: s.offload_deadline_skips,
        }
    }
}
impl GuestIdleControl for VmmHandle {
    fn arm_guest_idle(&self, maximum: Duration) -> Result<u64, String> {
        self.vmm.upgrade().ok_or("VMM stopped")?;
        self.idle.arm(maximum)
    }
    fn resume_and_arm_guest_idle(&self, epoch: u64, maximum: Duration) -> Result<u64, String> {
        let _transition = self
            .transition
            .lock()
            .map_err(|_| "VM transition lock poisoned")?;
        let vm = self.vmm.upgrade().ok_or("VMM stopped")?;
        let mut vm = vm.lock().map_err(|_| "VMM lock poisoned")?;
        if !vm.is_paused() {
            return Err("resume-rearm requires paused VM".into());
        }
        let next = self.idle.resume_arm(epoch, maximum)?;
        // Do not call ordinary handle.resume(): it would erase the fresh arm.
        self.idle.diagnose_transition("resume_release_begin");
        if let Err(error) = vm.resume() {
            self.idle.retire(next);
            vm.fail_control();
            return Err(error.to_string());
        }
        self.idle.diagnose_transition("resume_completed");
        Ok(next)
    }
    fn guest_idle_status(&self) -> GuestIdleStatus {
        self.idle.status()
    }
    fn wait_guest_idle(&self, sequence: u64, timeout: Duration) -> GuestIdleStatus {
        let mut s = self.idle.state.lock().unwrap();
        IdleGate::expire(&mut s);
        if s.sequence == sequence {
            let wait = timeout.min(Duration::from_millis(100)).min(
                s.until
                    .map_or(timeout, |d| d.saturating_duration_since(Instant::now())),
            );
            s = self.idle.changed.wait_timeout(s, wait).unwrap().0;
        }
        drop(s);
        self.idle.status()
    }
    fn pause_guest_idle(&self, epoch: u64) -> Result<bool, String> {
        let _transition = self
            .transition
            .lock()
            .map_err(|_| "VM transition lock poisoned")?;
        let vm = self.vmm.upgrade().ok_or("VMM stopped")?;
        let mut vm = vm.lock().map_err(|_| "VMM lock poisoned")?;
        if vm.is_paused() || !self.idle.claim(epoch) {
            return Ok(false);
        }
        vm.pause().map_err(|e| e.to_string())?;
        Ok(true)
    }
    fn offload_guest_idle(&self, epoch: u64) -> Result<Option<crate::api::RamReclaim>, String> {
        self.offload_ram_with_idle(Some(epoch))
    }
    fn retire_guest_idle(&self, epoch: u64) {
        self.idle.retire(epoch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resume_arm_requires_retired_claim_and_never_reuses_permission() {
        let (gate, epoch) = armed();
        assert!(gate.resume_arm(epoch, Duration::from_secs(1)).is_err());
        gate.probe();
        gate.offer(5000, true);
        assert!(gate.claim(epoch));
        assert!(gate.resume_arm(epoch, Duration::from_secs(1)).is_err());
        gate.wake("guest_timer");
        gate.retire(epoch);
        assert!(gate.resume_arm(epoch + 1, Duration::from_secs(1)).is_err());
        assert!(gate.resume_arm(epoch, Duration::from_secs(2)).is_err());
        assert!(gate.status().claimed);
        let next = gate.resume_arm(epoch, Duration::from_secs(1)).unwrap();
        assert_eq!(next, epoch + 1);
        assert!(!gate.claim(next), "arm is not permission");
        gate.retire(epoch);
        assert_eq!(gate.probe(), MAGIC);
        gate.offer(5000, false);
        assert!(!gate.claim(next));
        gate.probe();
        gate.offer(5000, true);
        assert!(gate.claim(next));
        gate.wake("network_rx");
        assert!(!gate.begin_offload(next));
        assert!(gate.status().claimed);
    }
    #[test]
    fn resume_arm_acquisition_timeout_is_not_guest_timer() {
        let (gate, epoch) = armed();
        gate.probe();
        gate.offer(5000, true);
        assert!(gate.claim(epoch));
        gate.retire(epoch);
        let next = gate.resume_arm(epoch, Duration::from_secs(1)).unwrap();
        gate.state.lock().unwrap().until = Some(Instant::now());
        assert!(!gate.claim(next));
        assert_eq!(
            gate.status().wake_reason.as_deref(),
            Some("handoff_timeout")
        );
    }
    #[test]
    fn offer_ordinals_include_interrupt_rejection_and_survive_wake() {
        let (gate, epoch) = armed();
        gate.probe();
        gate.offer(5000, false); // A pending interrupt rejected this PIO.
        assert_eq!(gate.status().offer_sequence, None);
        assert!(!gate.status().offered);
        gate.probe();
        gate.offer(5000, true);
        assert_eq!(gate.status().offer_sequence, Some(2));
        gate.wake("network_rx");
        assert_eq!(gate.status().offer_sequence, Some(2));
        gate.retire(epoch);
        gate.arm(Duration::from_secs(1)).unwrap();
        assert_eq!(gate.status().offer_sequence, None);
        gate.probe();
        gate.offer(5000, true);
        assert_eq!(gate.status().offer_sequence, Some(3));
    }
    fn armed() -> (IdleGate, u64) {
        let gate = IdleGate::default();
        gate.enable(true);
        let epoch = gate.arm(Duration::from_secs(1)).unwrap();
        (gate, epoch)
    }
    #[test]
    fn short_deadline_skips_reclaim_without_revoking_pause() {
        let (gate, epoch) = armed();
        gate.set_offload_minimum(Duration::from_secs(1));
        gate.probe();
        gate.offer(250, true);
        assert!(gate.claim(epoch));
        assert!(!gate.begin_offload(epoch));
        let status = gate.status();
        assert!(status.claimed && status.offered);
        assert!(status.wake_reason.is_none());
        assert_eq!(status.offload_deadline_skips, 1);
        assert_eq!(status.offload_minimum_ns, 1_000_000_000);
        gate.wake("network_rx");
        assert_eq!(gate.status().wake_reason.as_deref(), Some("network_rx"));
        assert!(!gate.begin_offload(epoch));
    }
    #[test]
    fn offload_checks_current_deadline_after_pause_and_allows_no_timer() {
        for millis in [5000, u32::MAX] {
            let (gate, epoch) = armed();
            gate.set_offload_minimum(Duration::from_secs(1));
            gate.probe();
            gate.offer(millis, true);
            assert!(gate.claim(epoch));
            assert!(gate.begin_offload(epoch));
            assert!(!gate.begin_offload(epoch));
        }
        let (gate, epoch) = armed();
        gate.set_offload_minimum(Duration::from_secs(1));
        gate.probe();
        gate.offer(5000, true);
        assert!(gate.claim(epoch));
        // Simulate time spent awaiting native transition/device gate locks.
        gate.state.lock().unwrap().until = Some(Instant::now() + Duration::from_millis(250));
        assert!(!gate.begin_offload(epoch));
        assert!(gate.status().offered);
    }
    #[test]
    fn opt_in_and_timer_bounds_are_required() {
        let gate = IdleGate::default();
        assert!(gate.arm(Duration::from_secs(1)).is_err());
        assert_eq!(gate.probe(), 0);
        gate.enable(true);
        assert!(gate.arm(MARGIN).is_err());
        assert!(gate.arm(Duration::from_secs(2)).is_err());
        gate.arm(Duration::from_secs(1)).unwrap();
        assert_eq!(gate.probe(), MAGIC);
        gate.offer(2, true);
        assert!(!gate.offered());
    }
    #[test]
    fn wake_between_probe_and_offer_revokes_permission() {
        let (gate, epoch) = armed();
        assert_eq!(gate.probe(), MAGIC);
        gate.wake("network_rx");
        gate.offer(500, true);
        assert!(!gate.claim(epoch));
        assert!(gate.status().wake_reason.is_none());
        assert_eq!(gate.probe(), MAGIC);
        gate.offer(500, true);
        assert!(
            gate.claim(epoch),
            "a fresh probe may retry before publication"
        );
    }
    #[test]
    fn claimed_wake_stays_parked_until_serialized_resume() {
        let (gate, epoch) = armed();
        gate.probe();
        gate.offer(500, true);
        assert!(!gate.claim(epoch + 1));
        assert!(gate.claim(epoch));
        gate.wake("device_ram");
        gate.retire(epoch);
        assert!(
            gate.offered(),
            "claimed guest must not run before pause ACK"
        );
        assert!(gate.arm(Duration::from_secs(1)).is_err());
        gate.resumed();
        assert!(!gate.offered());
        let newer = gate.arm(Duration::from_secs(1)).unwrap();
        gate.retire(epoch);
        assert_eq!(gate.probe(), MAGIC);
        gate.offer(500, true);
        assert!(gate.claim(newer));
    }
    #[test]
    fn expired_probe_and_offer_cannot_pause() {
        let (gate, epoch) = armed();
        gate.probe();
        gate.state.lock().unwrap().probe = Some(Instant::now() - Duration::from_secs(1));
        gate.offer(100, true);
        assert!(!gate.claim(epoch));
        gate.probe();
        gate.offer(100, true);
        gate.state.lock().unwrap().until = Some(Instant::now());
        assert!(!gate.claim(epoch));
        assert_eq!(gate.status().wake_reason.as_deref(), Some("guest_timer"));
    }
    #[test]
    fn offload_requires_claimed_current_live_lease_and_is_admitted_once() {
        let (gate, epoch) = armed();
        gate.probe();
        gate.offer(500, true);
        assert!(!gate.begin_offload(epoch));
        assert!(gate.claim(epoch));
        assert!(!gate.begin_offload(epoch + 1));
        assert!(gate.begin_offload(epoch));
        assert!(!gate.begin_offload(epoch));
        gate.wake("network_rx");
        assert_eq!(gate.status().wake_reason.as_deref(), Some("network_rx"));
        assert!(gate.status().claimed, "writeback must not unpark the vCPU");
        gate.retire(epoch);
        assert!(gate.offered());
        gate.resumed();
        assert!(!gate.offered());
        let next = gate.arm(Duration::from_secs(1)).unwrap();
        gate.probe();
        gate.offer(500, true);
        assert!(gate.claim(next));
        assert!(gate.begin_offload(next));
    }
    #[test]
    fn offload_rejects_revoked_and_short_or_expired_claims() {
        for remaining in [Duration::ZERO, Duration::from_millis(1)] {
            let (gate, epoch) = armed();
            gate.probe();
            gate.offer(500, true);
            assert!(gate.claim(epoch));
            gate.state.lock().unwrap().until = Some(Instant::now() + remaining);
            assert!(!gate.begin_offload(epoch));
            assert!(gate.status().claimed);
        }
        let (gate, epoch) = armed();
        gate.probe();
        gate.offer(500, true);
        assert!(gate.claim(epoch));
        gate.wake("device_ram");
        assert!(!gate.begin_offload(epoch));
    }
    #[test]
    fn offer_uses_conservative_probe_anchor() {
        let (gate, _) = armed();
        gate.probe();
        let anchor = Instant::now() - Duration::from_millis(10);
        gate.state.lock().unwrap().probe = Some(anchor);
        gate.offer(50, true);
        assert_eq!(
            gate.state.lock().unwrap().until,
            Some(anchor + Duration::from_millis(48))
        );
    }
}

#[cfg(test)]
mod deadline_tests {
    use super::*;
    #[test]
    fn successful_handoff_replaces_acquisition_timeout() {
        let gate = IdleGate::default();
        gate.enable(true);
        let epoch = gate.arm(Duration::from_millis(100)).unwrap();
        gate.probe();
        gate.offer(30_000, true);
        assert!(gate.status().remaining_ns > 29_000_000_000);
        assert!(gate.claim(epoch));
        assert!(gate.begin_offload(epoch));
    }
    #[test]
    fn explicit_no_timer_waits_for_external_event() {
        let gate = IdleGate::default();
        gate.enable(true);
        let epoch = gate.arm(Duration::from_secs(1)).unwrap();
        gate.probe();
        gate.offer(u32::MAX, true);
        assert_eq!(gate.status().remaining_ns, u64::MAX);
        assert!(gate.claim(epoch));
        assert!(gate.begin_offload(epoch));
        gate.wake("network_rx");
        assert_eq!(gate.status().remaining_ns, 0);
        assert_eq!(gate.status().wake_reason.as_deref(), Some("network_rx"));
        assert!(gate.offered());
    }
    #[test]
    fn optional_scans_defer_but_real_completion_revokes_no_timer_handoff() {
        let gate = IdleGate::default();
        gate.enable(true);
        gate.host_read(true);
        assert_eq!(gate.status().pending_host_reads, 1);
        let epoch = gate.arm(Duration::from_secs(1)).unwrap();
        assert!(!gate.defer_maintenance());
        gate.probe();
        assert!(gate.defer_maintenance());
        gate.offer(u32::MAX, true);
        assert!(gate.claim(epoch));
        gate.host_read(false);
        let status = gate.status();
        assert_eq!(status.pending_host_reads, 0);
        assert_eq!(status.wake_reason.as_deref(), Some("host_read_ready"));
        assert!(!status.offered);
        assert!(
            gate.defer_maintenance(),
            "claimed pause still owns the RAM gate"
        );
        gate.resumed();
        assert!(!gate.defer_maintenance());
    }
    #[test]
    fn acquisition_expiry_is_not_a_guest_timer() {
        let gate = IdleGate::default();
        gate.enable(true);
        gate.arm(Duration::from_secs(1)).unwrap();
        gate.state.lock().unwrap().until = Some(Instant::now());
        assert_eq!(
            gate.status().wake_reason.as_deref(),
            Some("handoff_timeout")
        );
        assert_eq!(gate.probe(), 0);
    }
}
