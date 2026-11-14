//! Region scheduling inspired by UKSM's hierarchical region distilling (FAST
//! 2018, section 5). This is an independent userspace policy, not the kernel
//! patch or its adaptive partial hash. Content identity and validation remain
//! the store's responsibility. Linux and macOS physical-page workers share it.
//! Reference: <https://www.usenix.org/system/files/conference/fast18/fast18-xia.pdf>
//!
//! Each RAM mapping is split into at most 2 MiB regions. Every round retains a
//! low-rate exploration quota; successful merges increase a region's quota,
//! while observed COW/publication races reduce it. A seeded coprime walk covers
//! every page before repeating, without a page-sized permutation allocation.
//! After bootstrap, full low-yield regions group four exploration quotas into
//! one of four rounds. A full region is covered per 32 rounds on both 4 and
//! 16 KiB hosts. Independent rung work/rest budgets prevent rich regions from
//! accelerating poor ones. As in UKSM, promotion subtracts the COW penalty;
//! unlike the kernel, work attribution here uses conservative wall time.
use std::{
    collections::VecDeque,
    ops::Range,
    time::{Duration, Instant},
};

// Byte budgets keep native-page geometry from multiplying work on macOS.
const REGION_BYTES: usize = 2 * 1024 * 1024;
const QUOTA_BYTES: [usize; 4] = [64 * 1024, 256 * 1024, 1024 * 1024, REGION_BYTES];
const VISIT_BYTES: usize = 256 * 1024;
const PROBE_BYTES: usize = 16 * 1024 * 1024;
// Local work/rest targets, not measured host CPU percentages. The bottom
// rung explores cheaply; richer rungs receive progressively more effort.
const RUNG_WORK_PER_MILLE: [u32; 4] = [2, 25, 50, 100];
const MAX_RUNG_REST: Duration = Duration::from_secs(1);
pub(super) const BATCH_BYTES: usize = 1024 * 1024;
pub(super) const WORK_BUDGET: Duration = Duration::from_millis(8);
pub(super) const REJECT_RETRY: Duration = Duration::from_millis(250);
pub(super) const INVALIDATED_RETRY: Duration = Duration::from_secs(30);
#[cfg(test)]
const REGION_PAGES: usize = 512;
#[cfg(test)]
const QUOTAS: [usize; 4] = [16, 64, 256, 512];

struct Region {
    pages: Range<usize>,
    offset: usize,
    stride: usize,
    remaining: usize,
    sampled: u32,
    observed: u32,
    limited: u32,
    merged: u32,
    churn: u32,
    level: usize,
    visited: u16,
    quiet_left: u8,
}

#[derive(Default)]
struct Rung {
    next_at: Option<Instant>,
    probes: u32,
    samples: u32,
    inspected: Duration,
}

pub(super) struct Visit {
    /// Full contiguous region for one bounded residency query.
    pub(super) region: Range<usize>,
    /// Permuted page indexes selected from this region's current quota.
    pub(super) pages: Vec<usize>,
}

pub(super) struct Scanner {
    regions: Vec<Region>,
    page_bytes: usize,
    cursor: usize,
    unfinished: usize,
    pub(super) probes: u64,
    pub(super) merges: u64,
    pub(super) cow_breaks: u64,
    pub(super) unstable: u64,
    ready: VecDeque<usize>,
    bootstrap_until: Option<Instant>,
    work_delay: Duration,
    rungs: [Rung; 4],
    rung_budgets: bool,
}

impl Scanner {
    pub(super) fn new(
        mappings: impl Iterator<Item = Range<usize>>,
        page_bytes: usize,
        seed: u64,
    ) -> Self {
        assert!(matches!(page_bytes, 4096 | 16384));
        let region_pages = REGION_BYTES / page_bytes;
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        use rand::{Rng, SeedableRng};
        let mut regions = Vec::new();
        for mapping in mappings {
            for first in (mapping.start..mapping.end).step_by(region_pages) {
                let end = (first + region_pages).min(mapping.end);
                let len = end - first;
                let offset = rng.random_range(0..len);
                let mut stride = rng.random_range(1..=len);
                while gcd(stride, len) != 1 {
                    stride = stride % len + 1;
                }
                regions.push(Region {
                    pages: first..end,
                    offset,
                    stride,
                    remaining: (QUOTA_BYTES[0] / page_bytes).min(len),
                    sampled: 0,
                    observed: 0,
                    limited: 0,
                    merged: 0,
                    churn: 0,
                    level: 0,
                    visited: 0,
                    quiet_left: 0,
                });
            }
        }
        let unfinished = regions.len();
        Self {
            regions,
            page_bytes,
            cursor: 0,
            unfinished,
            probes: 0,
            merges: 0,
            cow_breaks: 0,
            unstable: 0,
            ready: VecDeque::new(),
            bootstrap_until: None,
            work_delay: Duration::ZERO,
            rungs: std::array::from_fn(|_| Rung::default()),
            rung_budgets: true,
        }
    }

