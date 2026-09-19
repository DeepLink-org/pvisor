# pVisor case catalog

The case catalog is the executable index for `pvisor run`. It starts with a
minimal host Run and progresses through staged changes, provider selection,
network policy, Gateway capture, and prepared RunSpecs.

Use the catalog when you need a reproducible command or a regression target.
Each case documents its prerequisites, command, expected result, and machine
check. The checks are folded into the page so the same examples can be read by
people and executed by `scripts/run-pvisor-cases.py`.

## Choose a case group

| Goal | Cases |
| --- | --- |
| First command and Run identity | A01–A03 |
| Limits and runtime controls | A04, B01–B04 |
| Review, apply, and drop changes | C01–C05 |
| Host and VM execution | D01–E06 |
| Native OCI containers | F01–F04 |
| Network and Gateway behavior | G01–H02 |
| RunConfig and RunSpec inputs | I01–I03 |
| Complete combinations | J01–J03 |

## Run the catalog

From the repository root, list the available cases:

```bash
python3 scripts/run-pvisor-cases.py --list
```

Run selected cases or produce a report:

```bash
python3 scripts/run-pvisor-cases.py \
  --pvisor target/release/pvisor \
  --case A01,C01 \
  --keep
python3 scripts/run-pvisor-cases.py \
  --pvisor target/release/pvisor \
  --report target/pvisor-case-report.md
```

The runner creates an isolated workspace per case. Replace placeholder paths
and images with local resources when a case requires a Linux rootfs, OCI runtime,
VM image, or Agent executable. Missing prerequisites are reported as skips. Use `--strict-skips` to make them
fail the run in CI.

The complete command-by-command catalog is maintained in the [Chinese catalog](../../zh/reference/cases.md); the executable assertions below each example are language-neutral.
