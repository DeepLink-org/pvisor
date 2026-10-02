# Codex CLI

```bash
pvisor run --safe -- codex
pvisor status --review last
pvisor apply last --path src   # 或：pvisor drop last
```

## What `--safe` mode does for Codex

- **Network:** executable codex selects port 443 on api.openai.com, chatgpt.com, and ab.chatgpt.com.
- **Workspace:** changes remain staged until review/apply.
- **HOME / CODEX_HOME:** private stages; state writes are discarded after execution and excluded from the workspace Bundle.
- **Sensitive paths:** .ssh, .gnupg, and private-key names are rejected within the view.

## Without `--safe` mode

Direct Codex execution inherits the host environment for account/routing compatibility. State and project writes reach the host and cannot be undone with drop. Use `--safe` or an explicit `--stage PATH` for reviewable changes.

## Credentials

`--safe` mode needs explicit `--pass-env OPENAI_API_KEY` projection or a Gateway-held key; see [credentials](../policies/credentials.md).

## In a VM

For mandatory networking or fixed Linux userspace:

```bash
pvisor run --safe --vm --rootfs image=my-agent-image:latest -- codex
```

The image must contain Codex. See [VM prerequisites](../executors/vm.md).

## Fork

To keep a stopped run's file state but try a different continuation:

```bash
pvisor fork last -- codex
```

See [checkpoints and forks](../fork-checkpoint.md).