    /// HVF must detach COW shadow mappings before releasing pins. Until that
    /// backend has independent ownership maintenance, retain its batch pacing.
    #[cfg(any(target_os = "macos", test))]
    pub(super) fn disable_rung_budgets(&mut self) {
        self.rung_budgets = false;
        for rung in &mut self.rungs {
            rung.next_at = None;
        }
    }

    pub(super) fn round_complete(&self) -> bool {
        self.unfinished == 0
    }

    pub(super) fn delay(&self) -> Duration {
        let now = Instant::now();
        let floor = if self.round_complete() && self.ready.is_empty() && !self.bootstrapping_at(now)
        {
            REJECT_RETRY
        } else {
            Duration::from_millis(1)
        };
        let mut needed = [false; 4];
        for region in &self.regions {
            if self.round_complete() || region.remaining != 0 {
                needed[region.level] = true;
            }
        }
        for &page in &self.ready {
            needed[self.regions[self.region_index(page)].level] = true;
        }
        let rung_wait = if self.bootstrapping_at(now) || !self.rung_budgets {
            Duration::ZERO
        } else {
            self.rungs
                .iter()
                .zip(needed)
                .filter(|(_, needed)| *needed)
                .map(|(rung, _)| {
                    rung.next_at
                        .map_or(Duration::ZERO, |next| next.saturating_duration_since(now))
                })
                .min()
                .unwrap_or_default()
        };
        floor.max(self.work_delay).max(rung_wait)
    }

    fn bootstrapping_at(&self, now: Instant) -> bool {
        self.bootstrap_until.is_some_and(|until| now < until)
    }

    fn rung_due(&self, level: usize, now: Instant) -> bool {
        !self.rung_budgets
            || self.bootstrapping_at(now)
            || self.rungs[level].next_at.is_none_or(|next| now >= next)
    }

    /// Conservative worker wall-time duty cycle, not a host CPU percentage:
    /// bootstrap permits one busy interval per equal rest; every later batch
    /// rests for nine work intervals, including productive or hinted batches.
    /// Hints and merges prioritize candidates, never renew the global budget.
    /// I/O wait counts as work.
    pub(super) fn finished_batch(&mut self, work: Duration) {
        let now = Instant::now();
        let bootstrap = self.bootstrapping_at(now);
        self.work_delay = work.saturating_mul(if bootstrap { 1 } else { 9 });
        let inspected: Duration = self.rungs.iter().map(|rung| rung.inspected).sum();
        let extra = work.saturating_sub(inspected);
        let samples: u32 = self.rungs.iter().map(|rung| rung.samples).sum();
        let probes: u32 = self.rungs.iter().map(|rung| rung.probes).sum();
        for (level, rung) in self.rungs.iter_mut().enumerate() {
            // Residency queries are timed at their own rung. Attribute capture,
            // publication and barrier overhead by sampled-page count; without
            // samples, use probes. This is a conservative wall-time estimate,
            // not the kernel's task_sched_runtime or per-rung CPU accounting.
            let (weight, total) = if samples != 0 {
                (rung.samples, samples)
            } else {
                (rung.probes, probes)
            };
            let cost = rung.inspected
                + if total != 0 {
                    (extra / total).saturating_mul(weight)
                } else {
                    Duration::ZERO
                };
            if self.rung_budgets && !bootstrap && !cost.is_zero() {
                let share = RUNG_WORK_PER_MILLE[level];
                let rest = cost.saturating_mul(1000 - share) / share;
                // Bound each rung's advisory pause for responsiveness. The
                // independent whole-batch 9:1 rest above remains mandatory.
                rung.next_at = Some(now + rest.min(MAX_RUNG_REST));
            }
            rung.probes = 0;
            rung.samples = 0;
            rung.inspected = Duration::ZERO;
        }
    }

    pub(super) fn ready(&mut self, page: usize) {
        let index = self
            .regions
            .partition_point(|region| region.pages.start <= page);
        if index != 0
            && self.regions[index - 1].pages.contains(&page)
            && self.ready.len() < 256
            && !self.ready.contains(&page)
        {
            self.ready.push_back(page);
        }
    }

