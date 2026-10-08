//! Cooperative quiescence for the single VM hosted by a libkrun runner.
//!
//! Independent device workers must enter before touching guest memory, queue
//! state, or performing guest I/O, and keep the guard through completion.
//! Never hold a guard while waiting for the next event/descriptor. Acquire it
//! before device locks, so a paused worker cannot block an admitted operation.
//! The VMM stops vCPUs before closing this gate and stops its own event loop.
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

#[derive(Default)]
struct State {
    paused: bool,
    active: usize,
}

#[derive(Default)]
struct Gate {
    state: Mutex<State>,
    changed: Condvar,
}

impl Gate {
    fn enter(&self) -> ActivityGuard<'_> {
        let mut state = self.state.lock().unwrap();
        while state.paused {
            state = self.changed.wait(state).unwrap();
        }
        state.active += 1;
        ActivityGuard { gate: self }
    }

    fn pause(&self, timeout: Duration) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        let mut state = self.state.lock().unwrap();
        state.paused = true;
        while state.active != 0 {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(format!(
                    "device pause timed out with {} active operation(s)",
                    state.active
                ));
            }
            let (next, _) = self.changed.wait_timeout(state, remaining).unwrap();
            state = next;
        }
        Ok(())
    }

    fn resume(&self) {
        let mut state = self.state.lock().unwrap();
        state.paused = false;
        self.changed.notify_all();
    }
}

/// Admission token covering one complete device operation, including its
/// used-ring update and interrupt. Dropping it acknowledges completion.
pub struct ActivityGuard<'a> {
    gate: &'a Gate,
}

impl Drop for ActivityGuard<'_> {
    fn drop(&mut self) {
        let mut state = self.gate.state.lock().unwrap();
        state.active -= 1;
        if state.active == 0 {
            self.gate.changed.notify_all();
        }
    }
}

fn gate() -> &'static Gate {
    static GATE: OnceLock<Gate> = OnceLock::new();
    GATE.get_or_init(Gate::default)
}

/// Block new worker operations and wait for admitted operations to finish.
/// On timeout the gate stays closed. The caller must either explicitly resume
/// it during recovery, or retain a faulted VM state. A timeout is not a pause
/// acknowledgement and does not cancel a blocking host syscall.
pub fn pause(timeout: Duration) -> Result<(), String> {
    gate().pause(timeout)
}

/// Reopen admission, preserving the existing device objects and descriptors.
pub fn resume() {
    gate().resume();
}

pub fn enter() -> ActivityGuard<'static> {
    gate().enter()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{mpsc, Arc};
    use std::thread;

    #[test]
    fn pause_waits_for_inflight_completion_and_blocks_new_work() {
        let gate = Arc::new(Gate::default());
        let operation = gate.enter();
        let other = gate.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let pauser = thread::spawn(move || {
            done_tx.send(other.pause(Duration::from_secs(2))).unwrap();
        });
        // Observe closure under the same mutex used by admission; no timing
        // assumption about when the pauser is scheduled is needed.
        while !gate.state.lock().unwrap().paused {
            thread::yield_now();
        }
        assert!(matches!(done_rx.try_recv(), Err(mpsc::TryRecvError::Empty)));
        let other = gate.clone();
        let (entered_tx, entered_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _operation = other.enter();
            entered_tx.send(()).unwrap();
        });
        drop(operation);
        done_rx
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .unwrap();
        assert!(matches!(
            entered_rx.try_recv(),
            Err(mpsc::TryRecvError::Empty)
        ));
        gate.resume();
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        pauser.join().unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn timeout_keeps_admission_closed_until_explicit_recovery() {
        let gate = Gate::default();
        let operation = gate.enter();
        assert!(gate.pause(Duration::ZERO).is_err());
        assert!(gate.state.lock().unwrap().paused);
        drop(operation);
        gate.pause(Duration::ZERO).unwrap();
        gate.resume();
        let _operation = gate.enter();
    }

    #[test]
    fn repeated_pause_and_resume_are_idempotent() {
        let gate = Gate::default();
        gate.pause(Duration::ZERO).unwrap();
        gate.pause(Duration::ZERO).unwrap();
        gate.resume();
        gate.resume();
        let operation = gate.enter();
        assert_eq!(gate.state.lock().unwrap().active, 1);
        drop(operation);
        gate.pause(Duration::ZERO).unwrap();
    }
}
