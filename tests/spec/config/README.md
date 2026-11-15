# config feature specifications

Native Scrut specifications own the public promises and checks for this feature.
Each ID is independent; related conventional regressions belong in `regression/`.
Shared resource and review rules are in the [specification README](../README.md).

```sh
mise run cases -- --feature config --list
mise run cases -- --feature config
```

- [S-DOC-043](S-DOC-043.md)
- [S-DOC-044](S-DOC-044.md)
- [S-DOC-045](S-DOC-045.md)
- [S-CONFIG-001](S-CONFIG-001.md): new parse-failure contract; invalid input must
  not execute a fallback command, overwrite an existing result or publish a
  Session. Requires only the real CLI and Bash, not FUSE or a VM.
