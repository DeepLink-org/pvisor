"""Optional transports for B-SWE-RESIDENT-ENG; no model or policy API calls.

AgentENV uses the same pinned E2B routing as the validated native adapter.
The current pVisor transport uses its existing container CLI; it does not
resurrect the retired sweeval_resident binary or enable historical offload.
"""

import os
import shlex
import subprocess
import uuid


class PvisorBackend:
    def __init__(self, config):
        self.config = config
        self.name = "replay-" + uuid.uuid4().hex
        self.attempted = False

    def command(self, *args):
        return subprocess.run(
            [self.config["binary"], *args], text=True, capture_output=True, check=False
        )

    def start(self):
        self.attempted = True
        c = self.config
        value = self.command(
            "run",
            "-d",
            "--name",
            self.name,
            "--network",
            "none",
            "--cpus",
            str(c["vcpus"]),
            "--memory",
            str(c["memory"]),
            "--entrypoint",
            "/bin/sh",
            c["image"],
            "-c",
            "while :; do sleep 3600; done",
        )
        if value.returncode:
            raise RuntimeError("sandbox creation failed")

    def exec(self, command):
        c = self.config
        value = self.command(
            "exec", "-u", "root", "-w", c["cwd"], self.name, "/bin/bash", "-c", command
        )
        return dict(return_code=value.returncode, stdout=value.stdout, stderr=value.stderr)

    def stop(self):
        if not self.attempted:
            return dict(api_deleted=False, physical_exit_verified=False)
        value = self.command("rm", "-f", self.name)
        if value.returncode:
            raise RuntimeError("sandbox removal failed; inspect owned name")
        return dict(api_deleted=True, physical_exit_verified=False, sandbox_id=self.name)


class AgentenvBackend:
    def __init__(self, config):
        self.config = config
        self.sandbox = None
        self.sandbox_id = None

    def start(self):
        # Private SDK routing is pinned and isolated here, not exposed as a new
        # AgentENV service API. Import only when this backend is executed.
        from importlib.metadata import version

        if version("e2b") != "2.5.0":
            raise RuntimeError("AgentENV adapter requires e2b==2.5.0")
        from e2b import Sandbox
        from e2b.connection_config import ConnectionConfig
        from e2b.sandbox_sync.sandbox_api import SandboxApi
        from packaging.version import Version

        c = self.config
        key = os.environ[c["api_key_env"]]
        response = SandboxApi._create_sandbox(
            template=c["template"],
            timeout=c["vm_timeout_s"],
            allow_internet_access=False,
            metadata={"purpose": "command-replay"},
            env_vars={},
            secure=True,
            lifecycle={"on_timeout": "kill", "auto_resume": False},
            api_key=key,
            api_url=c["endpoint"],
            sandbox_url=c["endpoint"],
            request_timeout=120,
        )
        self.sandbox_id = response.sandbox_id  # retain ownership before client construction
        headers = {"E2b-Sandbox-Id": response.sandbox_id, "E2b-Sandbox-Port": "49983"}
        if response.envd_access_token:
            headers["X-Access-Token"] = response.envd_access_token
        connection = ConnectionConfig(
            api_key=key,
            api_url=c["endpoint"],
            sandbox_url=c["endpoint"],
            extra_sandbox_headers=headers,
            request_timeout=15,
        )
        self.sandbox = Sandbox(
            sandbox_id=response.sandbox_id,
            sandbox_domain=response.sandbox_domain,
            envd_version=Version(str(response.envd_version)),
            envd_access_token=response.envd_access_token,
            traffic_access_token=response.traffic_access_token,
            connection_config=connection,
        )

    def exec(self, command):
        from e2b.sandbox.commands.command_handle import CommandExitException

        try:
            value = self.sandbox.commands.run(
                "cd "
                + shlex.quote(self.config["cwd"])
                + " && /bin/bash -c "
                + shlex.quote(command),
                user="root",
                timeout=0,
            )
        except CommandExitException as exc:
            value = exc
        return dict(return_code=value.exit_code, stdout=value.stdout, stderr=value.stderr)

    def stop(self):
        if self.sandbox is None:
            # A create timeout may have created a VM. Do not guess safe cleanup.
            raise RuntimeError("creation identity/client incomplete; inspect dedicated instance")
        self.sandbox.kill()
        return dict(api_deleted=True, physical_exit_verified=False, sandbox_id=self.sandbox_id)


def build_backend(config):
    """Validate explicit transport configuration without importing optional SDKs."""
    kind = config.get("kind")
    fields = {
        "pvisor": ("binary", "image", "memory", "cwd"),
        "agentenv": ("endpoint", "template", "api_key_env", "cwd"),
    }
    if kind not in fields:
        raise ValueError("backend kind must be pvisor or agentenv")
    for name in fields[kind]:
        if not isinstance(config.get(name), str) or not config[name]:
            raise ValueError("missing backend field: " + name)
    number = "vcpus" if kind == "pvisor" else "vm_timeout_s"
    if type(config.get(number)) is not int or config[number] <= 0:
        raise ValueError("positive integer required: " + number)
    if kind == "agentenv":
        from urllib.parse import urlsplit

        url = urlsplit(config["endpoint"])
        if (
            url.scheme not in ("http", "https")
            or not url.hostname
            or url.username
            or url.password
            or url.query
            or url.fragment
        ):
            raise ValueError("endpoint must be an HTTP(S) URL without credentials/query")
    return PvisorBackend(config) if kind == "pvisor" else AgentenvBackend(config)
