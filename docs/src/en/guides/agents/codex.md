# Codex CLI

```bash
pvisor run --safe -- codex
pvisor status --review last
pvisor apply last --path src   # 或：pvisor drop last
```

## What `--safe` does for Codex

- **Network:** the preset matches the `codex` executable and allows only port 443 on `api.openai.com`, `chatgpt.com`, and `ab.chatgpt.com`.
- **Workspace:** changes go to the stage and are applied after review.
- **HOME and `CODEX_HOME`:** separate private stages; Codex state changes are discarded when the Run ends and never enter the workspace Run Bundle.
- **Sensitive paths:** `.ssh`, `.gnupg`, and private-key files within the view are denied.

## Without `--safe`

When you run Codex directly (without `--safe`), pVisor keeps host environment inheritance to preserve account and routing configuration. Codex state and project writes reach the host directly and cannot be undone with `drop`. Use `--safe` or `--stage PATH` when you need reviewable changes.

## Credentials

Under `--safe`, deliver credentials explicitly with `--pass-env OPENAI_API_KEY`, or configure a Gateway so the trusted side holds the key; see [credentials and environment](../policies/credentials.md).

## In a VM

For mandatory networking or fixed Linux userspace:

```bash
pvisor run --safe --vm --rootfs image=my-agent-image:latest -- codex
```

The image must contain Codex. See [libkrun VM](../executors/vm.md) for prerequisites and limits.

## Fork

To keep a stopped run's file state but try a different continuation:

```bash
pvisor fork last -- codex
```

See [checkpoints and forks](../fork-checkpoint.md).
