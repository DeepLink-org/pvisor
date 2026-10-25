# Contributing to PolicyVisor

Thanks for helping. This page is the short version; the full guide is
[community/contributing](https://deeplink-org.github.io/pvisor/en/community/contributing/).

## Before you start

- Bugs and feature requests: open a GitHub issue. Security problems follow
  [SECURITY.md](SECURITY.md) instead.
- Larger changes: open an issue first so the design can be discussed before code.
- Documentation fixes should update both languages. Engineering follow-ups and
  acceptance requirements are collected in `docs/README.md`.

## Development loop

Install [mise](https://mise.jdx.dev/installing-mise.html) 2026.8.5 or newer, then run:

```bash
mise trust
mise install --locked
mise run setup
```

Tool versions live in `mise.toml`; Python development dependencies use `uv.lock`
and `.venv`. Platform libraries and virtualization permissions are documented in
the development guide. Task arguments follow `--`.
The minimum mise version is a tested compatibility floor. CI and wheel builds
continue to use mise 2026.10.18 and the committed tool versions and lockfiles.

`mise run setup -- python` syncs only Python development dependencies, without
installing guest Rust targets. CI's shared setup action accepts a `tools` subset
and a `setup` scope (`none`, `python`, or `full`); tool versions stay in `mise.toml`.
CI disables automatic tool installation after provisioning so nested tasks keep
the requested scope. Full local setup and build jobs retain the complete tools.

```bash
mise run build            # debug build
mise run test             # Rust tests via cargo nextest, then Python tests
mise run test -- pvisor-core # limit Rust tests to one package
mise run ci -- "fmt-rust --check" "fmt-py --check"        # formatting
mise run lint             # clippy and ruff
mise run docs-build       # build and check the documentation site
```

See [development](https://deeplink-org.github.io/pvisor/en/community/development/)
for platform setup (FUSE/macFUSE, KVM/HVF, OCI runtimes).

### Local tool overrides

Put compatible personal tool selections in `mise.local.toml`, which is ignored
along with `mise.local.lock`. To use Python, uv and jq already available on your
`PATH`, create:

```toml
# mise.local.toml
[settings]
disable_tools = ["python", "uv", "jq"]
```

Only disable tools you provide yourself. Python development tasks require
Python 3.12 or newer; when Python is disabled, `UV_PYTHON` falls back to `python3`
on `PATH`. Other project tools retain their default selections.

To select a different mise-managed version, use a local tool declaration:

```toml
# mise.local.toml
[tools]
python = "3.12.11"
```

Remove that tool from `disable_tools` if present. Check the effective selections
with `mise ls --current` and run the checks relevant to your change. Rust,
rustfmt, Clippy and Ruff versions can affect diagnostics and formatting; the
committed defaults remain the CI reference. Keep personal changes out of the
committed tool and dependency lockfiles. Release builds and recorded benchmark
comparisons use a clean checkout without personal overrides or disabled tools.
Use `mise install --locked` with the committed lockfile; regenerate it with
CI's mise 2026.10.18 when intentionally updating shared tool versions.

## Semantic specifications

Product promises are written as semantic specifications (semaspec) under
`docs/src/zh/cases/` and `docs/src/zh/reference/cases.md`. A passing case is not
an approval:

- you may draft new cases and fix the implementation;
- you must not weaken existing claims, checks, or `xfail` annotations to get a pass;
- approving a case (`semaspec approve`) and editing `REVIEWED.toml` or `.approved/`
  snapshots are human-only maintainer actions.

Run `mise run cases -- --suite stage` for the STAGE domain and `mise run semaspec -- lint docs/src/zh/cases` for the
specifications. Details: [testing](https://deeplink-org.github.io/pvisor/en/community/testing/).

## Pull requests

- Keep each PR focused; describe the behavior change and how you verified it.
- Run `mise run ci -- "fmt-rust --check" "fmt-py --check"`, `mise run lint`, and the relevant `mise run test` targets.
- Update the documentation that owns the behavior you changed, and keep
  `docs/README.md` conventions (one canonical page per subject).

By contributing you agree that your contributions are licensed under the
[Apache License 2.0](LICENSE).
