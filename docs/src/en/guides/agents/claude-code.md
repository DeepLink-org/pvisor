---
description: "Run Claude Code under pVisor: what --safe allows on the network, how to pass ANTHROPIC_API_KEY, and how to review and apply its file changes."
---

# Claude Code

Use Claude Code to fix a Python function that mishandles whitespace. After testing the fix, accept the source and tests and discard the experiment notes. You will use Host stagebox, Python's standard library, and your own model account.

## Complete task: repair failing tests {#repair-task}

<div class="pv-guide-steps" markdown="1">

1. ### Prepare tools and authentication {#task-prerequisites}

    Complete [Host staging setup](../../start/installation.md#local-prerequisites) and the [offline demo](../../start/first-run.md#agent-style). Check versions in the current terminal:

    ```bash title="Shell"
    pvisor --version
    claude --version
    python3 --version
    ```

    Record versions for reproduction. Claude Code must be installed locally and Python must be 3.10+. Set `ANTHROPIC_API_KEY` in this terminal using your existing credential manager; pass it explicitly with `--pass-env` at launch. Keep keys out of project files. Model calls use your own account quota; see [credentials](#credentials) below for login sessions and other provider configurations.

1. ### Create a small project with failing tests {#task-project}

    Use a new directory, choosing a different name if it already exists. Run all following steps in this directory and terminal. No Git or third-party dependencies are needed.

    ```bash title="Shell"
    mkdir pvisor-agent-demo
    cd pvisor-agent-demo
    cat > slugify.py <<'PYCODE'
    def slugify(text):
        return text.lower().replace(" ", "-")
    PYCODE
    cat > test_slugify.py <<'PYCODE'
    import unittest
    from slugify import slugify


    class SlugifyTests(unittest.TestCase):
        def test_words(self):
            self.assertEqual(slugify("Hello World"), "hello-world")

        def test_trim(self):
            self.assertEqual(slugify("  Hello World  "), "hello-world")

        def test_whitespace(self):
            self.assertEqual(slugify("Hello\t \nWorld"), "hello-world")
    PYCODE
    python3 -B -m unittest -v
    ```

    Expect 3 tests, 2 failures, and a nonzero exit. The agent will fix these failures. If you see import or syntax errors, check the files before starting it.

1. ### Launch the agent and give it the task {#task-agent}

    ```bash title="Shell"
    pvisor spawn --safe --pass-env ANTHROPIC_API_KEY -- claude
    ```

    Paste this task into the agent:

    ```text
    Fix slugify in slugify.py: lowercase the input, trim surrounding whitespace, and collapse consecutive whitespace (including tabs and newlines) into one hyphen.
    Preserve the three existing tests in test_slugify.py and add a test that whitespace-only input returns an empty string.
    Modify only slugify.py and test_slugify.py; also create experiment-notes.txt describing the attempts, as an experiment record to leave out of acceptance.
    Install no dependencies, access no external services, and make no Git commits or pushes.
    Run the tests first to confirm failure. After the fix, run python3 -B -m unittest -v and report the actual result.
    ```

    The agent may request tool approvals. Decide based on the actual prompts without enabling unverified allow-all flags. Record prompts and human interventions. Retain authentication or connectivity errors and check the network and credential settings below. Exit the agent when done to return to the original terminal.

1. ### Inspect results and experiment records {#task-review}

    ```bash title="Shell"
    pvisor status last
    pvisor review last --diff
    pvisor inspect last -- python3 -B -m unittest -v
    ```

    Expect 4 passing tests. `-B` prevents bytecode-cache writes, and these tests only read files using the standard library, allowing them to run in the read-only inspect view. Do not generalize this to tests that write caches or artifacts.

    Check the diff against the task: the three original tests must remain intact, the whitespace-only test must be present, and the implementation must meet the requirements. Check that `experiment-notes.txt` contains only experiment notes. The agent's answer does not replace this review. Source and tests in the original directory should still be unchanged.

1. ### Accept selected files and retest in the original directory {#task-accept}

    ```bash title="Shell"
    pvisor apply last --path slugify.py --path test_slugify.py
    python3 -B -m unittest -v
    test ! -e experiment-notes.txt && echo 'Experiment notes not accepted'
    ```

    Expect 4 passing tests again and `Experiment notes not accepted`. The accepted files should work in the original project without the experiment environment. If validation fails, retain the stage and check for omitted dependencies or candidate edits.

    If review confirmed that `experiment-notes.txt` remains an unaccepted candidate, discard it:

    ```bash title="Shell"
    pvisor drop last
    python3 -B -m unittest -v
    ```

    Expect 4 passing tests afterward. If the agent created no notes and all candidates were accepted, skip drop. Review unexpected files before deciding. For command failure or cancellation, inspect Job status and retain available records before following [troubleshooting](../troubleshooting.md).

</div>

## Verification and approval scope {#verification}

| Item | Existing evidence or acceptance requirement |
| --- | --- |
| Real-client tool loop | Existing records use Claude Code 2.1.128 with local deterministic model responses; see [end-to-end tasks](../../benchmarks/agent-tasks.md) for configuration and scope. |
| This tutorial's cloud-model repair task | A cloud-model run of this task using a release artifact still needs end-to-end verification. |
| Operation without repeated approvals | A verified version/configuration matrix is not yet established; tool-loop tests do not establish this behavior. |
| Reproduction record | Keep OS, pVisor/Claude versions, launch command, before/after test results, Job identity, and actual approval prompts; omit keys. |

This tutorial retains Claude's own approval settings. Initial authentication, tool permissions, or other confirmations may still appear. File review/apply does not undo model charges or remote operations. Source and tests are the intended deliverables; notes are explicitly designated experiment records in this task. The product does not automatically classify or certify files as safe.

## What `--safe` does for Claude Code

The network preset matches the `claude` executable and allows only `api.anthropic.com:443`; other destinations are denied by default. Workspace staging, discarded HOME state and sensitive-path rules follow the [common Agent settings](index.md).

## Credentials {#credentials}

`--safe` does not pass host environment credentials to the agent. Pick one:

- Deliver `ANTHROPIC_API_KEY` explicitly with `--pass-env ANTHROPIC_API_KEY`.
- Configure a Gateway route so the trusted side holds the upstream key and the agent never sees it; see [credentials and environment](../policies/credentials.md).

On macOS, `--safe` uses a temporary HOME, so any host-side logged-in session state is unavailable. On Linux, HOME is projected through a private stage, and state the agent writes does not return to the host.

## Additional destinations

When the agent needs to install dependencies or reach a documentation site, add targets explicitly. `--overlaynet-allow` replaces the preset list, so list the model API alongside them:

```bash title="Shell"
pvisor spawn --safe \
  --overlaynet-allow api.anthropic.com:443 \
  --overlaynet-allow pypi.org:443 \
  --pass-env ANTHROPIC_API_KEY -- claude
```

## Network strength

On a macOS host, `--safe` blocks direct external connections. On a Linux host, selective rules run through a cooperative proxy and direct sockets can still bypass them. Use a VM when you need an unbypassable boundary; see [network boundaries](../policies/network.md#网络边界).