    /// Backends inspect only the selected visit and append at most one result
    /// per index. Empty/sparse visits consume quota too. Even an expired soft
    /// budget permits one due background visit; a rung awaiting credit retains
    /// its quota and hints. Foreground hints cannot starve due exploration.
    pub(super) fn select<T, E>(
        &mut self,
        limit: usize,
        started: Instant,
        mut inspect: impl FnMut(Visit, &mut Vec<T>) -> Result<(), E>,
    ) -> Result<Vec<T>, E> {
        let mut selected = Vec::new();
        let mut visited = 0;
        let probe_limit = PROBE_BYTES / self.page_bytes;
        // Hints never consume more than half a batch or replace background
        // quota. Even after an expired hint budget one due background visit remains.
        let mut hint_budget = self.ready.len().min(limit / 2);
        let mut hints_checked = self.ready.len();
        while hint_budget != 0 && hints_checked != 0 {
            if started.elapsed() >= WORK_BUDGET {
                break;
            }
            hints_checked -= 1;
            let page = self.ready.pop_front().unwrap();
            let level = self.regions[self.region_index(page)].level;
            if !self.rung_due(level, started) {
                self.ready.push_back(page);
                continue;
            }
            let region = self.region_mut(page).pages.clone();
            let mut pages = vec![page];
            self.ready.retain(|page| {
                if pages.len() < hint_budget && region.contains(page) {
                    pages.push(*page);
                    false
                } else {
                    true
                }
            });
            hint_budget -= pages.len();
            self.probes += pages.len() as u64;
            visited += pages.len();
            self.rungs[level].probes += pages.len() as u32;
            let inspect_started = Instant::now();
            inspect(Visit { region, pages }, &mut selected)?;
            self.rungs[level].inspected += inspect_started.elapsed();
        }
        let mut background_visits = 0;
        while selected.len() < limit
            && visited < probe_limit
            && (background_visits == 0 || started.elapsed() < WORK_BUDGET)
        {
            let Some(visit) = self.next_at(
                (limit - selected.len()).min(probe_limit - visited),
                Some(started),
            ) else {
                break;
            };
            let level = self.regions[self.region_index(visit.region.start)].level;
            visited += visit.pages.len();
            background_visits += 1;
            let inspect_started = Instant::now();
            inspect(visit, &mut selected)?;
            self.rungs[level].inspected += inspect_started.elapsed();
            debug_assert!(selected.len() <= limit);
        }
        Ok(selected)
    }

    /// Called only between publication batches. Feedback from the previous
    /// batch has been recorded before the next round's quota is chosen.
    pub(super) fn begin_round(&mut self) {
        if !self.round_complete() {
            return;
        }
        for region in &mut self.regions {
            let informative = region
                .sampled
                .saturating_sub(region.observed + region.limited);
            region.level = if region.churn > 0 && region.churn * 2 >= region.merged {
                0
            } else if informative == 0 && region.sampled != 0 {
                // Waiting for another session or storage capacity is not
                // evidence that this region lacks duplicates.
                region.level
            } else if informative >= (QUOTA_BYTES[0] / self.page_bytes / 2) as u32
                && region.merged.saturating_sub(region.churn) * 10 >= informative
            {
                (region.level + 1).min(QUOTA_BYTES.len() - 1)
            } else {
                region.level.saturating_sub(1)
            };
            if region.level == 0
                && region.pages.len() == REGION_BYTES / self.page_bytes
                && usize::from(region.visited) == REGION_BYTES / self.page_bytes
            {
                if region.quiet_left == 0 {
                    region.remaining = QUOTA_BYTES[0] / self.page_bytes * 4;
                    region.quiet_left = 3;
                } else {
                    region.remaining = 0;
                    region.quiet_left -= 1;
                }
            } else {
                region.quiet_left = 0;
                region.remaining =
                    (QUOTA_BYTES[region.level] / self.page_bytes).min(region.pages.len());
            }
            region.sampled = 0;
            region.observed = 0;
            region.limited = 0;
            region.merged = 0;
            region.churn = 0;
        }
        self.unfinished = self
            .regions
            .iter()
            .filter(|region| region.remaining != 0)
            .count();
    }

    /// Round-robin bounded visits prevent a rich region from starving another
    /// region. Even absent/cooling/already-shared pages consume probe quota.
    #[cfg(test)]
    pub(super) fn next(&mut self, limit: usize) -> Option<Visit> {
        self.next_at(limit, None)
    }

