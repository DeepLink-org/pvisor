# pVisor case catalog

The case catalog is the executable index for `pvisor run`. It starts with a
minimal host Run and progresses through staged changes, provider selection,
network policy, Gateway capture, and prepared RunSpecs.

Use the catalog when you need a reproducible command or a regression target.
Each case documents its prerequisites, command, expected result, and machine
check. The [Chinese catalog](../../zh/reference/cases.md) contains both the
documentation and executable checks; semspec reads it directly. The A01–M02
document labels remain in each semantic case title.

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

From the repository root:

```bash
just semspec list --domain DOC
just semspec run docs/src/zh/reference/cases.md --subject-bin target/release/pvisor
just cases --case S-DOC-001,S-DOC-012 --keep
just cases
just semspec show S-DOC-001
```

`just cases` builds release pVisor, runs the 54 active DOC specifications and writes
`target/pvisor-case-report.json`. Select by S-DOC IDs: A01 is S-DOC-001, C01 is
S-DOC-012. The complete mapping lives in `tests/semantics/README.md`. Use
`just semspec run --domain DOC --subject-bin PATH` to test an existing binary.
L01 and L02 were retired with the removed `env` feature; S-DOC-053 and S-DOC-054
remain reserved in `semspec.toml`.

Each specification preserves the command, expected exit status and original
assertions. Expected nonzero exits remain explicit checks, never xfail. Sealed
`cases.sh` provides isolated fixtures and Run Bundle/record/output assertions.
Missing prerequisites produce SKIP; failures retain their temporary directories.
Use `--keep` to retain every executed case. Rootfs, image, runtime and Agent
settings still use the `PVISOR_CASE_*` resource variables described in the
[Chinese catalog](../../zh/reference/cases.md).

The migrated specifications are UNREVIEWED. Passing checks do not constitute
human approval. Enable `--require-reviewed` only after human review of cases,
vocabulary and engine. Use `just semspec run --help` for supported options.
