//! Pure, bounded lowering of ordered device permission updates to eBPF.
use crate::plan::DeviceRulePlan;
use anyhow::{Result, bail, ensure};

const ALL_ACCESS: u32 = 7;
const MAX_INSTRUCTIONS: usize = 4096;

struct Rule {
    kind: Option<u32>,
    major: Option<u32>,
    minor: Option<u32>,
    access: u32,
    allow: bool,
}

impl Rule {
    fn normalize(input: &DeviceRulePlan) -> Result<Self> {
        let kind = match input.typ.as_deref() {
            None | Some("a") => None,
            Some("b") => Some(1),
            Some("c") => Some(2),
            _ => bail!("device type must be a, b, or c"),
        };
        let mut access = 0;
        for mode in input.access.chars() {
            let bit = match mode {
                'm' => 1,
                'r' => 2,
                'w' => 4,
                _ => bail!("unknown device access mode"),
            };
            ensure!(access & bit == 0, "duplicate device access mode");
            access |= bit;
        }
        ensure!(access != 0, "device access must not be empty");
        fn number(value: Option<i64>) -> Result<Option<u32>> {
            match value {
                None | Some(-1) => Ok(None),
                Some(n) => Ok(Some(u32::try_from(n).map_err(|_| {
                    anyhow::anyhow!("device number must be -1 or fit u32")
                })?)),
            }
        }
        let major = number(input.major)?;
        let minor = number(input.minor)?;
        // Type a is a complete policy reset in the device-controller ABI.
        // Reject ambiguous subsets instead of silently broadening them.
        ensure!(
            kind.is_some() || (major.is_none() && minor.is_none() && access == ALL_ACCESS),
            "all-device rules require wildcard numbers and rwm access"
        );
        Ok(Self {
            kind,
            major,
            minor,
            access,
            allow: input.allow,
        })
    }
}

// Linux bpf_insn: opcode, packed registers, native-endian offset and immediate.
fn instruction(code: u8, dst: u8, src: u8, offset: i16, imm: u32) -> [u8; 8] {
    let mut bytes = [0; 8];
    bytes[0] = code;
    bytes[1] = if cfg!(target_endian = "little") {
        (src << 4) | dst
    } else {
        (dst << 4) | src
    };
    bytes[2..4].copy_from_slice(&offset.to_ne_bytes());
    bytes[4..8].copy_from_slice(&imm.to_ne_bytes());
    bytes
}

