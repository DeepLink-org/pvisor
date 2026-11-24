---
description: Install pVisor on Apple Silicon and run an Alpine container that prints hello.
---

# Run your first container {#container-quickstart}

Install pVisor and run an Alpine container that prints `hello`. You don't need
a Docker daemon, a model account, or an API key.

## Before you start {#prerequisites}

- **Apple Silicon Mac:** `uname -sm` prints `Darwin arm64`, and
  `sysctl -n kern.hv_support` prints `1`. For Linux x86_64, use the
  [container walkthrough](first-run.md#docker-style) instead.
- **Download tools:** `command -v curl tar shasum` lists all three. See
  [other installation methods](installation.md#installation-options) if one is missing.
- **Network access:** The installer downloads from GitHub, and the first run
  pulls `alpine:3.20` from Docker Hub. Allow room for the image cache.

## 1. Install the stable CLI {#install-cli}

This command runs the project's shell installer. To review it first, download
the script without piping it to `sh`.

```sh
curl -fsSL https://raw.githubusercontent.com/DeepLink-org/pvisor/main/scripts/install.sh | sh
```

Expect an `Installed pVisor` line. The installer uses your home directory and
doesn't change your shell configuration.

No installation message? Check access to GitHub or see the
[installation options](installation.md#installation-options).

## 2. Check that pVisor can create a VM {#check-vm}

```sh
"$HOME/.local/share/pvisor/cli/current/bin/pvisor" vm-check --json
```

Expect `"can_create_vm": true` and `"firmware": "embedded"`. The check creates
an empty VM; it doesn't boot the container yet.

If either value differs, check the [VM prerequisites](installation.md#docker-prerequisites).

## 3. Run Alpine and see `hello` {#run-container}

```sh
"$HOME/.local/share/pvisor/cli/current/bin/pvisor" run --rm --network none alpine:3.20 echo hello
```

After any image download messages, expect:

```text
hello
```

That line came from the container. `--rm` removes the container after it exits;
the downloaded image remains cached. The host pulls the image before the guest
starts with networking disabled.

If the pull fails, check Docker Hub access and disk space. If the image pulls
but the VM fails to start, see [VM troubleshooting](../guides/executors/vm.md).

## Next steps {#next-steps}

- [Keep a container running and run more commands](first-run.md#docker-exec).
- [Add `pvisor` to `PATH`](installation.md#shell-installer).
- [Ask for help](https://github.com/DeepLink-org/pvisor/discussions).
