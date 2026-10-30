---
description: "Run Codex CLI under pVisor: the --safe network allowlist, separate HOME and CODEX_HOME stages, and reviewing staged file changes."
---

# Codex CLI

```bash
pvisor spawn --safe -- codex
pvisor status --review last
pvisor apply last --path src   # or: pvisor drop last
```

## What `--safe` does for Codex

The network preset matches the `codex` executable and allows only port 443 on `api.openai.com`, `chatgpt.com` and `ab.chatgpt.com`. HOME and `CODEX_HOME` use separate private stages; state changes are discarded when the Run ends and never enter the workspace Run Bundle. Workspace staging and sensitive-path rules follow the [common Agent settings](index.md).

## Without `--safe`

When you run Codex directly (without `--safe`), pVisor keeps host environment inheritance to preserve account and routing configuration. Codex state and project writes reach the host directly and cannot be undone with `drop`. Use `--safe` or `--stage PATH` when you need reviewable changes.

## Credentials

Under `--safe`, deliver credentials explicitly with `--pass-env OPENAI_API_KEY`, or configure a Gateway so the trusted side holds the key; see [credentials and environment](../policies/credentials.md).

## In a VM

For mandatory networking or fixed Linux userspace:

```bash
pvisor spawn --safe --vm --rootfs image=my-agent-image:latest -- codex
```

The image must contain Codex. See [libkrun VM](../executors/vm.md) for prerequisites and limits.

## Fork

To keep file state and try another approach, first confirm that the stopped host Job meets the [workspace fork prerequisites](../fork-checkpoint.md):

```bash
pvisor fork last -- codex
```

See [checkpoints and forks](../fork-checkpoint.md).