/// Normalize selectors/access masks, apply every matching permission update,
/// then allow only if the final mask covers the entire requested access.
/// No policy decision returns early from an individual rule.
pub(crate) fn build_device_program(inputs: &[DeviceRulePlan]) -> Result<Vec<u8>> {
    ensure!(
        inputs.len() <= MAX_INSTRUCTIONS - 16,
        "device policy exceeds instruction limit"
    );
    let rules = inputs
        .iter()
        .map(Rule::normalize)
        .collect::<Result<Vec<_>>>()?;
    // R1 remains the kernel context. R0 accumulates permissions; R2 is scratch;
    // R3 retains requested access. Reject unknown device types and empty access.
    let mut program = vec![
        instruction(0xb4, 0, 0, 0, 0),      // MOV32 R0, 0
        instruction(0x61, 2, 1, 0, 0),      // LDX W R2, [R1+0]
        instruction(0x54, 2, 0, 0, 0xffff), // AND32 R2, type mask
        instruction(0x16, 2, 0, 2, 1),      // JEQ32 block -> access extraction
        instruction(0x16, 2, 0, 1, 2),      // JEQ32 char -> access extraction
        instruction(0x95, 0, 0, 0, 0),      // EXIT deny
        instruction(0x61, 3, 1, 0, 0),
        instruction(0x74, 3, 0, 0, 16), // RSH32 R3, 16
        instruction(0x56, 3, 0, 1, 0),  // JNE32 R3, 0 -> updates
        instruction(0x95, 0, 0, 0, 0),
    ];
    for rule in rules {
        let mut mismatches = Vec::with_capacity(3);
        if let Some(kind) = rule.kind {
            program.push(instruction(0x61, 2, 1, 0, 0));
            program.push(instruction(0x54, 2, 0, 0, 0xffff));
            mismatches.push(program.len());
            program.push(instruction(0x56, 2, 0, 0, kind));
        }
        for (offset, number) in [(4, rule.major), (8, rule.minor)] {
            if let Some(number) = number {
                program.push(instruction(0x61, 2, 1, offset, 0));
                mismatches.push(program.len());
                program.push(instruction(0x56, 2, 0, 0, number));
            }
        }
        program.push(if rule.allow {
            instruction(0x44, 0, 0, 0, rule.access) // OR32: add permissions
        } else {
            instruction(0x54, 0, 0, 0, !rule.access) // AND32: remove permissions
        });
        let next = program.len();
        for index in mismatches {
            // A rule has at most eight selector instructions and one update.
            let offset = i16::try_from(next - index - 1)?;
            program[index][2..4].copy_from_slice(&offset.to_ne_bytes());
        }
        ensure!(
            program.len() + 6 <= MAX_INSTRUCTIONS,
            "device policy exceeds instruction limit"
        );
    }
    program.extend([
        instruction(0x5c, 0, 3, 0, 0), // AND32 R0, R3
        instruction(0x5e, 0, 3, 2, 0), // JNE32 R0, R3 -> deny
        instruction(0xb4, 0, 0, 0, 1),
        instruction(0x95, 0, 0, 0, 0),
        instruction(0xb4, 0, 0, 0, 0),
        instruction(0x95, 0, 0, 0, 0),
    ]);
    Ok(program.into_iter().flatten().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::DeviceRulePlan;

    fn rule(
        allow: bool,
        typ: Option<&str>,
        major: Option<i64>,
        minor: Option<i64>,
        access: &str,
    ) -> DeviceRulePlan {
        DeviceRulePlan {
            allow,
            typ: typ.map(str::to_owned),
            major,
            minor,
            access: access.into(),
        }
    }

    // Interpret the emitted instructions, not an alternate implementation of
    // the compiler. The policy oracle below operates on concrete permissions.
    fn execute(bytes: &[u8], typ: u32, major: u32, minor: u32, access: u32) -> bool {
        let context = [(access << 16) | typ, major, minor];
        let mut registers = [0_u64; 11];
        let mut pc = 0_i64;
        for _ in 0..bytes.len() / 8 + 1 {
            let instruction = &bytes[pc as usize * 8..pc as usize * 8 + 8];
            let (dst, src) = if cfg!(target_endian = "little") {
                (instruction[1] & 15, instruction[1] >> 4)
            } else {
                (instruction[1] >> 4, instruction[1] & 15)
            };
            let (dst, src) = (dst as usize, src as usize);
            let offset = i16::from_ne_bytes(instruction[2..4].try_into().unwrap());
            let imm = u32::from_ne_bytes(instruction[4..8].try_into().unwrap());
            match instruction[0] {
                0x61 => {
                    assert_eq!(src, 1);
                    registers[dst] = context[offset as usize / 4] as u64;
                }
                0x44 => registers[dst] = ((registers[dst] as u32) | imm) as u64,
                0x54 => registers[dst] = ((registers[dst] as u32) & imm) as u64,
                0x5c => registers[dst] = ((registers[dst] as u32) & registers[src] as u32) as u64,
                0x74 => registers[dst] = ((registers[dst] as u32) >> imm) as u64,
                0xb4 => registers[dst] = imm as u64,
                0x55 => {
                    if registers[dst] != imm as i32 as i64 as u64 {
                        pc += offset as i64;
                    }
                }
                0x56 => {
                    if registers[dst] as u32 != imm {
                        pc += offset as i64;
                    }
                }
                0x16 => {
                    if registers[dst] as u32 == imm {
                        pc += offset as i64;
                    }
                }
                0x5e => {
                    if registers[dst] as u32 != registers[src] as u32 {
                        pc += offset as i64;
                    }
                }
                0x95 => {
                    assert!(registers[0] <= 1);
                    return registers[0] == 1;
                }
                code => panic!("unsupported instruction {code:#x}"),
            }
            pc += 1;
            assert!(pc >= 0 && (pc as usize) < bytes.len() / 8);
        }
        panic!("device program did not terminate")
    }

    fn allows(rules: &[DeviceRulePlan], typ: u32, major: u32, minor: u32, access: u32) -> bool {
        execute(
            &build_device_program(rules).unwrap(),
            typ,
            major,
            minor,
            access,
        )
    }

    #[test]
    fn later_deny_removes_only_its_permissions_including_combined_access() {
        let rules = [
            rule(true, Some("c"), Some(240), Some(0), "rw"),
            rule(false, Some("c"), Some(240), Some(0), "w"),
        ];
        assert!(allows(&rules, 2, 240, 0, 2));
        assert!(!allows(&rules, 2, 240, 0, 4));
        assert!(!allows(&rules, 2, 240, 0, 6));
    }

    #[test]
    fn grant_after_deny_all_and_separate_permission_updates_take_effect() {
        let rules = [
            rule(false, None, None, None, "rwm"),
            rule(true, Some("c"), Some(240), Some(0), "r"),
            rule(true, Some("c"), Some(240), Some(0), "w"),
        ];
        assert!(allows(&rules, 2, 240, 0, 6));
        assert!(!allows(&rules, 2, 240, 0, 7));
        assert!(!allows(&rules, 1, 240, 0, 2));
        assert!(!allows(&rules, 2, 240, 1, 2));
    }

    #[test]
    fn generated_program_matches_a_concrete_permission_table() {
        let updates = [
            rule(true, None, None, None, "rwm"),
            rule(false, Some("c"), Some(240), None, "w"),
            rule(true, Some("c"), Some(240), Some(1), "w"),
            rule(false, Some("b"), None, Some(0), "rm"),
            rule(true, Some("b"), Some(8), Some(0), "r"),
        ];
        // Independent finite oracle: update each concrete device's three
        // permission booleans, and check every requested subset after each prefix.
        for typ in [1, 2] {
            for major in [0, 8, 240, 241] {
                for minor in [0, 1, 2] {
                    let mut permissions = [false; 3];
                    for prefix in 0..=updates.len() {
                        for access in 1..8 {
                            let expected =
                                (0..3).all(|bit| access & (1 << bit) == 0 || permissions[bit]);
                            assert_eq!(
                                allows(&updates[..prefix], typ, major, minor, access),
                                expected,
                                "prefix={prefix}, device={typ}:{major}:{minor}, access={access}"
                            );
                        }
                        if let Some(update) = updates.get(prefix) {
                            let matches = update
                                .typ
                                .as_deref()
                                .is_none_or(|kind| kind == if typ == 1 { "b" } else { "c" })
                                && update.major.is_none_or(|n| n == major as i64)
                                && update.minor.is_none_or(|n| n == minor as i64);
                            if matches {
                                for (bit, mode) in ['m', 'r', 'w'].into_iter().enumerate() {
                                    if update.access.contains(mode) {
                                        permissions[bit] = update.allow;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn runtime_defaults_survive_deny_all_without_granting_other_devices() {
        let rules = crate::plan::with_default_devices(&[
            rule(false, None, None, None, "rwm"),
            rule(true, Some("b"), Some(8), Some(0), "r"),
            rule(false, Some("c"), Some(1), Some(3), "rwm"),
        ]);
        assert!(
            allows(&rules, 2, 1, 3, 7),
            "standard runtime grant is appended"
        );
        assert!(allows(&rules, 1, 8, 0, 2), "custom grant after deny-all");
        assert!(!allows(&rules, 1, 8, 0, 4));
        assert!(!allows(&rules, 2, 240, 0, 2));
    }

    #[test]
    fn numbers_preserve_u32_bits_and_minus_one_is_wildcard() {
        let rules = [rule(true, Some("c"), Some(u32::MAX as i64), Some(-1), "rw")];
        assert!(allows(&rules, 2, u32::MAX, 0, 6));
        assert!(allows(&rules, 2, u32::MAX, u32::MAX, 6));
        assert!(!allows(&rules, 2, u32::MAX - 1, 0, 6));
        assert!(!allows(&rules, 1, u32::MAX, 0, 6));
    }

    #[test]
    fn invalid_rules_and_oversized_programs_fail_before_loading() {
        for invalid in [
            rule(true, Some("x"), None, None, "r"),
            rule(true, Some("c"), Some(-2), None, "r"),
            rule(true, Some("c"), None, Some(u32::MAX as i64 + 1), "r"),
            rule(true, Some("c"), None, None, ""),
            rule(true, Some("c"), None, None, "rr"),
            rule(true, Some("c"), None, None, "x"),
            rule(true, None, None, None, "r"),
            rule(true, Some("a"), Some(1), None, "rwm"),
        ] {
            assert!(build_device_program(&[invalid]).is_err());
        }
        let excessive = vec![rule(true, Some("c"), Some(240), Some(0), "r"); MAX_INSTRUCTIONS];
        assert!(build_device_program(&excessive).is_err());
        // Exercise the emitted-instruction bound as well as the input bound:
        // 16 fixed instructions plus eight for each fully specified selector.
        let boundary = vec![rule(true, Some("c"), Some(240), Some(0), "r"); 510];
        assert_eq!(build_device_program(&boundary).unwrap().len(), 4096 * 8);
        let mut over_boundary = boundary;
        over_boundary.push(rule(false, Some("c"), Some(240), Some(0), "w"));
        assert!(build_device_program(&over_boundary).is_err());
        let allowed = [rule(true, None, None, None, "rwm")];
        assert!(!allows(&allowed, 0, 0, 0, 2));
        assert!(!allows(&allowed, 2, 0, 0, 0));
        assert!(!allows(&allowed, 2, 0, 0, 8));
    }
}
