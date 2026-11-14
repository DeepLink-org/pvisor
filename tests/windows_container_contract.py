#!/usr/bin/env python3
"""Real Windows/WHP Docker-style workflows; unavailable virtualization is failure."""

import argparse
import hashlib
import json
import os
import re
import subprocess
import time
from pathlib import Path


def snapshot_store(store, destination, limit=1024 * 1024):
    """Copy bounded host diagnostics, never image blobs or raw root disks."""
    if limit < 1:
        raise ValueError("diagnostic byte limit must be positive")
    instances = store / "instances"
    if not instances.is_dir() or instances.is_symlink():
        return []
    captured = []
    for instance in sorted(instances.iterdir()):
        if (
            not re.fullmatch("[0-9a-f]{32}", instance.name)
            or instance.is_symlink()
            or not instance.is_dir()
        ):
            continue
        for name in ["record.json", "boot.stdout", "boot.stderr", "stdout.log", "stderr.log"]:
            path = instance / name
            if path.is_symlink() or not path.is_file():
                continue
            entry = {"instance": instance.name, "file": name}
            captured.append(entry)
            try:
                with path.open("rb") as source:
                    size = os.fstat(source.fileno()).st_size
                    offset = max(0, size - limit)
                    source.seek(offset)
                    data = source.read(limit)
                output = destination / instance.name / name
                output.parent.mkdir(parents=True, exist_ok=True)
                output.write_bytes(data)
                entry.update(source_bytes=size, offset=offset, captured_bytes=len(data))
            except OSError as error:
                entry["error"] = repr(error)
    return captured


