# Testing and semaspec

pVisor separates two levels of correctness. Unit/integration tests verify code and can be automated. Semantic specifications express product promises and require human review.

## Common commands

| Command | Purpose |
| --- | --- |
| `mise run test` | All Rust tests through `cargo nextest` in debug mode, native script regressions and Python tests |
| `mise run test -- pvisor-core` | One Rust package; aliases include `pvisor`, `core` (`control`, `agentctl`), `capture` (Gateway) and `shim` |
| `mise run test-py -- -k NAME` | Python unittest checks; accepts -k/-v and file or test names |
| `mise run test-isolation` | Strict Linux rootless/FUSE regressions; unavailable user namespaces do not skip checks |
| `mise run smoke` | Build debug CLI and check main subcommands |
| `mise run examples -- [scenario]` | End-to-end examples under `examples/pvisor/` |
| `mise run cases -- --suite stage` | STAGE specifications in a fresh temporary workspace |
| `mise run cases` | Documentation specifications (S-DOC, sourced from [`reference/cases.md`](../reference/cases.md)) |
| `mise run semaspec -- lint docs/src/zh/cases` | Static specification format checks |
| `mise run test-semaspec` | Tests for the semaspec tool itself |

Private tasks in `tests/tasks.toml` check mise argument forwarding, Bash assertion vocabulary and the offline installer. Run them separately with `mise run test-scripts`; CI runs both `mise run test-scripts` and `mise run test-py`. Python tests cover Python contracts, structured data, PTY interaction and process lifecycle.

Use direct `cargo test` only for doctests or an explicitly documented special runner.

## Semantic specifications

Each Markdown case has an ID such as `S-STAGE-008`, a **semantic claim**, **violation example** and executable script. Current domains:

- **STAGE** (`docs/src/zh/cases/06-stage-apply.md`): staging/apply/drop promises; see [Staging and apply semantics](../concepts/staging.md);
- **DOC** (`docs/src/zh/reference/cases.md`): behavior of executable documentation examples.

Execution results are PASS, FAIL, SKIP, XFAIL, XPASS or ERROR. Review states are UNREVIEWED, STALE or REVIEWED.

## Human review rules

- PASS means the implementation satisfies the script, not that the semantic claim is correct;
- New/modified cases are UNREVIEWED or STALE; release gates do not treat them as approved;
- Only maintainers may manually run `semaspec approve`/`revoke` or edit `REVIEWED.toml`/`.approved/` snapshots;
- Nobody, including AI tools, may weaken existing claims, checks or `xfail` annotations just to obtain a pass.

See `tools/semaspec/DESIGN.md` for the complete design.
