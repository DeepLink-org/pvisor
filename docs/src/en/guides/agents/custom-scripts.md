# Scripts and automation commands

Run an offline script, inspect its report, then accept the report and discard temporary notes. No model account or third-party dependencies are needed. Complete [Host staging setup](../../start/installation.md#local-prerequisites) first.

## Run a script {#run-script}

<div class="pv-guide-steps" markdown="1">

1. ### Prepare the script {#prepare-script}

    Create the script in a new directory. Choose another name if it already exists, and run the remaining steps in this terminal and directory.

    ```bash title="Shell"
    mkdir pvisor-script-demo
    cd pvisor-script-demo
    cat > agent.sh <<'SCRIPT'
    #!/bin/sh
    set -eu
    mkdir -p output
    echo 'Report generated' > output/report.txt
    echo 'Temporary notes' > scratch.txt
    SCRIPT
    chmod +x agent.sh
    ```

1. ### Run and inspect the report {#inspect-script}

    ```bash title="Shell"
    pvisor spawn --safe --overlaynet-deny-all -- ./agent.sh
    pvisor status --review last
    pvisor inspect last -- cat output/report.txt
    ```

    The last command should print `Report generated`. Review should list `output/report.txt` and `scratch.txt`; neither report nor notes should exist in the original directory yet. Failed commands can also leave candidates, so inspect records before deciding.

1. ### Accept the report and clean up {#accept-report}

    ```bash title="Shell"
    pvisor apply last --path output
    pvisor drop last
    cat output/report.txt
    test ! -e scratch.txt && echo 'scratch.txt: absent'
    ```

    Expect the report contents and `scratch.txt: absent`. The report is now in the original directory, the notes are discarded, and the original script remains.

</div>

## Behavior

- The command runs in the staged view, which contains the project's current files; writes go to the stage.
- `pvisor spawn` returns the command's exit code unchanged, so scripts and CI can judge success or failure.
- If the command exits nonzero, its file changes stay staged for you to review and apply or drop.
- Everything after `--` is passed through unchanged as the command and arguments.

## Networking

When a script matches no agent preset, `--safe` denies all outbound access. Choose as needed:

| Need | Option |
| --- | --- |
| Fully offline | `--overlaynet-deny-all` |
| Only a few destinations | `--overlaynet-allow pypi.org:443` (repeatable) |
| Reject a destination | `--overlaynet-deny 169.254.0.0/16` |

When used as command names, `bash`, `sh`, `zsh`, and `fish` reuse the Codex preset destinations; see [`--safe` presets](../../reference/cli.md#safe-参数预设).

## Environment variables

`--safe` projects only the necessary variables. Pass the variables a script needs explicitly with `--pass-env NAME`; see [credentials and environment](../policies/credentials.md).

## Complete example

[Your first run](../../start/first-run.md) demonstrates offline file staging and selective acceptance. The runnable examples under [`examples/pvisor/`](https://github.com/DeepLink-org/pvisor/tree/main/examples/pvisor) cover file isolation, changeset management, and network isolation.