    fn next_at(&mut self, limit: usize, now: Option<Instant>) -> Option<Visit> {
        if self.round_complete() || limit == 0 {
            return None;
        }
        let due: [bool; 4] =
            std::array::from_fn(|level| now.is_none_or(|now| self.rung_due(level, now)));
        for _ in 0..self.regions.len() {
            let index = self.cursor;
            self.cursor = (index + 1) % self.regions.len();
            let region = &mut self.regions[index];
            if region.remaining == 0 || !due[region.level] {
                continue;
            }
            let count = region
                .remaining
                .min(VISIT_BYTES / self.page_bytes)
                .min(limit);
            let mut pages = Vec::with_capacity(count);
            for _ in 0..count {
                pages.push(region.pages.start + region.offset);
                region.offset = (region.offset + region.stride) % region.pages.len();
            }
            region.remaining -= count;
            region.visited =
                (usize::from(region.visited) + count).min(REGION_BYTES / self.page_bytes) as u16;
            if region.remaining == 0 {
                self.unfinished -= 1;
            }
            self.probes += count as u64;
            self.rungs[region.level].probes += count as u32;
            return Some(Visit {
                region: region.pages.clone(),
                pages,
            });
        }
        None
    }

    fn region_index(&self, page: usize) -> usize {
        let index = self
            .regions
            .partition_point(|region| region.pages.start <= page)
            - 1;
        debug_assert!(self.regions[index].pages.contains(&page));
        index
    }

    fn region_mut(&mut self, page: usize) -> &mut Region {
        let index = self.region_index(page);
        &mut self.regions[index]
    }

    pub(super) fn sampled(&mut self, page: usize) {
        if self.bootstrap_until.is_none() {
            self.bootstrap_until = Some(Instant::now() + Duration::from_secs(2));
        }
        let region = self.region_mut(page);
        region.sampled += 1;
        let level = region.level;
        self.rungs[level].samples += 1;
    }

    pub(super) fn observed(&mut self, page: usize) {
        self.region_mut(page).observed += 1;
    }

    pub(super) fn limited(&mut self, page: usize) {
        self.region_mut(page).limited += 1;
    }

    pub(super) fn merged(&mut self, page: usize) {
        self.region_mut(page).merged += 1;
        self.merges += 1;
    }

    pub(super) fn cow(&mut self, page: usize) {
        self.region_mut(page).churn += 1;
        self.cow_breaks += 1;
    }

    pub(super) fn invalidated(&mut self, page: usize) {
        self.region_mut(page).churn += 1;
        self.unstable += 1;
    }

    pub(super) fn levels(&self) -> [usize; 4] {
        let mut counts = [0; 4];
        for region in &self.regions {
            counts[region.level] += 1;
        }
        counts
    }

    pub(super) fn report(&self, file_hint_candidates: u64) {
        eprintln!(
            "pvisor-shared-scan regions_by_level={:?} probed_pages_total={} successful_merges_total={} cow_breaks_total={} unstable_total={} file_hint_candidates_total={file_hint_candidates} pid={}",
            self.levels(),
            self.probes,
            self.merges,
            self.cow_breaks,
            self.unstable,
            std::process::id()
        );
    }
}

pub(super) fn deadline(time: Instant) -> u64 {
    static EPOCH: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
    time.saturating_duration_since(*EPOCH.get_or_init(Instant::now))
        .as_millis() as u64
}

// The callback owns the actual VM barrier; empty work must never enter it.
pub(super) fn quiesce_nonempty<T, R>(
    work: &mut [T],
    action: impl FnOnce(&mut [T]) -> Result<Option<R>, String>,
) -> Result<Option<R>, String> {
    if work.is_empty() {
        Ok(None)
    } else {
        action(work)
    }
}

