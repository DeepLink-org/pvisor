//! Region scheduling inspired by UKSM's hierarchical region distilling (FAST
//! 2018, section 5). This is an independent userspace policy, not the kernel
//! patch or its adaptive partial hash. Content identity and validation remain
//! the store's responsibility. Only the Linux physical-page pager uses it.
//! Reference: <https://www.usenix.org/system/files/conference/fast18/fast18-xia.pdf>
//!
//! Each RAM mapping is split into at most 2 MiB regions. Every round retains a
//! low-rate exploration quota; successful merges increase a region's quota,
//! while observed COW/publication races reduce it. A seeded coprime walk covers
//! every page before repeating, without a page-sized permutation allocation.
//! After bootstrap, full low-yield regions group 64 probes into one of four
//! rounds. Coverage remains 512 pages per 32 rounds with fewer metadata walks.
use std::ops::Range;

const REGION_PAGES: usize = 512;
const QUOTAS: [usize; 4] = [16, 64, 256, 512];
const VISIT_PAGES: usize = 64;

struct Region {
    pages: Range<usize>,
    offset: usize,
    stride: usize,
    remaining: usize,
    sampled: u32,
    merged: u32,
    churn: u32,
    level: usize,
    visited: u16,
    quiet_left: u8,
}

pub(super) struct Visit {
    /// Full contiguous region for one bounded residency query.
    pub(super) region: Range<usize>,
    /// Permuted page indexes selected from this region's current quota.
    pub(super) pages: Vec<usize>,
}

pub(super) struct Scanner {
    regions: Vec<Region>,
    cursor: usize,
    unfinished: usize,
    pub(super) probes: u64,
    pub(super) merges: u64,
    pub(super) cow_breaks: u64,
    pub(super) unstable: u64,
}

impl Scanner {
    pub(super) fn new(mappings: impl Iterator<Item = Range<usize>>, seed: u64) -> Self {
        let mut rng = rand::rngs::StdRng::seed_from_u64(seed);
        use rand::{Rng, SeedableRng};
        let mut regions = Vec::new();
        for mapping in mappings {
            for first in (mapping.start..mapping.end).step_by(REGION_PAGES) {
                let end = (first + REGION_PAGES).min(mapping.end);
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
                    remaining: QUOTAS[0].min(len),
                    sampled: 0,
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
            cursor: 0,
            unfinished,
            probes: 0,
            merges: 0,
            cow_breaks: 0,
            unstable: 0,
        }
    }

    pub(super) fn round_complete(&self) -> bool {
        self.unfinished == 0
    }

    /// Called only between publication batches. Feedback from the previous
    /// batch has been recorded before the next round's quota is chosen.
    pub(super) fn begin_round(&mut self) {
        if !self.round_complete() {
            return;
        }
        for region in &mut self.regions {
            region.level = if region.churn > 0 && region.churn * 2 >= region.merged {
                0
            } else if region.sampled >= 8 && region.merged * 10 >= region.sampled {
                (region.level + 1).min(QUOTAS.len() - 1)
            } else {
                region.level.saturating_sub(1)
            };
            if region.level == 0
                && region.pages.len() == REGION_PAGES
                && usize::from(region.visited) == REGION_PAGES
            {
                if region.quiet_left == 0 {
                    region.remaining = QUOTAS[0] * 4;
                    region.quiet_left = 3;
                } else {
                    region.remaining = 0;
                    region.quiet_left -= 1;
                }
            } else {
                region.quiet_left = 0;
                region.remaining = QUOTAS[region.level].min(region.pages.len());
            }
            region.sampled = 0;
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
    pub(super) fn next(&mut self, limit: usize) -> Option<Visit> {
        if self.round_complete() || limit == 0 {
            return None;
        }
        loop {
            let index = self.cursor;
            self.cursor = (index + 1) % self.regions.len();
            let region = &mut self.regions[index];
            if region.remaining == 0 {
                continue;
            }
            let count = region.remaining.min(VISIT_PAGES).min(limit);
            let mut pages = Vec::with_capacity(count);
            for _ in 0..count {
                pages.push(region.pages.start + region.offset);
                region.offset = (region.offset + region.stride) % region.pages.len();
            }
            region.remaining -= count;
            region.visited = (usize::from(region.visited) + count).min(REGION_PAGES) as u16;
            if region.remaining == 0 {
                self.unfinished -= 1;
            }
            self.probes += count as u64;
            return Some(Visit {
                region: region.pages.clone(),
                pages,
            });
        }
    }

    fn region_mut(&mut self, page: usize) -> &mut Region {
        let index = self
            .regions
            .partition_point(|region| region.pages.start <= page)
            - 1;
        let region = &mut self.regions[index];
        debug_assert!(region.pages.contains(&page));
        region
    }

    pub(super) fn sampled(&mut self, page: usize) {
        self.region_mut(page).sampled += 1;
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
    fn poor_regions_eventually_visit_every_page_including_non_power_of_two_tails() {
        for len in [1, 2, 17, 511, 512, 513, 1301] {
            let mut scanner = Scanner::new(std::iter::once(0..len), 17);
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
            let mut scanner = Scanner::new(std::iter::once(0..len), 91);
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
        let mut scanner = Scanner::new(std::iter::once(0..1536), 37);
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
        let mut scanner = Scanner::new(std::iter::once(0..512), 1);
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
        let mut scanner = Scanner::new(std::iter::once(0..1024), 19);
        // The first walk retains the original small, frequent exploration.
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
        let mut scanner = Scanner::new(std::iter::once(0..512), 21);
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
        let scanner = Scanner::new([0..17, 17..1041].into_iter(), 1);
        assert_eq!(scanner.regions.len(), 3);
        assert_eq!(scanner.regions[0].pages, 0..17);
        assert_eq!(scanner.regions[1].pages, 17..529);
        assert_eq!(scanner.regions[2].pages, 529..1041);
        let scanner = Scanner::new(std::iter::once(0..512 * 1024 * 1024 / 4096), 1);
        assert!(scanner.regions.capacity() * std::mem::size_of::<Region>() <= 32 * 1024);
        let mut empty = Scanner::new(std::iter::empty(), 0);
        empty.begin_round();
        assert!(empty.next(1).is_none());
    }
}
