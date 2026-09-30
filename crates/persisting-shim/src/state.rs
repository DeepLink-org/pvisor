//! Task lifecycle bookkeeping shared by the task service and the exit
//! watcher. Pure data manipulation so transitions stay unit-testable.

use std::path::PathBuf;
use std::time::SystemTime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TaskStatus {
    Created,
    Running,
    Stopped,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitInfo {
    pub status: u32,
    pub exited_at: SystemTime,
}

/// One tracked task (container or exec id). For M1 only init processes are
/// tracked; exec ids arrive with M2.
#[derive(Clone, Debug)]
pub struct TaskEntry {
    pub id: String,
    pub bundle: PathBuf,
    pub stdin: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub terminal: bool,
    pub pid: Option<u32>,
    pub status: TaskStatus,
    pub exit: Option<ExitInfo>,
}

impl TaskEntry {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: &str,
        bundle: PathBuf,
        stdin: Option<String>,
        stdout: Option<String>,
        stderr: Option<String>,
        terminal: bool,
        pid: Option<u32>,
    ) -> Self {
        TaskEntry {
            id: id.to_string(),
            bundle,
            stdin,
            stdout,
            stderr,
            terminal,
            pid,
            status: TaskStatus::Created,
            exit: None,
        }
    }

    /// Transition Created -> Running. Returns whether the transition applied.
    pub fn mark_started(&mut self, pid: u32) -> bool {
        if self.status != TaskStatus::Created {
            return false;
        }
        self.status = TaskStatus::Running;
        self.pid = Some(pid);
        true
    }

    /// Record the process exit. Transitions from Created (killed before
    /// start) and Running are both legal; repeated notifications are no-ops.
    pub fn mark_exited(&mut self, status: u32, exited_at: SystemTime) -> bool {
        if matches!(self.status, TaskStatus::Stopped) {
            return false;
        }
        self.status = TaskStatus::Stopped;
        self.exit = Some(ExitInfo { status, exited_at });
        true
    }

    /// True once the task may be deleted per the task v2 protocol.
    pub fn can_delete(&self) -> bool {
        self.status == TaskStatus::Stopped
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> TaskEntry {
        TaskEntry::new(
            "c1",
            PathBuf::from("/bundle"),
            None,
            None,
            None,
            false,
            None,
        )
    }

    #[test]
    fn created_entry_waits_for_start() {
        let mut task = entry();
        assert_eq!(task.status, TaskStatus::Created);
        assert!(!task.can_delete());
        assert!(task.mark_started(42));
        assert_eq!(task.status, TaskStatus::Running);
        assert_eq!(task.pid, Some(42));
    }

    #[test]
    fn start_is_idempotent_and_one_way() {
        let mut task = entry();
        assert!(task.mark_started(42));
        assert!(!task.mark_started(43));
        assert_eq!(task.pid, Some(42));
    }

    #[test]
    fn exit_from_running_and_created_are_legal() {
        let mut running = entry();
        running.mark_started(7);
        let at = SystemTime::now();
        assert!(running.mark_exited(3, at));
        assert_eq!(running.exit.map(|e| e.status), Some(3));
        assert!(!running.mark_exited(9, at), "second exit is a no-op");

        let mut created = entry();
        assert!(created.mark_exited(137, at));
        assert!(created.can_delete());
    }

    #[test]
    fn delete_requires_stopped() {
        let mut task = entry();
        assert!(!task.can_delete());
        task.mark_started(7);
        assert!(!task.can_delete());
        task.mark_exited(0, SystemTime::now());
        assert!(task.can_delete());
    }
}