fn gcd(mut a: usize, mut b: usize) -> usize {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn bootstrap_and_steady_work_have_explicit_rest_budgets() {
        let mut scanner = Scanner::new(std::iter::once(0..512), 4096, 1);
        scanner.sampled(0);
        scanner.finished_batch(Duration::from_millis(4));
        assert_eq!(scanner.delay(), Duration::from_millis(4));
        while scanner.next(512).is_some() {}
        assert_eq!(
            scanner.delay(),
            Duration::from_millis(4),
            "bootstrap must not wait 250 ms per round"
        );
        scanner.bootstrap_until = Some(Instant::now() - Duration::from_secs(1));
        scanner.begin_round();
        scanner.finished_batch(Duration::from_millis(4));
        assert_eq!(scanner.work_delay, Duration::from_millis(36));
        assert!(scanner.delay() >= scanner.work_delay);
        // Advance the rung credit without sleeping. The old whole-batch and
        // quiet-round floors must still apply after that credit is available.
        scanner.rungs[0].next_at = None;
        assert_eq!(scanner.delay(), Duration::from_millis(36));
        while scanner.next(512).is_some() {}
        assert_eq!(scanner.delay(), REJECT_RETRY);
        scanner.ready(17);
        assert_eq!(
            scanner.delay(),
            Duration::from_millis(36),
            "hints bypass the quiet-round floor, not duty-cycle rest"
        );
    }

    #[test]
    fn ongoing_merges_and_ready_hints_cannot_renew_the_bootstrap_budget() {
        for page_bytes in [4096, 16384] {
            let mut scanner =
                Scanner::new(std::iter::once(0..REGION_BYTES / page_bytes), page_bytes, 7);
            scanner.sampled(0);
            // Advance past the one-shot window without sleeping or depending
            // on the host's scheduling speed.
            let expired = Instant::now() - Duration::from_secs(1);
            scanner.bootstrap_until = Some(expired);
            let work = Duration::from_millis(4);
            let mut total_rest = Duration::ZERO;
            for page in 0..128 {
                scanner.sampled(page);
                scanner.merged(page);
                scanner.ready(page);
                scanner.finished_batch(work);
                assert_eq!(scanner.bootstrap_until, Some(expired));
                assert_eq!(scanner.work_delay, work * 9);
                assert!(scanner.delay() >= work * 9);
                total_rest += scanner.work_delay;
            }
            assert_eq!(total_rest, work * 128 * 9);
            assert_eq!(scanner.merges, 128, "productive feedback remains intact");
            assert_eq!(scanner.ready.len(), 128, "hints are retained, not disabled");
            while scanner.next(512).is_some() {}
            scanner.rungs[0].next_at = None;
            assert_eq!(
                scanner.delay(),
                work * 9,
                "queued hints bypass only the quiet-round floor"
            );
            scanner.ready.clear();
            assert_eq!(scanner.delay(), REJECT_RETRY);
        }
    }

    #[test]
    fn rung_rest_is_independent_and_never_relaxes_the_whole_batch_budget() {
        for bytes in [4096, 16384] {
            let mut scanner = Scanner::new(std::iter::once(0..REGION_BYTES / bytes), bytes, 1);
            for rung in &mut scanner.rungs {
                rung.inspected = Duration::from_millis(1);
            }
            scanner.finished_batch(Duration::from_millis(4));
            let fastest = scanner.rungs[3].next_at.unwrap();
            for (rung, extra_ms) in scanner.rungs.iter().zip([490, 30, 10, 0]) {
                assert_eq!(
                    rung.next_at.unwrap().duration_since(fastest),
                    Duration::from_millis(extra_ms)
                );
                assert_eq!(rung.probes, 0);
                assert_eq!(rung.samples, 0);
                assert!(rung.inspected.is_zero());
            }
            assert_eq!(scanner.work_delay, Duration::from_millis(36));
            scanner.rungs[0].inspected = Duration::from_secs(2);
            scanner.finished_batch(Duration::from_secs(2));
            assert_eq!(scanner.work_delay, Duration::from_secs(18));
            assert!(
                scanner.delay() >= Duration::from_secs(18),
                "the one-second per-rung pause bound must not bypass global rest"
            );
        }
    }

    #[test]
    fn rich_work_and_ready_hints_do_not_spend_a_poor_rungs_budget() {
        for bytes in [4096, 16384] {
            let pages = REGION_BYTES / bytes;
            let mut scanner = Scanner::new(std::iter::once(0..2 * pages), bytes, 9);
            scanner.regions[1].level = 3;
            scanner.rungs[0].inspected = Duration::from_millis(1);
            scanner.rungs[3].inspected = Duration::from_millis(1);
            scanner.finished_batch(Duration::from_millis(2));
            let poor_due = scanner.rungs[0].next_at.unwrap();
            let rich_due = scanner.rungs[3].next_at.unwrap();
            let remaining = scanner.regions[0].remaining;
            scanner.ready(0);
            let rich = scanner
                .select(64, rich_due, |visit, out| {
                    out.extend(visit.pages);
                    Ok::<_, ()>(())
                })
                .unwrap();
            assert!(!rich.is_empty());
            assert!(rich.iter().all(|page| *page >= pages));
            assert_eq!(scanner.regions[0].remaining, remaining);
            assert!(scanner.ready.contains(&0), "budgeted hints remain pending");
            assert_eq!(scanner.rungs[0].next_at, Some(poor_due));
            let background = scanner
                .select(64, poor_due, |visit, out| {
                    out.extend(visit.pages);
                    Ok::<_, ()>(())
                })
                .unwrap();
            assert!(background.iter().any(|page| *page < pages));
            assert!(!scanner.ready.contains(&0));
            assert!(
                scanner.round_complete(),
                "deferred quota must eventually run"
            );
        }
    }

    #[test]
    fn backends_without_reference_maintenance_retain_the_batch_budget() {
        let mut scanner = Scanner::new(std::iter::once(0..128), 16384, 9);
        scanner.rungs[0].inspected = Duration::from_millis(1);
        scanner.finished_batch(Duration::from_millis(1));
        assert!(scanner.rungs[0].next_at.is_some());
        scanner.disable_rung_budgets();
        assert!(scanner.rungs.iter().all(|rung| rung.next_at.is_none()));
        assert_eq!(scanner.delay(), Duration::from_millis(9));
        let selected = scanner
            .select(64, Instant::now(), |visit, out| {
                out.extend(visit.pages);
                Ok::<_, ()>(())
            })
            .unwrap();
        assert!(!selected.is_empty());
        scanner.finished_batch(Duration::from_millis(1));
        assert!(scanner.rungs.iter().all(|rung| rung.next_at.is_none()));
        assert_eq!(scanner.work_delay, Duration::from_millis(9));
    }

    #[test]
    fn promotion_uses_net_merges_after_cow_penalty() {
        for bytes in [4096, 16384] {
            let pages = REGION_BYTES / bytes;
            let count = 640 * 1024 / bytes;
            let mut scanner = Scanner::new(std::iter::once(0..pages), bytes, 3);
            for churn in [true, false] {
                scanner.regions[0].remaining = count;
                while let Some(visit) = scanner.next(pages) {
                    for page in visit.pages {
                        scanner.sampled(page);
                    }
                }
                for _ in 0..count / 8 {
                    scanner.merged(0);
                }
                if churn {
                    for _ in 0..count / 20 {
                        scanner.cow(0);
                    }
                }
                // 12.5% gross merging with 40% COW is only 7.5% net yield.
                scanner.begin_round();
                assert_eq!(scanner.regions[0].level, usize::from(!churn));
            }
        }
    }

    #[test]
    fn observation_and_capacity_feedback_do_not_penalize_region_yield() {
        let mut scanner = Scanner::new(std::iter::once(0..512), 4096, 2);
        while let Some(visit) = scanner.next(512) {
            for page in visit.pages {
                scanner.sampled(page);
                scanner.merged(page);
            }
        }
        scanner.begin_round();
        assert_eq!(scanner.levels(), [0, 1, 0, 0]);
        while let Some(visit) = scanner.next(512) {
            for page in visit.pages {
                scanner.sampled(page);
                scanner.observed(page);
            }
        }
        scanner.begin_round();
        assert_eq!(scanner.levels(), [0, 1, 0, 0]);
        while let Some(visit) = scanner.next(512) {
            for page in visit.pages {
                scanner.sampled(page);
                scanner.limited(page);
            }
        }
        scanner.begin_round();
        assert_eq!(scanner.levels(), [0, 1, 0, 0]);
        while let Some(visit) = scanner.next(512) {
            for page in visit.pages {
                scanner.sampled(page);
            }
        }
        scanner.begin_round();
        assert_eq!(
            scanner.levels(),
            [1, 0, 0, 0],
            "real unmatched observations still reduce priority"
        );
    }

    #[test]
    fn bounded_ready_hints_preserve_background_exploration() {
        let mut scanner = Scanner::new(std::iter::once(0..1024), 4096, 3);
        scanner.ready(usize::MAX);
        for page in 512..1024 {
            scanner.ready(page);
            scanner.ready(page);
        }
        assert_eq!(scanner.ready.len(), 256);
        let selected = scanner
            .select(64, Instant::now(), |visit, output| {
                output.extend(visit.pages);
                Ok::<_, ()>(())
            })
            .unwrap();
        assert_eq!(selected.len(), 64);
        assert_eq!(scanner.ready.len(), 224);
        assert!(
            selected.iter().any(|page| *page < 512),
            "hints must not consume the background budget"
        );
        scanner.begin_round();
        let before = scanner.probes;
        let _ = scanner
            .select(64, Instant::now() - WORK_BUDGET * 2, |visit, output| {
                output.extend(visit.pages);
                Ok::<_, ()>(())
            })
            .unwrap();
        assert!(
            scanner.probes > before,
            "expired hints must not starve exploration"
        );
    }

    #[test]
    fn poor_regions_eventually_visit_every_page_including_non_power_of_two_tails() {
        for len in [1, 2, 17, 511, 512, 513, 1301] {
            let mut scanner = Scanner::new(std::iter::once(0..len), 4096, 17);
            let mut seen = vec![BTreeSet::new(); scanner.regions.len()];
            for _ in 0..REGION_PAGES / QUOTAS[0] + 1 {
                scanner.begin_round();
                while let Some(visit) = scanner.next(7) {
                    assert!(visit.pages.len() <= 7);
                    for page in visit.pages {
                        assert!(visit.region.contains(&page));
                        seen[page / REGION_PAGES].insert(page);
                    }
                }
            }
            assert_eq!(seen.iter().map(BTreeSet::len).sum::<usize>(), len);
        }
    }

    #[test]
    fn each_region_walk_is_a_permutation_not_repeated_random_samples() {
        for len in [17, 127, 512] {
            let mut scanner = Scanner::new(std::iter::once(0..len), 4096, 91);
            let mut seen = BTreeSet::new();
            while seen.len() < len {
                scanner.begin_round();
                while let Some(visit) = scanner.next(1) {
                    assert!(seen.insert(visit.pages[0]));
                    if seen.len() == len {
                        break;
                    }
                }
            }
        }
    }

    #[test]
    fn productive_region_gets_more_work_without_starving_poor_regions() {
        let mut scanner = Scanner::new(std::iter::once(0..1536), 4096, 37);
        let mut counts = [0; 3];
        for _ in 0..4 {
            scanner.begin_round();
            counts = [0; 3];
            while let Some(visit) = scanner.next(256) {
                for page in visit.pages {
                    counts[page / REGION_PAGES] += 1;
                    scanner.sampled(page);
                    if page < REGION_PAGES {
                        scanner.merged(page);
                    }
                }
            }
        }
        assert_eq!(counts, [512, 16, 16]);
        scanner.cow(0);
        // Force a churn-heavy round, despite the previous successful merges.
        for _ in 0..256 {
            scanner.cow(1);
        }
        scanner.begin_round();
        assert_eq!(scanner.levels(), [3, 0, 0, 0]);
        let mut after_churn = [0; 3];
        while let Some(visit) = scanner.next(256) {
            after_churn[visit.region.start / REGION_PAGES] += visit.pages.len();
        }
        assert_eq!(
            after_churn,
            [64, 16, 16],
            "the bootstrapped rich region returns to the lowest average quota"
        );
    }

    #[test]
    fn changed_workload_recovers_from_low_priority_and_finished_sharing_slows_down() {
        let mut scanner = Scanner::new(std::iter::once(0..512), 4096, 1);
        for _ in 0..3 {
            scanner.begin_round();
            while let Some(visit) = scanner.next(512) {
                for page in visit.pages {
                    scanner.sampled(page);
                }
            }
        }
        assert_eq!(scanner.levels(), [1, 0, 0, 0]);
        scanner.begin_round();
        while let Some(visit) = scanner.next(512) {
            for page in visit.pages {
                scanner.sampled(page);
                scanner.merged(page);
            }
        }
        scanner.begin_round();
        assert_eq!(scanner.levels(), [0, 1, 0, 0]);
        while scanner.next(512).is_some() {}
        scanner.begin_round();
        assert_eq!(scanner.levels(), [1, 0, 0, 0]);
    }

    #[test]
    fn grouped_poor_regions_keep_sliding_coverage_and_reduce_region_queries() {
        let mut scanner = Scanner::new(std::iter::once(0..1024), 4096, 19);
        for _ in 0..32 {
            scanner.begin_round();
            let mut probes = 0;
            while let Some(visit) = scanner.next(64) {
                probes += visit.pages.len();
            }
            assert_eq!(probes, 32);
        }
        let mut rounds = Vec::new();
        let mut queries = 0;
        for _ in 0..64 {
            scanner.begin_round();
            let mut pages = BTreeSet::new();
            while let Some(visit) = scanner.next(64) {
                queries += 1;
                pages.extend(visit.pages);
            }
            rounds.push(pages);
        }
        assert_eq!(queries, 32, "two regions queried once per four rounds");
        for window in rounds.windows(32) {
            let covered: BTreeSet<_> = window
                .iter()
                .flat_map(|pages| pages.iter().copied())
                .collect();
            assert_eq!(covered.len(), 1024, "no change to full-walk coverage bound");
        }
    }

    #[test]
    fn successful_hinted_feedback_wakes_a_quiet_region_without_waiting_for_its_turn() {
        let mut scanner = Scanner::new(std::iter::once(0..512), 4096, 21);
        for _ in 0..33 {
            scanner.begin_round();
            while scanner.next(64).is_some() {}
        }
        scanner.begin_round();
        assert!(
            scanner.round_complete(),
            "the quiet region skips this metadata-query round"
        );
        for page in 0..16 {
            scanner.sampled(page);
            scanner.merged(page);
        }
        scanner.begin_round();
        assert_eq!(scanner.levels(), [0, 1, 0, 0]);
        assert!(scanner.next(64).is_some());
    }

    #[test]
    fn geometry_keeps_mapping_boundaries_and_small_metadata() {
        let scanner = Scanner::new([0..17, 17..1041].into_iter(), 4096, 1);
        assert_eq!(scanner.regions.len(), 3);
        assert_eq!(scanner.regions[0].pages, 0..17);
        assert_eq!(scanner.regions[1].pages, 17..529);
        assert_eq!(scanner.regions[2].pages, 529..1041);
        let scanner = Scanner::new(std::iter::once(0..512 * 1024 * 1024 / 4096), 4096, 1);
        assert!(scanner.regions.capacity() * std::mem::size_of::<Region>() <= 32 * 1024);
        let mut empty = Scanner::new(std::iter::empty(), 4096, 0);
        empty.begin_round();
        assert!(empty.next(1).is_none());
    }

    #[test]
    fn native_page_sizes_keep_the_same_byte_quotas_and_adaptive_feedback() {
        let mut histories = Vec::new();
        for bytes in [4096, 16384] {
            let region = REGION_BYTES / bytes;
            let mut scanner = Scanner::new(std::iter::once(0..region * 2), bytes, 37);
            let mut history = Vec::new();
            for round in 0..40 {
                scanner.begin_round();
                let mut work = [0; 2];
                while let Some(visit) = scanner.next(usize::MAX) {
                    assert!(visit.region.len() * bytes <= REGION_BYTES);
                    assert!(visit.pages.len() * bytes <= VISIT_BYTES);
                    for index in visit.pages {
                        work[index / region] += bytes;
                        scanner.sampled(index);
                        if index < region && round < 4 {
                            scanner.merged(index);
                        }
                    }
                }
                if round == 3 {
                    // Both COW and in-flight writes must cancel a rich region's
                    // promotion, regardless of the host's page size.
                    for _ in 0..region / 4 {
                        scanner.cow(0);
                        scanner.invalidated(1);
                    }
                }
                history.push((work, scanner.levels()));
            }
            assert_eq!(history[0].0, [64 * 1024; 2]);
            assert_eq!(history[3].0, [REGION_BYTES, 64 * 1024]);
            assert_eq!(history[4].1, [2, 0, 0, 0]);
            assert_eq!(scanner.cow_breaks, (region / 4) as u64);
            assert_eq!(scanner.unstable, (region / 4) as u64);
            histories.push(history);
        }
        assert_eq!(histories[0], histories[1]);
    }

    #[test]
    fn sixteen_kib_walks_cover_mapping_tails_and_group_quiet_regions() {
        let region = REGION_BYTES / 16384;
        for len in [1, 2, 17, 127, 128, 129, 327] {
            let mut scanner = Scanner::new([0..len, len..len + region].into_iter(), 16384, 17);
            let mut seen = std::collections::BTreeSet::new();
            let mut rounds = Vec::new();
            for _ in 0..96 {
                scanner.begin_round();
                let mut pages = BTreeSet::new();
                while let Some(visit) = scanner.next(7) {
                    assert!(visit.region.end <= len || visit.region.start >= len);
                    assert!(visit.pages.len() <= 7);
                    pages.extend(visit.pages);
                }
                seen.extend(pages.iter().copied());
                rounds.push(pages);
            }
            assert_eq!(seen.len(), len + region);
            for window in rounds[32..].windows(32) {
                let covered: BTreeSet<_> = window.iter().flat_map(|pages| pages.iter()).collect();
                assert_eq!(covered.len(), len + region);
            }
        }
    }

    #[test]
    fn selection_bounds_sparse_probes_and_retains_work_after_hint_preemption() {
        for bytes in [4096, 16384] {
            let mut scanner =
                Scanner::new(std::iter::once(0..1024 * 1024 * 1024 / bytes), bytes, 9);
            let stale = Instant::now() - WORK_BUDGET * 2;
            let selected = scanner
                .select(3, stale, |visit, out| {
                    out.extend(visit.pages);
                    Ok::<_, ()>(())
                })
                .unwrap();
            assert_eq!(selected.len(), 3);
            assert!(!scanner.round_complete());
            assert_eq!(scanner.delay(), Duration::from_millis(1));
            // Deliberately use a future start to test the probe cap without
            // depending on host scheduling speed.
            let start = Instant::now() + Duration::from_secs(60);
            let before = scanner.probes;
            let selected: Vec<usize> = scanner
                .select(BATCH_BYTES / bytes, start, |_, _| Ok::<_, ()>(()))
                .unwrap();
            assert!(selected.is_empty());
            assert_eq!((scanner.probes - before) as usize * bytes, PROBE_BYTES);
            while scanner.next(usize::MAX).is_some() {}
            assert_eq!(scanner.delay(), Duration::from_millis(250));
            scanner.begin_round();
            assert_eq!(
                scanner.select::<usize, _>(1, start, |_, _| Err("query failed")),
                Err("query failed")
            );
        }
    }
}
