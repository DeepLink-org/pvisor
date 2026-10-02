# Claude Code

```bash
pvisor run --safe --pass-env ANTHROPIC_API_KEY -- claude
pvisor status --review last
pvisor apply last --path src   # 或：pvisor drop last
```

## What `--safe` mode does for Claude Code

- **Network:** executable `claude` selects api.anthropic.com:443; other destinations deny.
- **Workspace:** changes wait in staging until apply.
- **HOME:** private stage; HOME writes are discarded after the Run, outside the workspace Bundle.
- **Sensitive paths:** .ssh, .gnupg, and private-key names are rejected within the view.

## Credentials

`--safe` mode does not pass ambient credentials. Either explicitly pass ANTHROPIC_API_KEY or use a Gateway-held upstream key; see [credentials](../policies/credentials.md).

On macOS safe HOME is temporary and existing host login state is unavailable. Linux projects HOME through a private stage; agent writes do not reach host HOME. Use explicit credentials for stable authentication.

## Additional destinations

Dependency downloads or documentation may need extra grants. `--overlaynet-allow` replaces the preset list, so include the model API too:

```bash
pvisor run --safe \
  --overlaynet-allow api.anthropic.com:443 \
  --overlaynet-allow pypi.org:443 \
  --pass-env ANTHROPIC_API_KEY -- claude
```

## Network strength

macOS safe host blocks direct external connections. Linux selective host rules are cooperative; direct sockets may bypass them. Use a VM for mandatory selective enforcement; see [network boundaries](../policies/network.md#网络边界).
