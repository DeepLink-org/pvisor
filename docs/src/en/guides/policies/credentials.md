# Credentials and environment

By default the agent cannot see your credentials: pVisor projects only a small set of necessary environment variables, and credentials require an explicit grant.

## Environment projection

| Case | Behavior |
| --- | --- |
| Most commands | Disables host environment inheritance and projects only necessary variables such as `HOME`, `PATH`, `LANG`, `SHELL`, `TERM`, `USER`, and `LOGNAME` |
| Running Codex directly (without `--safe`) | Keeps host environment inheritance to preserve account and routing configuration |
| Running `zcode` directly on a Linux host | Applies a compatibility policy that inherits the host environment |
| Explicit `--pass-env NAME` | Hands the named variable to the agent |
| `--clear-pass-env` | Clears `run.pass_env` from the config file; later `--pass-env` flags still apply |

`--safe` clears `run.pass_env` from the config file, so under `--safe` credentials can only be delivered with an explicit `--pass-env` on the command line. The variables actually projected are listed in the Environment and resources section of `status --review`.

pVisor also injects runtime variables such as `PVISOR_RUN_ID` and `PVISOR_AGENTCTL_*`; when a network proxy is enabled it injects `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY`, and their lowercase forms.

## HOME and local credential files

- **`--safe`:** HOME (and an explicitly set `CODEX_HOME`) uses a separate private stage. On macOS the agent uses a temporary HOME and cannot read the original home directory directly; on Linux the launcher projects the host HOME through a private stage. HOME changes are discarded when the Run ends.
- **`.ssh`, `.gnupg`, and private-key files within the view:** the `--safe` preset denies access; see [file policies](files.md).
- **Linux note:** overlay deny rules do not hide secret files at original paths outside the workspace view; use `--filesystem sandbox` or a VM when you need a stronger guarantee.

## Two ways to deliver an API key

| Method | How | Can the agent see the key? |
| --- | --- | --- |
| Environment variable | `--pass-env OPENAI_API_KEY` | Yes, and it can use it however it likes |
| Gateway-held | Configure a Gateway route with `api_key_env` pointing at a trusted-side variable | No; the agent only connects to the Gateway |

```bash
pvisor run --safe \
  --gateway-mode capture \
  --gateway-route \
    'name="openai", provider="openai", upstream="https://api.openai.com/v1", api_key_env="OPENAI_API_KEY"' \
  -- my-agent
```

Prefer the Gateway: the key stays on the trusted side and model requests can still be recorded. See [Gateway capture](../capture.md) for configuration details.

Gateway-held keys are delegated only to supported model POST endpoints; unknown
or administrative actions are rejected before forwarding. Model discovery is
served locally. A configured model grant can still operate under `no-network`,
which controls ordinary proxy egress. See [Gateway action scope](../../design/gateway.md#delegated-credential-actions)
for supported paths and custom upstream prefix requirements.

## Out of scope

pVisor does not track how credentials handed to the agent through `--pass-env` are used, and it does not make them expire. See the [threat model](../../security/threat-model.md).
