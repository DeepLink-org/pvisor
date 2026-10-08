# Policy model

An execution passes through five steps, each recording something different; they cannot substitute for one another:

- **Request**: the permissions you declare (`--safe`, `--access`, `--overlaynet-*`, and so on).
- **Admission**: what the selected executor can provide, at most `Planned`.
- **Effective constraints**: the result of intersecting the user, workspace, session, and executor base policies.
- **Installed controls**: which enforcement mechanisms the executor actually installed.
- **Observed results**: the observations the executor reports at teardown—this is the evidence of what actually took effect.

Policy **narrows layer by layer**: a later `allow` cannot override an earlier explicit `deny`. A layer that declares rules but omits `default_action` denies by default; with no policy configured at all, the network runs public and reads are unrestricted—to restrict them you must declare it. `--safe` requires isolation to be installed and rejects sensitive paths; `--strict` validates every requested dimension and refuses to run without evidence.

For fields and presets see the [policy fields reference](../reference/policy.md) (in progress); for the evidence definitions see [capabilities and evidence](capabilities-and-evidence.md).
