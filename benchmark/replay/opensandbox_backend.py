"""OpenSandbox transport for Kubernetes deployments using Kata Firecracker."""

import json
import os
import shlex
import subprocess
from datetime import timedelta
from urllib.parse import urlsplit


def sdk_connection(config):
    from opensandbox.config import ConnectionConfigSync

    endpoint = urlsplit(config["endpoint"])
    if (endpoint.scheme not in ("http", "https") or not endpoint.hostname or endpoint.username is not None
            or endpoint.password is not None or endpoint.query or endpoint.fragment or endpoint.path not in ("", "/")):
        raise ValueError("Expected an HTTP(S) origin in endpoint")
    return ConnectionConfigSync(domain=endpoint.netloc, protocol=endpoint.scheme,
                               api_key=os.environ[config["api_key_env"]],
                               use_server_proxy=config.get("use_server_proxy", False),
                               request_timeout=timedelta(seconds=210))


class OpenSandboxBackend:
    def __init__(self, config):
        self.config = config
        self.sandbox = None
        self.create_attempted = False
        self.pod = None

    def kube(self, *args):
        return json.loads(subprocess.check_output([
            "kubectl", "--context", self.config["kube_context"], "--namespace",
            self.config["namespace"], "--request-timeout=15s", "get", *args, "-o", "json",
        ], text=True, timeout=20))

    def start(self):
        from opensandbox import SandboxSync
        from opensandbox.models.sandboxes import NetworkPolicy

        c = self.config
        connection = sdk_connection(c)
        runtime = self.kube("runtimeclass", c["runtime_class"])
        if runtime["handler"] != c["fc_handler"]:
            raise RuntimeError("RuntimeClass handler does not match configured Kata-FC handler")
        self.create_attempted = True
        self.sandbox = SandboxSync.create(
            c["image"], connection_config=connection, env=c.get("env", {}),
            timeout=timedelta(seconds=c["vm_timeout_s"]), ready_timeout=timedelta(minutes=3),
            network_policy=NetworkPolicy(default_action="deny"),
            resource={"cpu": str(c["vcpus"]), "memory": c["memory"]},
            entrypoint=["/bin/sh", "-c", "exec sleep infinity"],
        )
        pods = self.kube("pods", "-l", f"opensandbox.io/id={self.sandbox.id}")["items"]
        if len(pods) != 1 or pods[0]["spec"].get("runtimeClassName") != c["runtime_class"]:
            raise RuntimeError("Sandbox did not resolve to a unique Kata-FC pod")
        pod = pods[0]
        self.pod = dict(uid=pod["metadata"]["uid"], name=pod["metadata"]["name"],
                        node=pod["spec"]["nodeName"], runtime_class=c["runtime_class"],
                        handler=runtime["handler"], sandbox_id=self.sandbox.id)

    def exec(self, command):
        result = self.sandbox.commands.run(
            "cd " + shlex.quote(self.config["cwd"]) + " && /bin/bash -c " + shlex.quote(command)
        )
        code = result.error.exit_code if result.error else result.exit_code
        if code is None:
            raise RuntimeError("OpenSandbox did not return a command exit status")
        return dict(return_code=code, stdout=result.text,
                    stderr="\n".join(line.text for line in result.logs.stderr))

    def stop(self):
        if self.sandbox is None:
            if self.create_attempted:
                return dict(api_deleted=False, creation_outcome="unknown", physical_exit_verified=False)
            return dict(api_deleted=False, physical_exit_verified=False)
        self.sandbox.destroy()
        receipt = dict(api_deleted=True, physical_exit_verified=False, pod=self.pod,
                       sandbox_id=self.sandbox.id)
        return receipt
