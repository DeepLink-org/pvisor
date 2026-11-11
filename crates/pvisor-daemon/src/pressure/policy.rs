use super::api::{Config, Entry, ResidentMemoryAccounting, Snapshot, State};
use anyhow::{Result, ensure};
use std::collections::{BTreeMap, VecDeque};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Action {
    Offload(String),
    Resume(String),
}

pub(super) struct Policy {
    pub snapshot: Snapshot,
    pub config: Config,
    admission: VecDeque<String>,
    requests: VecDeque<String>,
    uncertain_writebacks: std::collections::BTreeSet<String>,
    pressure: bool,
}

impl Policy {
    pub fn new(config: Config) -> Result<Self> {
        ensure!(
            config.low_bytes > 0
                && config.low_bytes < config.high_bytes
                && config.high_bytes <= config.memory_bytes,
            "invalid memory watermarks"
        );
        ensure!(
            config.max_writebacks <= 16,
            "writeback ceiling must be between 0 and 16"
        );
        ensure!(
            config
                .waiting_memory_margin_bytes
                .is_none_or(|bytes| bytes > 0 && bytes <= config.memory_bytes),
            "invalid waiting memory margin"
        );
        ensure!(
            config.resident_memory_accounting != ResidentMemoryAccounting::Measured
                || config.waiting_memory_margin_bytes.is_none(),
            "measured resident accounting cannot use a waiting memory margin"
        );
        Ok(Self {
            snapshot: Snapshot {
                instance: uuid::Uuid::new_v4().to_string(),
                entries: BTreeMap::new(),
                writebacks: 0,
                measured_bytes: None,
                error: None,
            },
            config,
            admission: VecDeque::new(),
            requests: VecDeque::new(),
            uncertain_writebacks: Default::default(),
            pressure: false,
        })
    }

    pub fn reserve(&mut self, id: String, bytes: u64) -> Result<()> {
        ensure!(uuid::Uuid::parse_str(&id).is_ok(), "ID must be a UUID");
        ensure!(
            bytes > 0 && bytes <= self.config.memory_bytes,
            "invalid reservation"
        );
        if let Some(e) = self.snapshot.entries.get(&id) {
            ensure!(
                e.memory_bytes == bytes && e.state != State::Released,
                "ID already used"
            );
            return Ok(());
        }
        ensure!(
            self.snapshot.entries.len() < 1024,
            "registry capacity reached"
        );
        self.snapshot.entries.insert(
            id.clone(),
            Entry {
                memory_bytes: bytes,
                state: State::AdmissionQueued,
                token: None,
                error: None,
            },
        );
        self.admission.push_back(id);
        Ok(())
    }

    pub fn bound(&mut self, id: &str) -> Result<()> {
        let e = self.entry(id)?;
        ensure!(e.state == State::Admitted, "VM is not admitted");
        e.state = State::Active;
        Ok(())
    }

    pub fn wait(&mut self, id: &str, token: Option<String>) -> Result<()> {
        let e = self.entry(id)?;
        ensure!(
            (e.state == State::Active && token.is_none())
                || (e.state == State::Ready && e.token == token && token.is_some()),
            "wait requires the current execution permit"
        );
        e.state = State::Waiting;
        e.token = None;
        Ok(())
    }

    pub fn acquire(&mut self, id: &str, token: String) -> Result<()> {
        ensure!(
            uuid::Uuid::parse_str(&token).is_ok(),
            "request token must be a UUID"
        );
        let e = self.entry(id)?;
        if e.token.as_deref() == Some(&token) {
            return Ok(());
        }
        ensure!(e.token.is_none(), "another request already owns this VM");
        e.state = match e.state {
            State::Waiting => State::Queued,
            State::Offloaded => State::RestoreQueued,
            State::Offloading => State::Offloading,
            _ => anyhow::bail!("VM is not at a safe wait boundary"),
        };
        e.token = Some(token);
        self.requests.push_back(id.to_owned());
        Ok(())
    }

    pub fn release(&mut self, id: &str) -> Result<()> {
        let state = self.entry(id)?.state;
        ensure!(
            !matches!(state, State::Binding | State::Offloading | State::Restoring),
            "control still in flight"
        );
        if self.uncertain_writebacks.remove(id) {
            self.snapshot.writebacks -= 1;
        }
        self.entry(id)?.state = State::Released;
        self.admission.retain(|v| v != id);
        self.requests.retain(|v| v != id);
        Ok(())
    }

