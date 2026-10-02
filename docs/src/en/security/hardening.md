# Hardening

Tighten progressively, from low to high risk. Verify the controls actually installed at each level with `pvisor status --review`.

## Level 1: Protect the workspace

```bash
pvisor run --safe -- codex
```

- Workspace changes go to the staging area and reach the project only after review and apply; conflicts refuse to overwrite your edits.
- `.ssh`, `.gnupg`, and private-key files inside the view are rejected, and access to `.env`, `*.pem`, and similar files raises a warning.
- HOME uses an isolated view, and credentials are not passed through by default.

## Level 2: Use mandatory network boundaries

A cooperative proxy constrains only clients that respect proxy settings. When you need a mandatory boundary:

```bash
# 只允许必要的目标（VM 上不可绕过）
pvisor run --safe --vm --rootfs image=my-agent-image:latest \
  --overlaynet-allow api.openai.com:443 -- codex

# 或者在 host 上拒绝所有普通出口
pvisor run --safe --overlaynet-deny-all -- ./agent.sh
```

macOS host `--safe` already blocks direct external connections; selective rules on Linux host remain cooperative. See [network boundaries](../guides/policies/network.md#网络边界) for how the paths differ.

## Level 3: Tighten credentials

- Do not hand long-lived credentials to the agent through `--pass-env`; prefer a configured Gateway where the trusted side holds the upstream key. See [credentials and environment variables](../guides/policies/credentials.md).
- Add rules for secret files with custom names in your project, for example `--access 'config/secrets/**:deny'`; presets recognize only common filenames and do not detect renamed copies or keys in source.
- Do not share credential directories with `--mount SOURCE:write`; explicit shares do not pass through the workspace file rules.

## Level 4: Reduce visibility

- On Linux, confine reads to the projected root with `--filesystem sandbox`.
- On VM, use an OCI image instead of `--rootfs host`, and expose only the paths you need with `--mount`.
- Pin the image digest, agent version, and model version, and keep the Run Bundle for later verification.

## Verify fail-closed

`--strict` requires non-bypassable enforcement evidence for every requested capability dimension, and refuses to start the agent when any is missing. No executor currently claims complete subprocess enforcement, so it refuses to run; use it to confirm that your pipeline stops when a boundary is too weak, not as a stronger sandbox.
