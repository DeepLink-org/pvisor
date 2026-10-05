//! Aggregate retained userspace cache payload. Metadata, scratch and kernel pages
//! remain covered by process/cgroup limits, not this payload counter.
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicUsize, Ordering},
};
#[derive(Clone, Debug)]
pub(crate) struct Budget(Arc<Inner>);
#[derive(Debug)]
struct Inner {
    limit: usize,
    used: AtomicUsize,
    misses: AtomicUsize,
}
#[derive(Debug)]
pub(crate) struct Charge {
    budget: Budget,
    bytes: usize,
}
impl Budget {
    fn new(limit: usize) -> Self {
        Self(Arc::new(Inner {
            limit,
            used: AtomicUsize::new(0),
            misses: AtomicUsize::new(0),
        }))
    }
    fn reserve(&self, bytes: usize) -> Result<Charge, ()> {
        self.0
            .used
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |used| {
                used.checked_add(bytes).filter(|sum| *sum <= self.0.limit)
            })
            .map_err(|_| {
                self.0.misses.fetch_add(1, Ordering::SeqCst);
            })?;
        Ok(Charge {
            budget: self.clone(),
            bytes,
        })
    }
}
impl Drop for Charge {
    fn drop(&mut self) {
        self.budget.0.used.fetch_sub(self.bytes, Ordering::SeqCst);
    }
}
static NODE: OnceLock<Budget> = OnceLock::new();
pub(crate) fn configure(limit: usize) -> anyhow::Result<()> {
    anyhow::ensure!(limit > 0, "node cache payload budget must be positive");
    NODE.set(Budget::new(limit))
        .map_err(|_| anyhow::anyhow!("node cache budget already configured"))
}
pub(crate) fn stats() -> (usize, usize, usize) {
    NODE.get()
        .map(|budget| {
            (
                budget.0.used.load(Ordering::SeqCst),
                budget.0.limit,
                budget.0.misses.load(Ordering::SeqCst),
            )
        })
        .unwrap_or((0, 0, 0))
}
pub(crate) fn reserve_replacing(
    bytes: usize,
    evict: impl FnMut() -> bool,
) -> Result<Option<Charge>, ()> {
    NODE.get()
        .map(|budget| budget.reserve_replacing(bytes, evict).map(Some))
        .unwrap_or(Ok(None))
}
impl Budget {
    fn reserve_replacing(
        &self,
        bytes: usize,
        mut evict: impl FnMut() -> bool,
    ) -> Result<Charge, ()> {
        if bytes > self.0.limit {
            self.0.misses.fetch_add(1, Ordering::SeqCst);
            return Err(());
        }
        loop {
            match self.reserve(bytes) {
                Ok(charge) => return Ok(charge),
                Err(()) if evict() => {}
                Err(()) => return Err(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aggregate_cache_charge_never_exceeds_limit_and_releases_on_eviction() {
        let budget = Budget::new(10);
        let first = budget.reserve(6).unwrap();
        assert!(budget.reserve(5).is_err());
        let second = budget.reserve(4).unwrap();
        assert_eq!(budget.0.used.load(Ordering::SeqCst), 10);
        drop(first);
        let third = budget.reserve(6).unwrap();
        drop((second, third));
        assert_eq!(budget.0.used.load(Ordering::SeqCst), 0);
        assert!(budget.reserve(usize::MAX).is_err());
    }
    #[test]
    fn full_budget_can_replace_local_cache_without_evicting_active_charge() {
        let budget = Budget::new(10);
        let active = budget.reserve(6).unwrap();
        let mut local = Some(budget.reserve(4).unwrap());
        let replacement = budget
            .reserve_replacing(4, || local.take().is_some())
            .unwrap();
        assert_eq!(budget.0.used.load(Ordering::SeqCst), 10);
        assert!(budget.reserve_replacing(5, || false).is_err());
        // Oversized data must not churn an otherwise useful local cache.
        assert!(
            budget
                .reserve_replacing(11, || panic!("oversized cache eviction"))
                .is_err()
        );
        drop((active, replacement));
        assert_eq!(budget.0.used.load(Ordering::SeqCst), 0);
    }
}
