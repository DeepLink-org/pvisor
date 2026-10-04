//! Task lifecycle bookkeeping shared by the task service and the exit
//! watcher. Pure data manipulation so transitions stay unit-testable.

use std::collections::HashMap;
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

/// Which process inside a task an exit belongs to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExitTarget {
    Init,
    Exec(String),
}

/// One `exec`-added process of a task.
#[derive(Clone, Debug)]
pub struct ExecEntry {
    pub exec_id: String,
    pub stdin: Option<String>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
    pub terminal: bool,
    pub pid: Option<u32>,
    pub status: TaskStatus,
    pub exit: Option<ExitInfo>,
}

impl ExecEntry {
    pub fn new(
        exec_id: &str,
        stdin: Option<String>,
        stdout: Option<String>,
        stderr: Option<String>,
        terminal: bool,
        pid: Option<u32>,
    ) -> Self {
        ExecEntry {
            exec_id: exec_id.to_string(),
            stdin,
            stdout,
            stderr,
            terminal,
            pid,
            status: TaskStatus::Created,
            exit: None,
        }
    }

    /// Transition Created -> Running.
    pub fn mark_started(&mut self, pid: u32) -> bool {
        if self.status != TaskStatus::Created {
            return false;
        }
        self.status = TaskStatus::Running;
        self.pid = Some(pid);
        true
    }

    /// Record the process exit; repeated notifications are no-ops.
    pub fn mark_exited(&mut self, status: u32, exited_at: SystemTime) -> bool {
        if self.status == TaskStatus::Stopped {
            return false;
        }
        self.status = TaskStatus::Stopped;
        self.exit = Some(ExitInfo { status, exited_at });
        true
    }
}

/// One tracked task (container init process plus its exec processes).
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
    pub execs: HashMap<String, ExecEntry>,
}

impl TaskEntry {
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
            execs: HashMap::new(),
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

    /// Record the init process exit. Transitions from Created (killed
    /// before start) and Running are both legal; repeats are no-ops.
    pub fn mark_exited(&mut self, status: u32, exited_at: SystemTime) -> bool {
        if self.status == TaskStatus::Stopped {
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

    /// Register an exec process; false when the exec id already exists.
    pub fn add_exec(&mut self, exec: ExecEntry) -> bool {
        if self.execs.contains_key(&exec.exec_id) {
            return false;
        }
        self.execs.insert(exec.exec_id.clone(), exec);
        true
    }

    /// Record an exec exit by id (VM execs report guest pids that must not
    /// be matched against host pids).
    pub fn record_exec_exit(&mut self, exec_id: &str, status: u32, exited_at: SystemTime) -> bool {
        self.execs
            .get_mut(exec_id)
            .is_some_and(|exec| exec.mark_exited(status, exited_at))
    }

    /// Record an exit for whichever process (init or exec) owns `pid`.
    /// Returns what got transitioned, if anything.
    pub fn record_exit_by_pid(
        &mut self,
        pid: u32,
        status: u32,
        exited_at: SystemTime,
    ) -> Option<ExitTarget> {
        if self.pid == Some(pid) && self.mark_exited(status, exited_at) {
            return Some(ExitTarget::Init);
        }
        for (exec_id, exec) in self.execs.iter_mut() {
            if exec.pid == Some(pid) && exec.mark_exited(status, exited_at) {
                return Some(ExitTarget::Exec(exec_id.clone()));
            }
        }
        None
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

    fn task_with_execs() -> TaskEntry {
        let mut task = entry();
        task.mark_started(10);
        assert!(task.add_exec(ExecEntry::new("e1", None, None, None, false, Some(11),)));
        task
    }

    #[test]
    fn exec_ids_cannot_collide() {
        let mut task = task_with_execs();
        assert!(!task.add_exec(ExecEntry::new("e1", None, None, None, false, Some(12),)));
        assert_eq!(task.execs.len(), 1);
        assert!(task.add_exec(ExecEntry::new("e2", None, None, None, false, Some(13),)));
    }

    #[test]
    fn exec_lifecycle_transitions() {
        let mut task = task_with_execs();
        let exec = task.execs.get_mut("e1").expect("exec exists");
        assert!(exec.mark_started(11));
        assert!(!exec.mark_started(99));
        let at = SystemTime::now();
        assert!(exec.mark_exited(4, at));
        assert!(!exec.mark_exited(4, at));
        assert_eq!(exec.exit.map(|e| e.status), Some(4));
    }

    #[test]
    fn exits_are_attributed_by_pid() {
        let mut task = task_with_execs();
        task.execs.get_mut("e1").expect("exec").mark_started(11);
        let at = SystemTime::now();

        assert_eq!(
            task.record_exit_by_pid(11, 5, at),
            Some(ExitTarget::Exec("e1".to_string()))
        );
        // Repeated notification for the same pid is a no-op.
        assert_eq!(task.record_exit_by_pid(11, 5, at), None);
        assert_eq!(task.record_exit_by_pid(10, 0, at), Some(ExitTarget::Init));
        // Unknown pids are ignored.
        assert_eq!(task.record_exit_by_pid(404, 0, at), None);
    }

    #[test]
    fn init_pid_wins_when_pids_collide() {
        // Pids are unique per process, so this cannot happen in practice;
        // the mapping still stays deterministic.
        let mut task = task_with_execs();
        let init_pid = task.pid.unwrap_or(0);
        task.execs.get_mut("e1").expect("exec").pid = Some(init_pid);
        let at = SystemTime::now();
        assert_eq!(
            task.record_exit_by_pid(init_pid, 2, at),
            Some(ExitTarget::Init)
        );
    }
}
