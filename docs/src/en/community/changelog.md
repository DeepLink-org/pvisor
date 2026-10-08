# Changelog

See the repository-root [`CHANGELOG.md`](https://github.com/DeepLink-org/pvisor/blob/main/CHANGELOG.md) for the full history, and [GitHub Releases](https://github.com/DeepLink-org/pvisor/releases) for version release notes.

## What to check before upgrading

Record your current pVisor version or commit and the agent, model, and image versions you use. Check changes between your current and target versions to CLI arguments, TOML fields, record schemas, executor prerequisites, and platform artifacts.

Run an independent Job with the new version to verify policy admission, review-output reading, selective apply, and conflict refusal. If you use replay, also validate your adapter. Retain old Bundles, Journals, capture artifacts, and the tools that can read them. Do not remove schema fields to bypass version checks; see [Stability and compatibility](../reference/stability.md) for current reading rules.

## What to explain when submitting a change

Describe user-visible behavior, applicable platforms and executors, validation, and any required changes to commands, configuration, or record readers in the PR. Mark incompatible changes explicitly, identifying affected interfaces and upgrade actions. This does not establish long-term compatibility or deprecation windows.
