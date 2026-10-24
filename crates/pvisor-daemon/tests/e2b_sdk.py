"""Opt-in unmodified E2B 2.59.0 SDK acceptance against real native VMs.

Use an isolated daemon with e2b-base prepared, 2 CPUs / 1 GiB admission capacity,
E2B_API_KEY in the environment, and --api-url http://127.0.0.1:8080.
Only sandboxes created by this test are removed. No fake envd or SDK patches.
"""

import argparse
import asyncio
import concurrent.futures
import importlib.metadata
import os

import httpx
from e2b import AsyncSandbox, CommandExitException, Sandbox
from e2b.sandbox.sandbox_api import SandboxQuery


def run(api_url, template):
    assert importlib.metadata.version("e2b") == "2.59.0"
    key = os.environ["E2B_API_KEY"]
    options = {"api_url": api_url, "sandbox_url": api_url + "/e2b", "api_key": key}
    owned = []
    metadata = {"sdk-test": "native &+%= 雪"}

    def create():
        return Sandbox.create(
            template,
            timeout=600,
            metadata=metadata,
            envs={
                "SDK_VALUE": "literal $HOME; ✓",
                "PVISOR_ENVD_ACCESS_TOKEN": "untrusted",
                "LARGE_ENV": "v" * 24000,
            },
            **options,
        )

    try:
        with concurrent.futures.ThreadPoolExecutor(2) as pool:
            futures = [pool.submit(create) for _ in range(2)]
            concurrent.futures.wait(futures)
            # Collect every successful identity before propagating a failure.
            owned.extend(f.result() for f in futures if f.exception() is None)
            for future in futures:
                future.result()
        a, b = owned
        assert a.is_running() and b.is_running()
        assert a.get_info().memory_mb == 512
        assert a.commands.run('printf %s "$LARGE_ENV" | wc -c').stdout.strip() == "24000"
        out, err = [], []
        result = a.commands.run(
            "printf 'hello ✓'; printf error >&2; printf '\\n%s %s' \"$SDK_VALUE\" \"$PWD\"",
            cwd="/tmp",
            on_stdout=out.append,
            on_stderr=err.append,
        )
        assert result.stdout == "hello ✓\nliteral $HOME; ✓ /tmp"
        assert result.stderr == "error"
        assert "".join(out) == result.stdout and "".join(err) == result.stderr
        assert not a.commands.run("env | sort").stdout.count("PVISOR_ENVD_ACCESS_TOKEN")
        assert a.commands.run("id -un", user="root").stdout.strip() == "root"
        try:
            a.commands.run("printf failure >&2; exit 7")
            raise AssertionError("nonzero exit was hidden")
        except CommandExitException as error:
            assert error.exit_code == 7 and error.stderr == "failure"
        handle = a.commands.run(
            "read line; printf 'received:%s' \"$line\"", background=True, stdin=True
        )
        handle.send_stdin("input ✓\n")
        assert handle.wait().stdout == "received:input ✓"
        pending = a.commands.run("sleep 60", background=True)
        assert any(p.pid == pending.pid for p in a.commands.list())
        assert pending.kill()
        pending.disconnect()
        a.files.make_dir("/tmp/e2b-sdk")
        a.files.write("/tmp/e2b-sdk/中文.txt", "UTF-8 ✓")
        assert a.files.read("/tmp/e2b-sdk/中文.txt") == "UTF-8 ✓"
        binary = bytes(range(256)) * (3 * 4096 + 1)
        a.files.write("/tmp/e2b-sdk/binary", binary)
        assert a.files.read("/tmp/e2b-sdk/binary", format="bytes") == binary
        assert {f.name for f in a.files.list("/tmp/e2b-sdk")} == {"中文.txt", "binary"}
        a.files.rename("/tmp/e2b-sdk/中文.txt", "/tmp/e2b-sdk/renamed.txt")
        assert a.files.exists("/tmp/e2b-sdk/renamed.txt")
        assert not b.files.exists("/tmp/e2b-sdk/binary")
        b.files.write("/tmp/private-write", "second VM")
        assert not a.files.exists("/tmp/private-write")
        paginator = Sandbox.list(query=SandboxQuery(metadata=metadata), limit=1, **options)
        found = []
        while paginator.has_next:
            found.extend(item.sandbox_id for item in paginator.next_items())
        assert set(found) == {a.sandbox_id, b.sandbox_id}
        a.set_timeout(120)
        assert (a.get_info().end_at - a.get_info().started_at).total_seconds() < 130
        assert a.pause()
        assert not a.pause()
        assert not a.is_running()
        connected = Sandbox.connect(a.sandbox_id, timeout=600, **options)
        assert connected.sandbox_id == a.sandbox_id
        assert connected.files.read("/tmp/e2b-sdk/binary", format="bytes") == binary
        connected.files.remove("/tmp/e2b-sdk/renamed.txt")
        assert not connected.files.exists("/tmp/e2b-sdk/renamed.txt")
        with httpx.Client(base_url=api_url, trust_env=False, timeout=120) as client:
            token = client.get(f"/sandboxes/{a.sandbox_id}", headers={"X-API-Key": key}).json()[
                "envdAccessToken"
            ]
        headers = {
            "E2b-Sandbox-Id": a.sandbox_id,
            "E2b-Sandbox-Port": "49983",
            "X-Access-Token": token,
        }
        with httpx.Client(base_url=api_url, trust_env=False, timeout=120) as client:
            assert client.get("/e2b/envs", headers=headers).status_code == 200
            assert (
                client.get(
                    "/e2b/envs", headers={**headers, "E2b-Sandbox-Id": b.sandbox_id}
                ).status_code
                == 401
            )
            assert client.get("/sandboxes", headers={"X-API-Key": token}).status_code == 401
            for path in ("init", "%69nit", "freeze", "unfreeze", "upgrade"):
                assert client.post("/e2b/" + path, headers=headers, json={}).status_code == 403
            assert (
                client.post(
                    "/v2/sandboxes",
                    headers={"X-API-Key": key},
                    json={"templateID": template, "allow_internet_access": False},
                ).status_code
                == 501
            )
        print(
            "PASS: E2B sync SDK, two real VMs, streams, stdin, signals, binary files, isolation, pagination, TTL, pause/connect, authentication",
            flush=True,
        )
    finally:
        for sandbox in owned:
            sandbox.kill()
        for sandbox in owned:
            assert not sandbox.is_running()

    async def check_async():
        sandbox = await AsyncSandbox.create(template, **options)
        try:
            assert (await sandbox.commands.run("printf 'async ✓'")).stdout == "async ✓"
            await sandbox.files.write("/tmp/async-sdk", "async content")
            assert await sandbox.files.read("/tmp/async-sdk") == "async content"
        finally:
            await sandbox.kill()
        print("PASS: E2B async SDK create/command/file/delete", flush=True)

    asyncio.run(check_async())


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--api-url", default="http://127.0.0.1:8080")
    parser.add_argument("--template", default="e2b-base")
    args = parser.parse_args()
    run(args.api_url.rstrip("/"), args.template)
