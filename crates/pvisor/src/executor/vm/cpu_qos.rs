//! Native Linux VM CPU classes. A supervisor shares one LS cookie; parent policy
//! stays unchanged and BE children retain their inherited, distinct group.
use anyhow::{Context, ensure};
use pvisor_core::{CpuQosClass, CpuQosObservation};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    os::{fd::AsRawFd, unix::process::CommandExt},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, Weak},
};

const ANCHOR_ENV: &str = "PVISOR_NATIVE_CPU_QOS_ANCHOR";
const CORE: libc::c_int = 62;
const GET: libc::c_ulong = 0;
const CREATE: libc::c_ulong = 1;
const SHARE_FROM: libc::c_ulong = 3;
const THREAD: libc::c_ulong = 0;
const THREAD_GROUP: libc::c_ulong = 1;

pub(super) fn cookie(pid: u32) -> std::io::Result<u64> {
    let mut cookie = 0_u64;
    if unsafe {
        libc::prctl(
            CORE,
            GET,
            libc::c_ulong::from(pid),
            THREAD,
            &mut cookie as *mut u64,
        )
    } != 0
    {
        return Err(std::io::Error::last_os_error());
    }
    Ok(cookie)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct GroupBinding {
    pid: u32,
    cookie: u64,
}

/// One sleeping, credential-free owner pins the LS group across Attempts.
/// Each VMM inherits its selected policy/cookie before creating any threads.
#[derive(Debug)]
pub struct CpuQosGroup {
    anchor: Child,
    binding: GroupBinding,
}

impl CpuQosGroup {
    /// Start an LS cookie owner using an executable that calls
    /// `run_krun_internal_if_requested` before creating threads or parsing CLI arguments.
    pub fn with_launcher(launcher: &std::path::Path) -> anyhow::Result<Arc<Self>> {
        Self::start(launcher).map(Arc::new)
    }

    pub fn shared() -> anyhow::Result<Arc<Self>> {
        static GROUP: Mutex<Weak<CpuQosGroup>> = Mutex::new(Weak::new());
        let mut shared = GROUP
            .lock()
            .map_err(|_| anyhow::anyhow!("CPU QoS group lock poisoned"))?;
        if let Some(group) = shared.upgrade() {
            return Ok(group);
        }
        let group = Self::with_launcher(&std::env::current_exe()?)?;
        *shared = Arc::downgrade(&group);
        Ok(group)
    }

    fn start(launcher: &std::path::Path) -> anyhow::Result<Self> {
        let original = cookie(0).context("Linux core scheduling is unavailable")?;
        let mut command = Command::new(launcher);
        command
            .env_clear()
            .env(ANCHOR_ENV, "1")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        // Only a syscall in the fork child. Never alter the multi-threaded
        // supervisor or take a Rust lock/allocate from the pre-exec callback.
        unsafe {
            command.pre_exec(|| {
                if libc::prctl(
                    CORE,
                    CREATE,
                    0 as libc::c_ulong,
                    THREAD_GROUP,
                    0 as libc::c_ulong,
                ) != 0
                {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        let anchor = command.spawn().context("create CPU QoS anchor")?;
        let mut group = Self {
            binding: GroupBinding {
                pid: anchor.id(),
                cookie: 0,
            },
            anchor,
        };
        let mut output = group
            .anchor
            .stdout
            .take()
            .context("CPU QoS anchor readiness pipe missing")?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut descriptor = libc::pollfd {
            fd: output.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            ensure!(!remaining.is_zero(), "CPU QoS anchor readiness timed out");
            let status =
                unsafe { libc::poll(&mut descriptor, 1, remaining.as_millis().max(1) as i32) };
            if status < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    continue;
                }
                return Err(error.into());
            }
            ensure!(status != 0, "CPU QoS anchor readiness timed out");
            break;
        }
        let mut ready = [0_u8];
        output
            .read_exact(&mut ready)
            .context("CPU QoS launcher did not enter the native anchor")?;
        ensure!(ready == [1], "invalid CPU QoS anchor readiness");
        group.binding.cookie = cookie(group.binding.pid)?;
        ensure!(
            group.binding.cookie != 0 && group.binding.cookie != original,
            "CPU QoS anchor did not create a distinct LS group"
        );
        Ok(group)
    }
    pub(super) fn binding(&self) -> GroupBinding {
        self.binding.clone()
    }
}

impl Drop for CpuQosGroup {
    fn drop(&mut self) {
        // This helper only waits on a pipe, with no namespaces or backing FDs.
        // Reap it synchronously; no detached lifetime escapes the final owner.
        let _ = self.anchor.kill();
        let _ = self.anchor.wait();
    }
}

pub(super) fn run_anchor_if_requested() -> anyhow::Result<bool> {
    // Rootless launchers receive Task environment variables but carry an
    // internal argv. VM launchers carry a private RunnerSpec. Neither may be
    // diverted into this helper by a coincidentally named environment field.
    if std::env::var(ANCHOR_ENV).as_deref() != Ok("1")
        || std::env::args_os().nth(1).is_some()
        || std::env::var_os(super::supported::RUNNER_SPEC_ENV).is_some()
    {
        return Ok(false);
    }
    // Internal helper reentry runs before any runtime threads exist.
    unsafe {
        std::env::remove_var(ANCHOR_ENV);
    }
    std::io::stdout().write_all(&[1])?;
    std::io::stdout().flush()?;
    let mut byte = [0_u8];
    loop {
        match std::io::stdin().read(&mut byte) {
            Ok(0) => break,
            Ok(_) => anyhow::bail!("unexpected CPU QoS anchor input"),
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

pub(super) fn apply(
    class: CpuQosClass,
    group: Option<&GroupBinding>,
) -> anyhow::Result<CpuQosObservation> {
    let policy = match class {
        CpuQosClass::BestEffort => {
            ensure!(group.is_none(), "BE cannot enter the LS core group");
            libc::SCHED_IDLE
        }
        CpuQosClass::LatencySensitive => {
            let group = group.context("LS requires a pinned core scheduling group")?;
            ensure!(
                group.cookie != 0 && cookie(group.pid)? == group.cookie,
                "CPU QoS anchor identity changed"
            );
            ensure!(
                unsafe {
                    libc::prctl(
                        CORE,
                        SHARE_FROM,
                        libc::c_ulong::from(group.pid),
                        THREAD,
                        0 as libc::c_ulong,
                    )
                } == 0,
                "join LS core scheduling group: {}",
                std::io::Error::last_os_error()
            );
            ensure!(cookie(0)? == group.cookie, "LS cookie was not installed");
            libc::SCHED_OTHER
        }
    };
    // musl exposes additional sporadic-scheduler fields. Both policies require
    // a zero priority; zero all ABI fields rather than assuming the GNU layout.
    let params: libc::sched_param = unsafe { std::mem::zeroed() };
    ensure!(
        unsafe { libc::sched_setscheduler(0, policy, &params) } == 0,
        "install CPU QoS policy: {}",
        std::io::Error::last_os_error()
    );
    ensure!(
        unsafe { libc::sched_getscheduler(0) } == policy,
        "CPU QoS policy mismatch"
    );
    Ok(CpuQosObservation {
        class,
        scheduler_policy: policy,
        core_cookie: Some(cookie(0)?),
    })
}

pub(super) fn write_attestation(
    file: &mut std::fs::File,
    qos: Option<&CpuQosObservation>,
) -> anyhow::Result<()> {
    if let Some(qos) = qos {
        file.write_all(&serde_json::to_vec(qos)?)?;
    } else {
        file.write_all(b"pvisor-vmm-installed-v1\n")?;
    }
    Ok(())
}

/// Read kernel state again immediately before native entry.
pub(super) fn observe(class: CpuQosClass) -> anyhow::Result<CpuQosObservation> {
    let observation = CpuQosObservation {
        class,
        scheduler_policy: unsafe { libc::sched_getscheduler(0) },
        core_cookie: Some(cookie(0)?),
    };
    ensure!(
        valid(&observation, class, None),
        "CPU QoS changed before native entry"
    );
    Ok(observation)
}

pub(super) fn valid(
    observation: &CpuQosObservation,
    class: CpuQosClass,
    group: Option<&GroupBinding>,
) -> bool {
    observation.class == class
        && match class {
            CpuQosClass::BestEffort => {
                observation.scheduler_policy == libc::SCHED_IDLE
                    && observation.core_cookie.is_some()
            }
            CpuQosClass::LatencySensitive => {
                observation.scheduler_policy == libc::SCHED_OTHER
                    && observation.core_cookie.is_some_and(|cookie| {
                        cookie != 0 && group.is_none_or(|group| group.cookie == cookie)
                    })
            }
        }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipts_require_actual_policy_class_and_bound_ls_cookie() {
        let group = GroupBinding {
            pid: 1,
            cookie: 123,
        };
        for class in [CpuQosClass::BestEffort, CpuQosClass::LatencySensitive] {
            let expected = if class == CpuQosClass::BestEffort {
                libc::SCHED_IDLE
            } else {
                libc::SCHED_OTHER
            };
            let mut observation = CpuQosObservation {
                class,
                scheduler_policy: expected,
                core_cookie: Some(123),
            };
            assert!(valid(&observation, class, Some(&group)));
            observation.scheduler_policy = libc::SCHED_FIFO;
            assert!(!valid(&observation, class, Some(&group)));
            observation.scheduler_policy = expected;
            observation.core_cookie = None;
            assert!(!valid(&observation, class, Some(&group)));
            observation.core_cookie = Some(456);
            assert_eq!(
                valid(&observation, class, Some(&group)),
                class == CpuQosClass::BestEffort
            );
            observation.core_cookie = Some(0);
            assert_eq!(
                valid(&observation, class, Some(&group)),
                class == CpuQosClass::BestEffort
            );
        }
        let observation = CpuQosObservation {
            class: CpuQosClass::LatencySensitive,
            scheduler_policy: libc::SCHED_OTHER,
            core_cookie: Some(123),
        };
        assert!(!valid(&observation, CpuQosClass::BestEffort, None));
        assert!(serde_json::from_slice::<CpuQosObservation>(b"pvisor-vmm-installed-v1\n").is_err());
        assert!(
            serde_json::from_value::<CpuQosObservation>(serde_json::json!({
                "class":"latency_sensitive", "scheduler_policy":0, "core_cookie":123, "extra":true
            }))
            .is_err()
        );
    }
}
