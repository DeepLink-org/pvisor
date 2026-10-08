# Claude Code

```bash
pvisor run --safe --pass-env ANTHROPIC_API_KEY -- claude
pvisor status --review last
pvisor apply last --path src   # 或：pvisor drop last
```

## What `--safe` does for Claude Code

- **Network:** the preset matches the `claude` executable and allows only `api.anthropic.com:443`; other destinations are denied by default.
- **Workspace:** changes go to the stage and are applied after review.
- **HOME:** a separate private stage; state Claude Code writes under HOME is discarded when the Run ends and never enters the Run Bundle.
- **Sensitive paths:** `.ssh`, `.gnupg`, and private-key files within the view are denied.

## Credentials

`--safe` does not pass host environment credentials to the agent. Pick one:

- Deliver `ANTHROPIC_API_KEY` explicitly with `--pass-env ANTHROPIC_API_KEY`.
- Configure a Gateway route so the trusted side holds the upstream key and the agent never sees it; see [credentials and environment](../policies/credentials.md).

On macOS, `--safe` uses a temporary HOME, so any host-side logged-in session state is unavailable. On Linux, HOME is projected through a private stage, and state the agent writes does not return to the host.

## Additional destinations

When the agent needs to install dependencies or reach a documentation site, add targets explicitly. `--overlaynet-allow` replaces the preset list, so list the model API alongside them:

```bash
pvisor run --safe \
  --overlaynet-allow api.anthropic.com:443 \
  --overlaynet-allow pypi.org:443 \
  --pass-env ANTHROPIC_API_KEY -- claude
```

## Network strength

On a macOS host, `--safe` blocks direct external connections. On a Linux host, selective rules run through a cooperative proxy and direct sockets can still bypass them. Use a VM when you need an unbypassable boundary; see [network boundaries](../policies/network.md#网络边界).