    fn entry(&mut self, id: &str) -> Result<&mut Entry> {
        self.snapshot
            .entries
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("unknown VM"))
    }

    fn top_up(&self, e: &Entry, charge: u64) -> u64 {
        reservation_top_up(e, charge, &self.config)
    }

    /// Capacity for one quiescent VM to regain its full execution allowance.
    /// It is a new-admission fence, not a second reservation on Acquire.
    fn wake_headroom(&self, charges: &BTreeMap<String, u64>) -> u64 {
        self.snapshot
            .entries
            .iter()
            .filter_map(|(id, e)| {
                if self.config.resident_memory_accounting == ResidentMemoryAccounting::Measured {
                    // Resident tools do not need a second full reservation.
                    // Preserve one recovery allowance before admitting new VMs,
                    // including offloads whose acknowledgement is still pending.
                    matches!(
                        e.state,
                        State::Offloading | State::Offloaded | State::RestoreQueued
                    )
                    .then(|| e.memory_bytes.saturating_sub(charges[id]))
                } else if self.config.waiting_memory_margin_bytes.is_some() {
                    matches!(
                        e.state,
                        State::Waiting
                            | State::Queued
                            | State::Offloading
                            | State::Offloaded
                            | State::RestoreQueued
                    )
                    .then(|| {
                        e.memory_bytes
                            .saturating_sub(charges[id])
                            .saturating_sub(self.top_up(e, charges[id]))
                    })
                } else {
                    matches!(e.state, State::Offloaded | State::RestoreQueued)
                        .then_some(e.memory_bytes)
                }
            })
            .max()
            .unwrap_or(0)
    }

    /// Include actual parent charge once, then top up state-specific commitments.
    /// Offloaded pages/cache remain in measured charge; reclaim report byte counts
    /// never become an accounting credit. Missing samples stop every grant.
    pub fn plan(&mut self, parent: u64, charges: &BTreeMap<String, u64>) -> Result<Vec<Action>> {
        let mut committed = parent;
        for (id, e) in &self.snapshot.entries {
            if matches!(e.state, State::AdmissionQueued | State::Released) {
                continue;
            }
            let charge = if e.state == State::Admitted {
                0
            } else {
                *charges
                    .get(id)
                    .ok_or_else(|| anyhow::anyhow!("missing charge for {id}"))?
            };
            committed = committed
                .checked_add(self.top_up(e, charge))
                .ok_or_else(|| anyhow::anyhow!("memory accounting overflow"))?;
        }
        self.snapshot.measured_bytes = Some(parent);
        self.snapshot.error = None;
        if self
            .snapshot
            .entries
            .values()
            .any(|e| e.state == State::Failed)
        {
            return Ok(Vec::new());
        }
        if parent >= self.config.high_bytes {
            self.pressure = true;
        }
        if parent <= self.config.low_bytes {
            self.pressure = false;
        }
        let mut actions = Vec::new();
        // Do not advance FIFO past a native control. Ready entries leave the
        // queue immediately; only Reserved mode retains their full allowance.
        while let Some(id) = self.requests.front().cloned() {
            let e = &self.snapshot.entries[&id];
            match e.state {
                State::Queued => {
                    let extra = if self.config.resident_memory_accounting
                        == ResidentMemoryAccounting::Measured
                    {
                        0
                    } else {
                        e.memory_bytes
                            .saturating_sub(charges[&id])
                            .saturating_sub(self.top_up(e, charges[&id]))
                    };
                    if !fits(committed, extra, self.config.memory_bytes) {
                        break;
                    }
                    committed += extra;
                    self.snapshot.entries.get_mut(&id).unwrap().state = State::Ready;
                    self.requests.pop_front();
                }
                State::RestoreQueued => {
                    let extra = e.memory_bytes.saturating_sub(charges[&id]);
                    if !fits(committed, extra, self.config.memory_bytes) {
                        break;
                    }
                    committed += extra;
                    self.snapshot.entries.get_mut(&id).unwrap().state = State::Restoring;
                    actions.push(Action::Resume(id));
                    break;
                }
                State::Offloading | State::Restoring | State::Failed => break,
                _ => anyhow::bail!("invalid request queue state"),
            }
        }
        // Keep capacity for at least one full restore before admitting another
        // VM. Restores take priority; this headroom is not counted twice on resume.
        if self.requests.is_empty()
            && self.snapshot.writebacks == 0
            && !(self.pressure
                && self.config.resident_memory_accounting == ResidentMemoryAccounting::Measured)
        {
            let headroom = self.wake_headroom(charges);
            while let Some(id) = self.admission.front().cloned() {
                let bytes = self.snapshot.entries[&id].memory_bytes;
                if !committed
                    .checked_add(bytes)
                    .is_some_and(|next| fits(next, headroom, self.config.memory_bytes))
                {
                    break;
                }
                committed += bytes;
                self.snapshot.entries.get_mut(&id).unwrap().state = State::Admitted;
                self.admission.pop_front();
            }
        }
        let blocked_request = self.requests.front().is_some_and(|id| {
            matches!(
                self.snapshot.entries[id].state,
                State::Queued | State::RestoreQueued
            )
        });
        let blocked_create = !self.admission.is_empty() && self.requests.is_empty();
        if self.pressure || blocked_request || blocked_create {
            let restore_extra = self
                .requests
                .front()
                .filter(|_| blocked_request)
                .map(|id| {
                    let e = &self.snapshot.entries[id];
                    if e.state == State::Queued
                        && self.config.resident_memory_accounting
                            == ResidentMemoryAccounting::Measured
                    {
                        0
                    } else {
                        e.memory_bytes
                            .saturating_sub(charges[id])
                            .saturating_sub(self.top_up(e, charges[id]))
                    }
                })
                .unwrap_or(0);
            let create_extra = self
                .admission
                .front()
                .filter(|_| blocked_create)
                .map(|id| {
                    self.snapshot.entries[id]
                        .memory_bytes
                        .saturating_add(self.wake_headroom(charges))
                })
                .unwrap_or(0);
            // Estimates select victims only; grants still require a fresh sample.
            // Physical relief cannot include unused reservation. Commitment
            // relief includes charge plus its state-specific reservation top-up.
            // Use both estimates for new victims and writes already in flight.
            let mut physical_relief = 0u64;
            let mut commitment_relief = 0u64;
            for (id, e) in &self.snapshot.entries {
                if e.state == State::Offloading {
                    let charge = charges[id];
                    physical_relief = physical_relief.saturating_add(charge);
                    commitment_relief = commitment_relief
                        .saturating_add(charge.saturating_add(self.top_up(e, charge)));
                }
            }
            let physical_need = if self.pressure {
                parent.saturating_sub(self.config.low_bytes)
            } else {
                0
            };
            let commitment_need = committed
                .saturating_add(restore_extra)
                .saturating_add(create_extra)
                .saturating_sub(self.config.memory_bytes);
            // Reserve slots before yielding to I/O. Never reclaim Active/Ready,
            // even when measurements show pressure. Queued resident followers
            // are eligible, avoiding the FIFO head-of-line deadlock.
            for (id, e) in &mut self.snapshot.entries {
                if self.snapshot.writebacks >= self.config.max_writebacks {
                    break;
                }
                if physical_relief >= physical_need && commitment_relief >= commitment_need {
                    break;
                }
                if matches!(e.state, State::Waiting | State::Queued) {
                    // Offloading a blocked resident head cannot create capacity
                    // for that same VM's full execution allowance. Reclaim safe
                    // followers instead; the head keeps its place and token.
                    if blocked_request && self.requests.front() == Some(id) {
                        continue;
                    }
                    let charge = charges.get(id).copied().unwrap_or(0);
                    let top_up = reservation_top_up(e, charge, &self.config);
                    // A fully shared VM may have zero private charge but still
                    // hold a waiting margin. Its acknowledged offload can
                    // relinquish that margin for a blocked request, never count
                    // it as physical relief. Keep legacy zero-charge selection.
                    if charge == 0
                        && (self.config.waiting_memory_margin_bytes.is_none()
                            || top_up == 0
                            || commitment_relief >= commitment_need)
                    {
                        continue;
                    }
                    e.state = State::Offloading;
                    self.snapshot.writebacks += 1;
                    physical_relief = physical_relief.saturating_add(charge);
                    commitment_relief =
                        commitment_relief.saturating_add(charge.saturating_add(top_up));
                    actions.push(Action::Offload(id.clone()));
                }
            }
        }
        Ok(actions)
    }

    pub fn finish(&mut self, action: Action, result: Result<()>) {
        let (id, offload) = match action {
            Action::Offload(id) => (id, true),
            Action::Resume(id) => (id, false),
        };
        let e = self.snapshot.entries.get_mut(&id).unwrap();
        match result {
            Ok(()) if offload => {
                self.snapshot.writebacks -= 1;
                e.state = if e.token.is_some() {
                    State::RestoreQueued
                } else {
                    State::Offloaded
                };
            }
            Ok(()) => {
                e.state = State::Ready;
                assert_eq!(self.requests.pop_front().as_deref(), Some(id.as_str()));
            }
            Err(error) => {
                e.state = State::Failed;
                e.error = Some(format!("{error:#}").chars().take(512).collect());
                if offload {
                    self.uncertain_writebacks.insert(id);
                }
            }
        }
    }
}

fn fits(committed: u64, extra: u64, budget: u64) -> bool {
    committed
        .checked_add(extra)
        .is_some_and(|total| total <= budget)
}

fn reservation_top_up(e: &Entry, charge: u64, config: &Config) -> u64 {
    let full = e.memory_bytes.saturating_sub(charge);
    if config.resident_memory_accounting == ResidentMemoryAccounting::Measured {
        return match e.state {
            State::Admitted | State::Binding | State::Restoring | State::Failed => full,
            _ => 0,
        };
    }
    match e.state {
        State::Offloaded | State::RestoreQueued => 0,
        State::Waiting | State::Queued | State::Offloading => config
            .waiting_memory_margin_bytes
            .map_or(full, |margin| full.min(margin)),
        _ => full,
    }
}
