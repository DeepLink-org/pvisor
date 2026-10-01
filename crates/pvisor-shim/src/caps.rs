//! Linux capability name -> bit mapping.
//!
//! The init child applies capability sets through `capset(2)`, which takes
//! u32 bitmask pairs; names arrive from the OCI spec (with or without the
//! `CAP_` prefix depending on the serializer).

use std::collections::HashMap;
use std::sync::LazyLock;

use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
#[error("unknown capability: {0}")]
pub struct UnknownCapability(pub String);

/// bit index of every capability known to Linux 6.x (0..=40).
const CAPABILITY_BITS: &[(&str, u32)] = &[
    ("CHOWN", 0),
    ("DAC_OVERRIDE", 1),
    ("DAC_READ_SEARCH", 2),
    ("FOWNER", 3),
    ("FSETID", 4),
    ("KILL", 5),
    ("SETGID", 6),
    ("SETUID", 7),
    ("SETPCAP", 8),
    ("LINUX_IMMUTABLE", 9),
    ("NET_BIND_SERVICE", 10),
    ("NET_BROADCAST", 11),
    ("NET_ADMIN", 12),
    ("NET_RAW", 13),
    ("IPC_LOCK", 14),
    ("IPC_OWNER", 15),
    ("SYS_MODULE", 16),
    ("SYS_RAWIO", 17),
    ("SYS_CHROOT", 18),
    ("SYS_PTRACE", 19),
    ("SYS_PACCT", 20),
    ("SYS_ADMIN", 21),
    ("SYS_BOOT", 22),
    ("SYS_NICE", 23),
    ("SYS_RESOURCE", 24),
    ("SYS_TIME", 25),
    ("SYS_TTY_CONFIG", 26),
    ("MKNOD", 27),
    ("LEASE", 28),
    ("AUDIT_WRITE", 29),
    ("AUDIT_CONTROL", 30),
    ("SETFCAP", 31),
    ("MAC_OVERRIDE", 32),
    ("MAC_ADMIN", 33),
    ("SYSLOG", 34),
    ("WAKE_ALARM", 35),
    ("BLOCK_SUSPEND", 36),
    ("AUDIT_READ", 37),
    ("PERFMON", 38),
    ("BPF", 39),
    ("CHECKPOINT_RESTORE", 40),
];

static CAPABILITY_INDEX: LazyLock<HashMap<&'static str, u32>> =
    LazyLock::new(|| CAPABILITY_BITS.iter().copied().collect::<HashMap<_, _>>());

/// Normalize one capability name to the bare, prefix-free form.
fn normalize(name: &str) -> &str {
    name.trim()
        .strip_prefix("CAP_")
        .unwrap_or_else(|| name.trim())
}

/// Convert capability names into a u64 bitmask (bit n = capability n).
pub fn mask_from_names<I>(names: I) -> Result<u64, UnknownCapability>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
{
    let mut mask: u64 = 0;
    for name in names {
        let name = name.as_ref();
        let key = normalize(name).to_ascii_uppercase();
        let bit = *CAPABILITY_INDEX
            .get(key.as_str())
            .ok_or_else(|| UnknownCapability(name.to_string()))?;
        mask |= 1 << bit;
    }
    Ok(mask)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_with_and_without_prefix_map_to_bits() {
        assert_eq!(mask_from_names(["CAP_CHOWN"]).unwrap(), 1 << 0);
        assert_eq!(mask_from_names(["chown"]).unwrap(), 1 << 0);
        assert_eq!(mask_from_names(["NET_ADMIN"]).unwrap(), 1 << 12);
        assert_eq!(mask_from_names(["CHECKPOINT_RESTORE"]).unwrap(), 1 << 40);
    }

    #[test]
    fn duplicate_names_collapse() {
        let mask = mask_from_names(["CAP_KILL", "KILL"]).unwrap();
        assert_eq!(mask, 1 << 5);
    }

    #[test]
    fn unknown_names_are_rejected_with_the_original_name() {
        let error = mask_from_names(["CAP_NOT_A_CAP"]).unwrap_err();
        assert_eq!(error.0, "CAP_NOT_A_CAP");
    }

    #[test]
    fn every_known_capability_round_trips() {
        for (name, bit) in CAPABILITY_BITS {
            let mask = mask_from_names([name]).unwrap();
            assert_eq!(mask, 1u64 << bit, "{name}");
        }
        assert_eq!(CAPABILITY_BITS.len(), 41);
    }
}
