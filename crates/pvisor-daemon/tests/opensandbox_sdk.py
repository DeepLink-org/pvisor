"""Opt-in real OpenSandbox 1.1.0 SDK/native VM contract test.

Start an isolated pvisor-daemon with a prepared Ubuntu image, 2 CPU and >=1 GiB
admission capacity. Set OPEN_SANDBOX_API_KEY, then run with --domain HOST:PORT.
No fake runtime, host command execution fallback, or modified SDK is used.
Only sandboxes created by this invocation are deleted.
"""

import argparse
import asyncio
import importlib.metadata
import json
import os
from datetime import timedelta

import httpx
from opensandbox.config import ConnectionConfig
from opensandbox.models.execd import RunCommandOpts
from opensandbox.models.filesystem import DirectoryListEntry, SetPermissionEntry, WriteEntry
from opensandbox.sandbox import Sandbox


def stdout(execution):
    return "".join(item.text for item in execution.logs.stdout)


async def run(domain, image):
    assert importlib.metadata.version("opensandbox") == "1.1.0"
    key = os.environ["OPEN_SANDBOX_API_KEY"]
    config = ConnectionConfig(domain=domain, protocol="http", api_key=key, use_server_proxy=True)
    scoped_config = ConnectionConfig(
        domain=domain, protocol="http", api_key=key, use_server_proxy=False
    )
    owned = []

    async def create(connection):
        sandbox = await Sandbox.create(
            image,
            connection_config=connection,
            resource={"cpu": "1", "memory": "512Mi"},
            env={"SDK_VALUE": "literal $HOME; ✓", "PVISOR_EXECD_ACCESS_TOKEN": "untrusted"},
            ready_timeout=timedelta(seconds=120),
        )
        owned.append(sandbox)
        return sandbox

    try:
        # Observe both concurrent results before propagating failure, so every
        # successfully accepted instance is owned by the cleanup below.
        results = await asyncio.gather(
            create(config), create(scoped_config), return_exceptions=True
        )
        for result in results:
            if isinstance(result, BaseException):
                raise result
        a, b = results
        output = await a.commands.run("printf 'hello ✓'; printf 'error' >&2")
        assert stdout(output) == "hello ✓", repr(output)
        assert "".join(item.text for item in output.logs.stderr) == "error"
        assert (await a.commands.run("exit 7")).exit_code == 7
        output = await a.commands.run(
            'printf "%s|%s|%s" "$SDK_VALUE" "$COMMAND_VALUE" "$PWD"',
            opts=RunCommandOpts(working_directory="/tmp", envs={"COMMAND_VALUE": "argv"}),
        )
        assert stdout(output) == "literal $HOME; ✓|argv|/tmp"
        secret = await a.commands.run(
            'test -z "${PVISOR_EXECD_ACCESS_TOKEN+x}" && test -z "${EXECD_ACCESS_TOKEN+x}"'
        )
        assert secret.exit_code == 0, "transport credentials leaked into workload environment"
        # Background completion acknowledges admission, not process exit.
        background = await a.commands.run(
            "sleep 0.1; printf 'background ✓'", opts=RunCommandOpts(background=True)
        )
        for _ in range(100):
            status = await a.commands.get_command_status(background.id)
            if not status.running:
                break
            await asyncio.sleep(0.02)
        assert not status.running and status.exit_code == 0
        logs = await a.commands.get_background_command_logs(background.id)
        assert logs.content == "background ✓"
        assert (
            await a.commands.get_background_command_logs(background.id, cursor=logs.cursor)
        ).content == ""
        literal = await a.commands.run(["/usr/bin/printf", "%s|%s", "$HOME", ";"])
        assert stdout(literal) == "$HOME|;"
        binary = bytes(range(256)) * 12289
        await a.files.write_files(
            [
                WriteEntry(path="/tmp/sdk.txt", data="pool-backed ✓", mode=644),
                WriteEntry(path="/tmp/sdk.bin", data=binary, mode=600),
            ]
        )
        assert await a.files.read_file("/tmp/sdk.txt") == "pool-backed ✓"
        assert await a.files.read_bytes("/tmp/sdk.bin") == binary
        assert (
            await a.files.read_bytes("/tmp/sdk.bin", range_header="bytes=16381-65539")
            == binary[16381:65540]
        )
        assert await a.files.read_bytes("/tmp/sdk.bin", range_header="bytes=-17") == binary[-17:]
        info = (await a.files.get_file_info(["/tmp/sdk.bin"]))["/tmp/sdk.bin"]
        assert info.size == len(binary) and info.mode == 600
        assert info.created_at.tzinfo is not None and info.modified_at.tzinfo is not None
        entries = await a.files.list_directory(DirectoryListEntry(path="/tmp"))
        assert "/tmp/sdk.bin" in {entry.path for entry in entries}
        await a.files.set_permissions([SetPermissionEntry(path="/tmp/sdk.bin", mode=640)])
        assert (await a.files.get_file_info(["/tmp/sdk.bin"]))["/tmp/sdk.bin"].mode == 640
        await a.files.set_permissions([SetPermissionEntry(path="/tmp/sdk.bin", mode=0)])
        assert (await a.files.get_file_info(["/tmp/sdk.bin"]))["/tmp/sdk.bin"].mode == 0
        await a.files.set_permissions([SetPermissionEntry(path="/tmp/sdk.bin", mode=600)])
        assert (await b.commands.run("test ! -e /tmp/sdk.txt")).exit_code == 0
        assert stdout(await b.commands.run("printf scoped")) == "scoped"
        assert (await a.get_metrics()).cpu_count == 1
        await a.renew(timedelta(minutes=20))
        await a.pause()
        resumed = await Sandbox.resume(a.id, connection_config=config)
        try:
            assert await resumed.files.read_bytes("/tmp/sdk.bin") == binary
        finally:
            await resumed.close()
        connected = await Sandbox.connect(a.id, connection_config=scoped_config)
        try:
            assert stdout(await connected.commands.run("printf reconnected")) == "reconnected"
        finally:
            await connected.close()
        async with httpx.AsyncClient(
            base_url=f"http://{domain}", headers={"OPEN-SANDBOX-API-KEY": key}
        ) as client:
            first = (await client.get(f"/v1/sandboxes/{a.id}/endpoints/44772")).json()
            second = (await client.get(f"/v1/sandboxes/{b.id}/endpoints/44772")).json()
            async with httpx.AsyncClient() as public:
                denied = await public.get(
                    "http://" + second["endpoint"] + "/ping", headers=first["headers"]
                )
                assert denied.status_code == 401, "cross-sandbox credential accepted"
            policy = await client.post(f"/v1/sandboxes/{a.id}/proxy/18080/policy", json={})
            assert policy.status_code == 501, "unsupported policy was accepted"
        return {
            "sdk": "1.1.0",
            "profile": "native-opensandbox",
            "passed": [
                "concurrent-create",
                "server-proxy",
                "scoped-endpoint",
                "stdout-stderr-sse",
                "nonzero-exit",
                "env-cwd",
                "service-secret-filter",
                "utf8-file",
                "binary-file",
                "byte-ranges",
                "file-info-list-timestamps",
                "chmod-zero-and-restore",
                "private-writes",
                "metrics",
                "renew",
                "pause-resume",
                "reconnect",
                "cross-sandbox-auth",
                "network-policy-501",
            ],
        }
    finally:
        cleanup_errors = []
        for sandbox in owned:
            try:
                await sandbox.kill()
            except Exception as error:
                cleanup_errors.append(error)
            finally:
                try:
                    await sandbox.close()
                except Exception as error:
                    cleanup_errors.append(error)
        if cleanup_errors:
            raise RuntimeError("sandbox cleanup failed; reconcile owned IDs") from cleanup_errors[0]


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--domain", required=True)
    parser.add_argument("--image", default="ubuntu")
    args = parser.parse_args()
    print(json.dumps(asyncio.run(run(args.domain, args.image)), ensure_ascii=False))
