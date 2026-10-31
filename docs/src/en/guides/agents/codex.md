---
description: "Run Codex CLI under pVisor: the --safe network allowlist, separate HOME and CODEX_HOME stages, and reviewing staged file changes."
---

# Codex CLI

Run Codex in your project, inspect its staged changes, and accept selected files after it exits. This path uses a locally installed Codex CLI, an API key, and Host stagebox for project writes.

!!! note "Before you start"
    Complete [installation and Host staging setup](../../start/installation.md#local-prerequisites) and install Codex CLI. Choose a small task you can test and prepare model access. Retain Codex's own approval settings.

## Run Codex {#run-codex}

<div class="pv-guide-steps" markdown="1">

1. ### Check tools and open your project {#check-tools}

    Open a terminal at the project root and check both tools:

    ```bash title="Shell"
    pvisor --version
    codex --version
    ```

    Expect two version numbers. Record them and the current project state. Set `OPENAI_API_KEY` in this terminal through your existing credential manager, keeping the key out of project files.

1. ### Authenticate and launch in the staged environment {#launch-codex}

    Run from Bash or a compatible host shell:

    ```bash title="Shell"
    pvisor spawn --safe \
      --overlaynet-allow api.openai.com:443 \
      --pass-env OPENAI_API_KEY -- sh -c '
      printenv OPENAI_API_KEY | codex login --with-api-key && exec codex
    '
    ```

    Authentication and Codex run in the same Job's private HOME. Login reads the key from standard input, keeping it out of command arguments. See [official OpenAI authentication guidance](https://developers.openai.com/codex/auth/).

    !!! tip "Authentication state"
        `--safe` discards HOME writes at task end, so authenticate again on the next launch. This example allows only the model API; configure dependency downloads and other destinations separately using [network policy](../policies/network.md).

1. ### Give it a task and inspect changes {#review-task}

    Specify a goal such as fixing a named failing test, preserving existing tests, and running relevant checks. Exit Codex when finished, then run in the original terminal:

    ```bash title="Shell"
    pvisor status last
    pvisor review last --diff
    ```

    Check task status, file differences, and actual test results. Files should remain staged, with no changes accepted into the original directory yet. `last` selects the most recently registered Job for this workspace; finish review before starting another Job here.

1. ### Accept selected files {#accept-task}

    Select paths that exist in your project, for example the source directory:

    ```bash title="Shell"
    pvisor apply last --path src
    ```

    Rerun relevant tests in the original project. Once the remaining candidates are unnecessary, run `pvisor drop last`. Skip drop if everything was accepted. See [review and apply](../review-apply.md) for conflicts and failure recovery.

</div>

!!! note "Verification scope"
    This is an API-key integration path, without cloud-model end-to-end or approval-free acceptance evidence. Existing controlled-client tests use Codex 0.160.0; see [end-to-end tasks](../../benchmarks/agent-tasks.md) for scope. For task inputs with failing tests and explicit acceptance results, use the [Python repair example](claude-code.md#task-project) and give its task to Codex.

## What `--safe` does for Codex

The network preset matches the `codex` executable and allows only port 443 on `api.openai.com`, `chatgpt.com` and `ab.chatgpt.com`. HOME and `CODEX_HOME` use separate private stages; state changes are discarded when the Run ends and never enter the workspace Run Bundle. Workspace staging and sensitive-path rules follow the [common Agent settings](index.md).

## Without `--safe`

When you run Codex directly (without `--safe`), pVisor keeps host environment inheritance to preserve account and routing configuration. Codex state and project writes reach the host directly and cannot be undone with `drop`. Use `--safe` or `--stage PATH` when you need reviewable changes.

## Credentials

Under `--safe`, deliver credentials explicitly with `--pass-env OPENAI_API_KEY`, or configure a Gateway so the trusted side holds the key; see [credentials and environment](../policies/credentials.md).

## In a VM

For mandatory networking or fixed Linux userspace:

```bash
pvisor spawn --safe --vm --rootfs image=my-agent-image:latest -- codex
```

The image must contain Codex. See [libkrun VM](../executors/vm.md) for prerequisites and limits.

## Fork

To keep file state and try another approach, first confirm that the stopped host Job meets the [workspace fork prerequisites](../fork-checkpoint.md):

```bash
pvisor fork last -- codex
```

See [checkpoints and forks](../fork-checkpoint.md).
