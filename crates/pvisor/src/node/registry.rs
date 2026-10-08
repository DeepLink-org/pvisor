//! Node-local ownership. Preparation is serialized per key, never under the map lock.
use anyhow::ensure;
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, Weak},
};

type Slot<T> = Arc<Mutex<Weak<T>>>;

pub struct Registry<T> {
    state: Mutex<State<T>>,
    limit: usize,
    warm_limit: usize,
}
struct State<T> {
    slots: BTreeMap<String, Slot<T>>,
    warm: BTreeMap<String, Arc<T>>,
}
impl<T> Registry<T> {
    pub fn new(limit: usize, warm_limit: usize) -> anyhow::Result<Self> {
        ensure!(
            limit > 0 && warm_limit <= limit,
            "invalid node owner budget"
        );
        Ok(Self {
            state: Mutex::new(State {
                slots: BTreeMap::new(),
                warm: BTreeMap::new(),
            }),
            limit,
            warm_limit,
        })
    }
    pub fn acquire(
        &self,
        key: String,
        load: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<Arc<T>> {
        let (slot, retired) = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            let mut retired = Vec::new();
            // Idle warm owners are expendable. Active pins and preparations are not.
            if !state.slots.contains_key(&key) {
                state.slots.retain(|_, slot| {
                    Arc::strong_count(slot) > 1
                        || slot
                            .try_lock()
                            .map(|owner| owner.strong_count() > 0)
                            .unwrap_or(true)
                });
                if state.slots.len() >= self.limit {
                    let idle: Vec<_> = state
                        .warm
                        .iter()
                        .filter(|(_, value)| Arc::strong_count(value) == 1)
                        .map(|(key, _)| key.clone())
                        .collect();
                    for idle in idle {
                        let value = state.warm.remove(&idle).unwrap();
                        if Arc::strong_count(&value) == 1
                            && state
                                .slots
                                .get(&idle)
                                .is_some_and(|slot| Arc::strong_count(slot) == 1)
                        {
                            state.slots.remove(&idle);
                        }
                        retired.push(value);
                    }
                }
                ensure!(
                    state.slots.len() < self.limit,
                    "node active owner budget exhausted"
                );
            }
            let slot = state
                .slots
                .entry(key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(Weak::new())))
                .clone();
            (slot, retired)
        };
        drop(retired); // FUSE teardown may block; no registry lock is held.
        let mut owner = slot.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(value) = owner.upgrade() {
            return Ok(value);
        }
        let value = Arc::new(load()?);
        *owner = Arc::downgrade(&value);
        drop(owner);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.warm.len() < self.warm_limit {
            state.warm.insert(key, value.clone());
        }
        Ok(value)
    }
    pub fn live(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .slots
            .values()
            .filter(|slot| {
                slot.try_lock()
                    .map(|v| v.strong_count() > 0)
                    .unwrap_or(true)
            })
            .count()
    }
    /// Call from a blocking worker: returned owners may perform FUSE teardown.
    pub fn trim_idle(&self) -> Vec<Arc<T>> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let keys: Vec<_> = state
            .warm
            .iter()
            .filter(|(_, owner)| Arc::strong_count(owner) == 1)
            .map(|(key, _)| key.clone())
            .collect();
        keys.into_iter()
            .filter_map(|key| state.warm.remove(&key))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    #[test]
    fn pins_survive_pressure_and_same_key_shares_across_sessions() {
        let registry = Registry::new(1, 1).unwrap();
        let first = registry.acquire("a".into(), || Ok(7)).unwrap();
        let second = registry
            .acquire("a".into(), || panic!("duplicate preparation"))
            .unwrap();
        assert!(Arc::ptr_eq(&first, &second));
        assert!(registry.acquire("b".into(), || Ok(8)).is_err());
        drop(first);
        drop(second);
        assert_eq!(*registry.acquire("b".into(), || Ok(8)).unwrap(), 8);
    }
    #[test]
    fn failed_preparation_retries_without_publishing_partial_owner() {
        let registry = Registry::new(1, 0).unwrap();
        assert!(
            registry
                .acquire("a".into(), || anyhow::bail!("failure"))
                .is_err()
        );
        assert_eq!(*registry.acquire("a".into(), || Ok(9)).unwrap(), 9);
    }
    #[test]
    fn concurrent_preparation_runs_once() {
        let registry = Registry::new(2, 0).unwrap();
        let loads = AtomicUsize::new(0);
        let barrier = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            let jobs: Vec<_> = (0..4)
                .map(|_| {
                    scope.spawn(|| {
                        let value = registry
                            .acquire("same".into(), || {
                                loads.fetch_add(1, Ordering::SeqCst);
                                Ok(5)
                            })
                            .unwrap();
                        barrier.wait();
                        value
                    })
                })
                .collect();
            let owners: Vec<_> = jobs.into_iter().map(|job| job.join().unwrap()).collect();
            assert!(owners.iter().all(|owner| Arc::ptr_eq(owner, &owners[0])));
        });
        assert_eq!(loads.load(Ordering::SeqCst), 1);
    }
}
