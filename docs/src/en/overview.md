---
hide:
  - toc
---

# Get started

Follow one path from an installed CLI to an Agent run you can review.

## 1. Installation

Install the command-line tool and confirm the entry point:

```bash
pip install persisting
pvisor --help
```

On macOS, install macFUSE before using a staged host workspace:

```bash
brew install --cask macfuse
```

[Read the installation guide →](installation.md)

## 2. Running an Agent with pVisor

Run an Agent in a staged workspace, inspect what actually happened, and apply
only the changes you trust:

```bash
pvisor run --stage ./runs/task-001 -- codex
pvisor review last
pvisor apply last --path src
```

The base project stays unchanged while the Agent works. The Run Bundle records
filesystem Effects, effective controls, network evidence, and warnings. Continue
with [Run your first Agent](pvisor/get-started.md) for the complete walkthrough,
then learn [selective apply](pvisor/guides/review-apply.md).

**At the end of this section:** you have a reviewed project change and a clear
record of what remains staged.

## 3. Capture model traffic when you need it

Gateway can record the model traffic of a Run into that Run's directory. It is
optional and stays inside pVisor. Continue with [Capture](pvisor/guides/capture.md)
after the review loop is familiar.
