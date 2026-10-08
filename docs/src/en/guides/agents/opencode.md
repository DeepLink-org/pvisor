# OpenCode

!!! note "Experimental"
    The commands below use only existing pVisor options, but no pinned Agent-version end-to-end regression has been run yet.

## Integration starting point using the existing CLI

Install OpenCode inside the selected executor and verify it runs in an independent test project. The target below is api.openai.com:443; the example assumes you already selected the matching OpenAI service in the agent's own configuration — pVisor does not configure providers for you.

```bash
pvisor run --safe --overlaynet-allow api.openai.com:443 \
  --pass-env OPENAI_API_KEY -- opencode
pvisor status --review last
pvisor apply last --path src
```

Explicit allow replaces safe preset lists. Add authentication/provider/dependency destinations individually. Only the direct executable name selects adaptation; shell wrappers change matching. Safe HOME writes are discarded, so do not depend on persistent login state from this run.
