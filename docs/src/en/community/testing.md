# Testing and semspec

pVisor separates two levels of correctness. Unit/integration tests verify code and can be automated. Semantic specifications express product promises and require human review.

## Common commands

| Command | Purpose |
| --- | --- |
| `just test` | All Rust tests through `cargo nextest` in debug mode, then Python tests |
| `just test pvisor-core` | One Rust package; aliases include `pvisor`, `core` (`control`, `agentctl`), `capture` (Gateway) and `shim` |
| `just test-py -k NAME` | Python tests with optional pytest arguments |
| `just test-isolation` | Strict Linux rootless/FUSE regressions; unavailable user namespaces do not skip checks |
| `just smoke` | Build debug CLI and check main subcommands |
| `just examples [scenario]` | End-to-end examples under `examples/pvisor/` |
| `just semantics` | STAGE specifications in a fresh temporary workspace |
| `just cases` | Documentation specifications (S-DOC, sourced from [`reference/cases.md`](../reference/cases.md)) |
| `just semspec lint` | Static specification format checks |
| `just test-semspec` | Tests for the semspec tool itself |

Use direct `cargo test` only for doctests or an explicitly documented special runner.

## Semantic specifications

Each Markdown case has an ID such as `S-STAGE-008`, a **semantic claim**, **violation example** and executable script. Current domains:

- **STAGE** (`tests/semantics/stage-apply.md`): staging/apply/drop promises; see [Staging and apply semantics](../concepts/staging.md);
- **DOC** (`docs/src/zh/reference/cases.md`): behavior of executable documentation examples.

Execution results are PASS, FAIL, SKIP, XFAIL, XPASS or ERROR. Review states are UNREVIEWED, STALE or REVIEWED.

## Human review rules

- PASS means the implementation satisfies the script, not that the semantic claim is correct;
- New/modified cases are UNREVIEWED or STALE; release gates do not treat them as approved;
- Only maintainers may manually run `semspec approve`/`revoke` or edit `REVIEWED.toml`/`.approved/` snapshots;
- Nobody, including AI tools, may weaken existing claims, checks or `xfail` annotations just to obtain a pass.

See `tools/semspec/DESIGN.md` for the complete design.