def verify_report(report):
    """A pass flag cannot hide failed commands or teardown failures."""
    if (
        not report["passed"]
        or not report["commands"]
        or not all(case["passed"] for case in report["commands"])
        or report.get("cleanup_errors")
    ):
        raise AssertionError("Windows container acceptance failed; inspect report.json")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, required=True)
    parser.add_argument("--image", default="alpine")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if os.name != "nt":
        parser.error("this acceptance driver requires native Windows and WHP")
    from windows_pty import Pty

    binary = args.binary.resolve(strict=True)
    args.output.mkdir(parents=True, exist_ok=False)
    env = dict(os.environ, PVISOR_DOCKER_HOME=str((args.output / "store").resolve()))
    env["PVISOR_TEST_HOST_ONLY"] = "must-not-be-inherited"
    env.pop("GREETING", None)
    report = {
        "schema": 1,
        "binary_sha256": hashlib.sha256(binary.read_bytes()).hexdigest(),
        "image": args.image,
        "commands": [],
        "passed": False,
    }
    for name in ["libkrunfw.dll", "pvisor-rootfs.bin.gz"]:
        path = binary.parent / name
        report[name + "_sha256"] = (
            hashlib.sha256(path.read_bytes()).hexdigest() if path.is_file() else None
        )
    retained = set()
    live_exec = None
    live_follow = None

    def run(*words, code=0, timeout=180, input=None):
        command = [str(binary), *words]
        index = len(report["commands"])
        case = {"command": command, "expected": code, "passed": False}
        report["commands"].append(case)
        started = time.monotonic()
        process = subprocess.Popen(
            command,
            stdin=subprocess.PIPE if input is not None else subprocess.DEVNULL,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=env,
        )
        try:
            stdout, stderr = process.communicate(input=input, timeout=timeout)
        except subprocess.TimeoutExpired:
            subprocess.run(["taskkill", "/PID", str(process.pid), "/T", "/F"], check=False)
            process.kill()
            stdout, stderr = process.communicate()
            (args.output / f"{index}.stdout").write_bytes(stdout)
            (args.output / f"{index}.stderr").write_bytes(stderr)
            raise AssertionError(f"command deadline exceeded: {words}") from None
        (args.output / f"{index}.stdout").write_bytes(stdout)
        (args.output / f"{index}.stderr").write_bytes(stderr)
        case.update(actual=process.returncode, seconds=time.monotonic() - started)
        assert process.returncode == code, (words, process.returncode, stdout, stderr)
        case["passed"] = True
        return stdout, stderr

    def listing():
        return json.loads(run("ps", "-a", "--json")[0])

    def background(name, script="sleep 300", *options):
        retained.add(name)
        output, _ = run("run", "-d", "--name", name, *options, args.image, "sh", "-c", script)
        identity = output.decode().strip()
        assert len(identity) == 32 and all(c in "0123456789abcdef" for c in identity)
        assert any(c["id"] == identity and c["state"] == "running" for c in listing())
        return identity

    def terminal(words, *, code=0, interrupt=False):
        command = [str(binary), *words]
        index = len(report["commands"])
        case = {"command": command, "terminal": "ConPTY", "expected": code, "passed": False}
        report["commands"].append(case)
        started = time.monotonic()
        session = Pty(command, env)
        try:
            session.expect(b"TTY-READY", timeout=180)
            session.send("literal 你好\r".encode())
            session.expect("VALUE:literal 你好".encode())
            if interrupt:
                session.expect(b"TTY-SLEEP")
                session.send(b"\x03")
                session.expect(b"TTY-INTERRUPTED")
            actual = session.wait(timeout=20)
            case.update(actual=actual, seconds=time.monotonic() - started)
            assert actual == code, (words, actual, session.data())
            case["passed"] = True
        finally:
            try:
                session.close()
            finally:
                (args.output / f"{index}.conpty").write_bytes(session.data())

    try:
        run("pull", args.image)
        run("image", "pull", args.image)
        images = json.loads(run("images", "--json")[0])
        assert any(i["reference"] == args.image and i["id"].startswith("sha256:") for i in images)
        out, err = run("run", "--rm", args.image, "echo", "hello")
        assert out == b"hello\n" and err == b"", (out, err)
        assert listing() == [], "--rm returned before removing its instance"

        # None means no NIC. Default bridge must configure eth0, synthesize DNS,
        # and carry real external TCP traffic, not merely parse an option.
        out, _ = run(
            "run",
            "--rm",
            "--network",
            "none",
            args.image,
            "sh",
            "-c",
            "test -e /sys/class/net/lo || exit 1; "
            'for nic in /sys/class/net/*; do test ! -e "$nic/device" || exit 1; done; '
            "awk 'NR > 1 && $2 == \"00000000\" {found=1} END {exit found}' /proc/net/route "
            "&& printf 'offline\\n'",
        )
        # The firmware kernel also creates dummy0, a virtual sink with no
        # device backing. Isolation requires no attached NIC and no default
        # route, not the absence of kernel-created virtual interfaces.
        assert out == b"offline\n", out
        out, _ = run(
            "run",
            "--rm",
            args.image,
            "sh",
            "-c",
            "test -e /sys/class/net/eth0 && wget -q -T 20 -O - http://example.com",
        )
        assert b"Example Domain" in out, "default bridge did not provide DNS/TCP egress"

        payload = b"stdin literal \x00 \xff\n" * 2048
        out, err = run("run", "--rm", "-i", args.image, "cat", input=payload)
        assert out == payload and not err, "run -i lost bytes or failed to deliver EOF"
        # Leave host stdin open: guest completion must cancel the idle input
        # worker without requiring EOF or another keystroke.
        with (args.output / "idle-input.stderr").open("wb") as stderr:
            idle = subprocess.Popen(
                [str(binary), "run", "--rm", "-i", args.image, "true"],
                stdin=subprocess.PIPE,
                stdout=subprocess.DEVNULL,
                stderr=stderr,
                env=env,
            )
        try:
            assert idle.wait(timeout=180) == 0, "idle host input retained a completed workload"
        finally:
            idle.stdin.close()
            if idle.poll() is None:
                subprocess.run(["taskkill", "/PID", str(idle.pid), "/T", "/F"], check=False)
                idle.kill()
            idle.wait()

        retained.add("terminal")
        terminal(
            [
                "run",
                "-it",
                "--name",
                "terminal",
                args.image,
                "sh",
                "-c",
                "test -t 0 && test -t 1 && test \"$(stty size)\" = '37 91' || exit 99; "
                "printf 'TTY-READY\\n'; IFS= read -r line; printf 'VALUE:%s\\n' \"$line\"; "
                "trap 'printf \"TTY-INTERRUPTED\\n\"; exit 0' INT; printf 'TTY-SLEEP\\n'; sleep 300",
            ],
            interrupt=True,
        )
        run("rm", "terminal")
        retained.discard("terminal")

        retained.add("nonzero")
        out, err = run(
            "run",
            "--name",
            "nonzero",
            args.image,
            "sh",
            "-c",
            "printf 'seven\\n'; printf 'error-stream\\n' >&2; exit 7",
            code=7,
        )
        assert out == b"seven\n" and err == b"error-stream\n"
        out, err = run("logs", "nonzero")
        assert out == b"seven\n" and err == b"error-stream\n", "boot/exec output contaminated logs"
        assert any(c["name"] == "nonzero" and c["exit_code"] == 7 for c in listing())
        run("rm", "nonzero")
        retained.discard("nonzero")
        run("run", "--rm", args.image, "sh", "-c", "exit 255", code=255)
        out, _ = run(
            "run",
            "--rm",
            "-e",
            "GREETING=literal hello world",
            args.image,
            "sh",
            "-c",
            'test -z "${PVISOR_TEST_HOST_ONLY+x}" && printf "%s\\n" "$GREETING"',
        )
        assert out == b"literal hello world\n"

        env_file = args.output / "literal.env"
        env_file.write_text("GREETING=from-file\nOTHER=literal $HOME\n", encoding="utf-8")
        out, _ = run(
            "run",
            "--rm",
            "--cpus",
            "2",
            "-m",
            "384m",
            "-w",
            "/created",
            "--env-file",
            str(env_file),
            "-e",
            "GREETING=from-cli",
            "--entrypoint",
            "/bin/sh",
            args.image,
            "-c",
            'test "$PWD" = /created && test "$GREETING" = from-cli && printf "%s\\n" "$OTHER"',
        )
        assert out == b"literal $HOME\n"
        run("run", "-w", "C:\\not-a-guest-path", args.image, "true", code=125)

        box = background(
            "box",
            "printf 'main-out\\n'; printf 'main-err\\n' >&2; sleep 300",
            "-e",
            "GREETING=launch-default",
        )
        out, err = run("exec", "-i", "box", "cat", input=payload)
        assert out == payload and not err, "exec -i lost bytes or failed to deliver EOF"
        terminal(
            [
                "exec",
                "-it",
                "box",
                "sh",
                "-c",
                "test -t 0 && test -t 1 && test \"$(stty size)\" = '37 91' || exit 99; "
                "printf 'TTY-READY\\n'; IFS= read -r line; printf 'VALUE:%s\\n' \"$line\"; exit 7",
            ],
            code=7,
        )
        # Stock Alpine includes nc, but not the optional httpd applet.
        background("peer", "exec nc -lk -p 8080 -e /bin/cat")
        peer = next(c for c in listing() if c["name"] == "peer")
        assert peer["network"] == "bridge" and peer["ipv4"]
        out, _ = run(
            "exec",
            "box",
            "sh",
            "-c",
            f"for n in 1 2 3 4 5; do printf peer-network-ok | nc -w 3 {peer['ipv4']} 8080 && exit 0; sleep 1; done; exit 1",
        )
        assert out == b"peer-network-ok", "bridge peers did not carry guest TCP packets"
        run("rm", "-f", "peer")
        retained.discard("peer")
        assert run("ps", "-q")[0] == (box[:12] + "\n").encode()
        assert run("container", "ls", "-q", "--no-trunc")[0] == (box + "\n").encode()
        table = run("container", "list")[0].decode().splitlines()
        assert table[0] == "CONTAINER ID\tIMAGE\tCOMMAND\tSTATUS\tPORTS\tNAMES"
        assert len(table[1].split("\t")) == 6 and table[1].split("\t")[3] == "Up"
        follow_out = args.output / "follow.stdout"
        follow_err = args.output / "follow.stderr"
        with follow_out.open("wb") as stdout, follow_err.open("wb") as stderr:
            live_follow = subprocess.Popen(
                [str(binary), "container", "logs", "-f", "box"],
                stdout=stdout,
                stderr=stderr,
                env=env,
            )
        deadline = time.monotonic() + 15
        while b"main-out\n" not in follow_out.read_bytes():
            assert live_follow.poll() is None, "logs -f ended while the owner was live"
            assert time.monotonic() < deadline, "logs -f never delivered retained output"
            time.sleep(0.02)
        run("run", "--name", "box", args.image, "echo", "must-not-start", code=125)
        assert len(listing()) == 1, "duplicate name published another instance"
        run("rm", "box", code=1)
        out, err = run("exec", "box", "sh", "-c", "printf 'exec-out\\n'; printf 'exec-err\\n' >&2")
        assert out == b"exec-out\n" and err == b"exec-err\n"
        run("exec", "box", "sh", "-c", "exit 7", code=7)
        run("exec", "box", "/no-such-executable", code=127)
        out, _ = run(
            "exec",
            "-e",
            "GREETING=exec literal",
            "-w",
            "/tmp",
            "-u",
            "0:0",
            "box",
            "sh",
            "-c",
            'test "$PWD" = /tmp && printf "%s\\n" "$GREETING"',
        )
        assert out == b"exec literal\n"
        out, _ = run(
            "exec",
            "-e",
            "GREETING",
            "box",
            "sh",
            "-c",
            'test -z "${GREETING+x}" && echo explicitly-unset',
        )
        assert out == b"explicitly-unset\n"
        out, _ = run(
            "exec",
            "box",
            "sh",
            "-c",
            "umask 077; printf filesystem-ok > /state; ln -s /state /absolute-link; "
            'test "$(cat /absolute-link)" = filesystem-ok; '
            'test "$(stat -c %a /state)" = 600; '
            "if mknod /tmp/forbidden-device c 1 3 2>/dev/null; then exit 99; fi; "
            "printf 'linux-fs-ok\\n'",
        )
        assert out == b"linux-fs-ok\n"
        out, err = run("logs", "box")
        assert out == b"main-out\n" and err == b"main-err\n"

        # Stop must not queue behind a long-running exec on the command port.
        exec_log = args.output / "long-exec.stdout"
        with exec_log.open("wb") as stdout, (args.output / "long-exec.stderr").open("wb") as stderr:
            child = subprocess.Popen(
                [str(binary), "exec", "box", "sh", "-c", "printf exec-started; sleep 300"],
                stdout=stdout,
                stderr=stderr,
                env=env,
            )
            live_exec = child
            deadline = time.monotonic() + 15
            while b"exec-started" not in exec_log.read_bytes():
                assert child.poll() is None, "long exec ended before stop"
                assert time.monotonic() < deadline, "long exec never became ready"
                time.sleep(0.02)
            out, err = run("exec", "box", "echo", "concurrent", timeout=15)
            assert out == b"concurrent\n" and err == b"", "exec queued behind another execution"
            # Deleting an image-owned file catches accidental layer reapplication.
            run("exec", "box", "rm", "/etc/alpine-release")
            run("stop", "-t", "2", "box", timeout=20)
            assert child.wait(timeout=10) != 0, "main exit did not terminate the long exec"
            live_exec = None
        assert live_follow.wait(timeout=10) == 0, "logs -f did not finish after teardown"
        live_follow = None
        assert follow_out.read_bytes() == b"main-out\n"
        assert follow_err.read_bytes() == b"main-err\n"
        assert any(c["name"] == "box" and c["state"] == "exited" for c in listing())
        run("exec", "box", "echo", "must-not-start", code=1)
        before = next(c for c in listing() if c["name"] == "box")
        run("rmi", args.image, code=1)
        run("rmi", "-f", args.image)
        assert json.loads(run("images", "--json")[0]) == []
        run("start", "box")
        after = next(c for c in listing() if c["name"] == "box")
        assert after["id"] == before["id"] and after["generation"] != before["generation"]
        assert after["ipv4"] == before["ipv4"], "restart changed the retained bridge address"
        assert after["state"] == "running"
        run("start", "box")
        unchanged = next(c for c in listing() if c["name"] == "box")
        assert unchanged["generation"] == after["generation"], (
            "start restarted an already running instance"
        )
        out, _ = run(
            "exec",
            "box",
            "sh",
            "-c",
            'test ! -e /etc/alpine-release && test "$(stat -c %a /state)" = 600 && cat /absolute-link',
        )
        assert out == b"filesystem-ok", "start lost private writes or reapplied the image"
        run("stop", "box")
        run("rm", "box")
        retained.discard("box")

        background("force")
        run("rm", "-f", "force")
        retained.discard("force")
        assert listing() == []

        retained.add("batch")
        run("container", "run", "--name", "batch", "--net", "none", args.image, "true")
        run("container", "remove", "missing", "batch", code=1)
        retained.discard("batch")
        assert listing() == [], "batch removal stopped after the first failure"

        background(
            "signal",
            "trap 'printf signal-stopped; exit 37' USR1; "
            "printf signal-ready; while :; do sleep 1; done",
        )
        deadline = time.monotonic() + 15
        while b"signal-ready" not in run("logs", "signal")[0]:
            assert time.monotonic() < deadline, "signal trap never became ready"
            time.sleep(0.02)
        run("container", "stop", "-t", "-1", "--signal", "USR1", "signal", timeout=20)
        stopped = next(c for c in listing() if c["name"] == "signal")
        assert stopped["exit_code"] == 37, (
            "requested signal was not delivered to the guest workload"
        )
        assert b"signal-stopped" in run("logs", "signal")[0]
        run("rm", "signal")
        retained.discard("signal")
        run("stop", "-t", "-2", "missing", code=2)
        run("rmi", args.image)
        assert json.loads(run("images", "--json")[0]) == []

        # Unsupported host capabilities fail at parsing, before pulling/creation.
        for words in [
            ["run", "-p", "8080:80", args.image],
            ["run", "-v", "C:\\data:/data", args.image],
            ["run", "--network", "host", args.image],
        ]:
            run(*words, code=2)
        assert listing() == []
        report["passed"] = True
    except Exception as error:
        report["error"] = repr(error)
        raise
    finally:
        if not report["passed"]:
            # Preserve terminal state and workload diagnostics before cleanup
            # removes the failed campaign's instances.
            try:
                report["diagnostics"] = snapshot_store(
                    Path(env["PVISOR_DOCKER_HOME"]), args.output / "diagnostics"
                )
            except OSError as error:
                report.setdefault("diagnostic_errors", []).append(repr(error))
            try:
                report["remaining_instances"] = listing()
            except Exception as error:
                report.setdefault("diagnostic_errors", []).append(repr(error))
            for name in retained:
                try:
                    run("logs", name, timeout=10)
                except Exception as error:
                    report.setdefault("diagnostic_errors", []).append(repr(error))
        for name in retained:
            try:
                run("rm", "-f", name, timeout=20)
            except Exception as error:
                report.setdefault("cleanup_errors", []).append(str(error))
        if live_exec is not None:
            if live_exec.poll() is None:
                subprocess.run(["taskkill", "/PID", str(live_exec.pid), "/T", "/F"], check=False)
                live_exec.kill()
            live_exec.wait()
        if live_follow is not None:
            if live_follow.poll() is None:
                subprocess.run(["taskkill", "/PID", str(live_follow.pid), "/T", "/F"], check=False)
                live_follow.kill()
            live_follow.wait()
        if report.get("cleanup_errors"):
            report["passed"] = False
        (args.output / "report.json").write_text(json.dumps(report, indent=2), encoding="utf-8")

    verify_report(report)
    print(f"WINDOWS_CONTAINERS PASS: {len(report['commands'])} CLI invocations")


if __name__ == "__main__":
    main()
