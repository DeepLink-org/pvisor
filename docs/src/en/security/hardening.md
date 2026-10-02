# Hardening

Narrow permissions progressively and verify installed controls through `pvisor status --review`.

## 1. Protect the workspace

```bash
pvisor run --safe -- codex
```

Changes remain staged; conflicts refuse overwrite. Safe denies common private-key paths, warns on .env/pem, and uses private HOME with explicit credentials.

## 2. Mandatory networking

Cooperative proxies cover cooperating clients only:

```bash
# 只允许必要的目标（VM 上不可绕过）
pvisor run --safe --vm --rootfs image=my-agent-image:latest \
  --overlaynet-allow api.openai.com:443 -- codex

# 或者在 host 上拒绝所有普通出口
pvisor run --safe --overlaynet-deny-all -- ./agent.sh
```

macOS safe already blocks direct external connections; Linux selective host rules remain cooperative. See [network boundaries](../guides/policies/network.md#网络边界).

## 3. Credentials

- Prefer Gateway-held upstream keys to long-lived keys passed through `--pass-env`; see [credentials](../guides/policies/credentials.md).
- Add rules such as `--access 'config/secrets/**:deny'` for custom secret paths. Filename presets cannot detect renamed/embedded secrets.
- Do not share credential directories with `--mount SOURCE:write`; explicit shares bypass workspace rules.

## 4. Reduce visibility

- Linux `--filesystem sandbox` confines the projected root.
- Prefer VM images over `--rootfs host`; expose only necessary paths with `--mount`.
- Pin image digests, agent/model versions, and retain Bundles.

## Verify fail-closed

`--strict` rejects before agent startup when any required dimension lacks mandatory evidence. Current subprocess gaps make it reject all executors. Use this to check pipeline refusal, not as an available stronger sandbox preset.
