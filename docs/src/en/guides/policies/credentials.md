# Credentials and environment

For most commands, credentials require explicit grants: pVisor projects a small baseline environment instead of ambient secrets.

## Projection

| Case | Behavior |
| --- | --- |
| Most commands | No host inheritance; baseline HOME/PATH/LANG/SHELL/TERM/USER/LOGNAME, etc. |
| Direct Codex without safe | Host inheritance for account/routing compatibility |
| Direct zcode on Linux host | Compatibility policy inherits host environment |
| --pass-env NAME | Explicitly grants the named variable |
| --clear-pass-env | Clears configured pass_env; later explicit grants apply |

`--safe` clears configured pass_env; credentials need explicit CLI grants. `pvisor status --review`: Environment and resources shows projected names. Runtime PVISOR_RUN_ID/AGENTCTL variables and proxy upper/lowercase HTTP_PROXY/HTTPS_PROXY/ALL_PROXY are also injected.

## HOME and credential files

- Safe HOME/CODEX_HOME use private stages. macOS temporary HOME hides original HOME; Linux projects host HOME through a private stage. Writes are discarded after execution.
- Sensitive names are rejected within the view; see [file policies](files.md).
- Linux overlay deny does not hide secrets at original outside-view paths. Use `--filesystem sandbox`/VM for a stronger boundary.

## API key delivery

| Method | Configuration | Agent sees key? |
| --- | --- | --- |
| Environment | --pass-env OPENAI_API_KEY | Yes, and can use it arbitrarily |
| Gateway | Route api_key_env references trusted-side variable | No; agent talks to Gateway |

```bash
pvisor run --safe \
  --gateway-mode capture \
  --gateway-route \
    'name="openai", provider="openai", upstream="https://api.openai.com/v1", api_key_env="OPENAI_API_KEY"' \
  -- my-agent
```

Prefer Gateway when you need the key retained on the trusted side and model requests recorded. See [capture](../capture.md).

## Exclusions

Granted credentials are not usage-tracked or automatically expired. See [threat model](../../security/threat-model.md).
