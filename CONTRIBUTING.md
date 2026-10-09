# Contributing to PolicyVisor

Thanks for helping. This page is the short version; the full guide is
[community/contributing](https://deeplink-org.github.io/pvisor/zh/community/contributing/).

## Before you start

- Bugs and feature requests: open a GitHub issue. Security problems follow
  [SECURITY.md](SECURITY.md) instead.
- Larger changes: open an issue first so the design can be discussed before code.
- Documentation fixes should update both languages. Engineering follow-ups and
  acceptance requirements are collected in `docs/README.md`.

## Development loop

```bash
just build            # debug build
just test             # Rust tests via cargo nextest, then Python tests
just test pvisor-core # limit Rust tests to one package
just ci "fmt-rust --check" "fmt-py --check"        # formatting
just lint             # clippy and ruff
just docs-build       # build and check the documentation site
```

See [development](https://deeplink-org.github.io/pvisor/zh/community/development/)
for platform setup (FUSE/macFUSE, KVM/HVF, OCI runtimes).

## Semantic specifications

Product promises are written as semantic specifications (semspec) under
`docs/src/zh/cases/` and `docs/src/zh/reference/cases.md`. A passing case is not
an approval:

- you may draft new cases and fix the implementation;
- you must not weaken existing claims, checks, or `xfail` annotations to get a pass;
- approving a case (`semspec approve`) and editing `REVIEWED.toml` or `.approved/`
  snapshots are human-only maintainer actions.

Run `just cases --suite stage` for the STAGE domain and `just semspec lint docs/src/zh/cases` for the
specifications. Details: [testing](https://deeplink-org.github.io/pvisor/zh/community/testing/).

## Pull requests

- Keep each PR focused; describe the behavior change and how you verified it.
- Run `just ci "fmt-rust --check" "fmt-py --check"`, `just lint`, and the relevant `just test` targets.
- Update the documentation that owns the behavior you changed, and keep
  `docs/README.md` conventions (one canonical page per subject).

By contributing you agree that your contributions are licensed under the
[Apache License 2.0](LICENSE).
