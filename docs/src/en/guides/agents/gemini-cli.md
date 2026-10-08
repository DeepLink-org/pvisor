# Gemini CLI

!!! note "Experimental"
    The commands below use only existing pVisor options, but no pinned Agent-version end-to-end regression has been run yet.

## Integration starting point using the existing CLI

Install Gemini CLI inside the selected executor and verify it runs in an independent test project. The target below is generativelanguage.googleapis.com:443; configure the model provider in the agent itself — pVisor does not do that for you.

```bash
pvisor run --safe --overlaynet-allow generativelanguage.googleapis.com:443 \
  --pass-env GEMINI_API_KEY -- gemini
pvisor status --review last
pvisor apply last --path src
```

Explicit allow replaces safe preset lists. Add authentication/provider/dependency destinations individually. Only the direct executable name selects adaptation; shell wrappers change matching. Safe HOME writes are discarded, so do not depend on persistent login state from this run.
