//! Disposable fingerprint observations, never authority over a shared mapping.
//! Payload slots and live references have separate owners and budgets.
use std::collections::{HashMap, VecDeque};

/// Discovery protocol v2: unseeded XXH3-128 of a complete native page, encoded
/// little-endian. This is neither a content identity nor permission to map RAM.
pub(super) type Fingerprint = [u8; 16];

pub(super) fn fingerprint(bytes: &[u8]) -> Fingerprint {
    xxhash_rust::xxh3::xxh3_128(bytes).to_le_bytes()
}

#[derive(Clone, Copy)]
pub(super) struct PageObservation {
    pub(super) session: [u8; 16],
    pub(super) page: u64,
}

#[derive(Clone, Copy)]
pub(super) enum Observation {
    First,
    Revisit,
    Matched(Option<PageObservation>),
}

pub(super) struct FingerprintIndex {
    first_session: HashMap<Fingerprint, PageObservation>,
    order: VecDeque<Fingerprint>,
    limit: usize,
}

impl FingerprintIndex {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            first_session: HashMap::new(),
            order: VecDeque::new(),
            limit,
        }
    }

    /// Observations are hints: another session's hash does not prove equality.
    pub(super) fn observe(&mut self, id: Fingerprint, observation: PageObservation) -> Observation {
        if let Some(first) = self.first_session.get_mut(&id) {
            if first.session == observation.session {
                return Observation::Revisit;
            }
            // A hint is issued at most once per retained observation. Repeated
            // identical pages cannot flood a session's bounded ready queue.
            let hint = (first.page != 0).then_some(*first);
            first.page = 0;
            return Observation::Matched(hint);
        }
        if self.limit == 0 {
            return Observation::Revisit;
        }
        if self.first_session.len() == self.limit {
            let old = self.order.pop_front().unwrap();
            self.first_session.remove(&old);
        }
        self.first_session.insert(id, observation);
        self.order.push_back(id);
        Observation::First
    }

    pub(super) fn len(&self) -> usize {
        self.order.len()
    }

    /// Bounded retirement lets other sessions continue querying between passes.
    pub(super) fn forget(&mut self, session: [u8; 16], limit: usize) {
        for _ in 0..limit.min(self.order.len()) {
            let id = self.order.pop_front().unwrap();
            if self
                .first_session
                .get(&id)
                .is_some_and(|first| first.session == session)
            {
                self.first_session.remove(&id);
            } else {
                self.order.push_back(id);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observations_are_bounded_and_same_session_revisits_do_not_match() {
        let mut index = FingerprintIndex::new(2);
        let page = |id| PageObservation {
            session: [id; 16],
            page: 17,
        };
        assert!(matches!(
            index.observe([1; 16], page(1)),
            Observation::First
        ));
        assert!(matches!(
            index.observe([1; 16], page(1)),
            Observation::Revisit
        ));
        let Observation::Matched(Some(hint)) = index.observe([1; 16], page(2)) else {
            panic!("cross-session hint missing")
        };
        assert_eq!(hint.session, [1; 16]);
        assert_eq!(hint.page, 17);
        assert!(matches!(
            index.observe([1; 16], page(2)),
            Observation::Matched(None)
        ));
        assert!(matches!(
            index.observe([2; 16], page(2)),
            Observation::First
        ));
        assert!(matches!(
            index.observe([3; 16], page(3)),
            Observation::First
        ));
        assert_eq!(index.len(), 2);
        assert!(
            matches!(index.observe([1; 16], page(2)), Observation::First),
            "oldest hint was evicted"
        );
        index.forget([3; 16], 1);
        index.forget([3; 16], 2);
        assert_eq!(index.len(), 1);
        assert!(matches!(
            index.observe([1; 16], page(4)),
            Observation::Matched(_)
        ));
    }

    #[test]
    fn disabled_index_retains_nothing() {
        let mut index = FingerprintIndex::new(0);
        assert!(matches!(
            index.observe(
                [1; 16],
                PageObservation {
                    session: [1; 16],
                    page: 1
                }
            ),
            Observation::Revisit
        ));
        assert!(matches!(
            index.observe(
                [1; 16],
                PageObservation {
                    session: [2; 16],
                    page: 1
                }
            ),
            Observation::Revisit
        ));
        assert_eq!(index.len(), 0);
    }

    #[test]
    fn wire_fingerprints_use_unseeded_xxh3_128_little_endian() {
        assert_eq!(
            fingerprint(b""),
            0x99aa06d3014798d86001c324468d497fu128.to_le_bytes()
        );
        assert_eq!(
            fingerprint(b"abc"),
            0x06b05ab6733a618578af5f94892f3950u128.to_le_bytes()
        );
        for len in [4096, 16384] {
            let mut page = vec![7; len];
            let before = fingerprint(&page);
            assert_eq!(before, fingerprint(&page));
            page[len - 1] ^= 1;
            assert_ne!(before, fingerprint(&page));
        }
        assert_ne!(fingerprint(&[7; 4096]), fingerprint(&[7; 16384]));
    }
}
