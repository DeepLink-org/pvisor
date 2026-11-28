"""Check host metadata and data changes through virtio-fs. Requires x86_64 KVM."""

import argparse
import json
import os
import selectors
import shutil
import subprocess
import tempfile
import time
from pathlib import Path


def final_counter(profile, component, name, unit="calls"):
    records = [
        json.loads(line.removeprefix("pvisor-fs-profile "))
        for line in profile.splitlines()
        if line.startswith("pvisor-fs-profile ")
    ]
    final = [
        record["measurements"]
        for record in records
        if record["component"] == component and record["final_record"]
    ]
    if not final:
        raise RuntimeError(f"missing final {component} profile")
    return sum(m.get(name, {}).get(unit, 0) for m in final)


def expect_counter(profile, component, name, enabled, unit="calls"):
    count = final_counter(profile, component, name, unit)
    if bool(count) != enabled:
        raise RuntimeError(f"expected {name} {'> 0' if enabled else '= 0'}, got {count}")
    print(f"{name}={count}", flush=True)


def run(binary, guest, inline, log=None, workers=1, cpus=1, firmware=None, irq_ack=None):
    with tempfile.TemporaryDirectory() as directory:
        root = Path(directory)
        if firmware is not None:
            (root / "dev").mkdir()
        shutil.copy2(guest, root / "probe")
        subject = root / "subject"
        subject.write_bytes(b"old")
        subject.chmod(0o644)
        os.utime(subject, (1, 1))
        (root / "link").symlink_to("subject")
        command = [
            binary,
            *(["--firmware", firmware] if firmware is not None else []),
            "--rootfs",
            root,
            "--cpus",
            str(cpus),
            "--memory",
            "512",
            "--task-stdio",
            "--",
            "/probe",
        ]
        env = os.environ | {
            "PVISOR_GUEST_KERNEL": "linux" if firmware is not None else "rust-native",
            "PVISOR_VM_FS_WORKERS": str(workers),
            "PVISOR_FS_PROFILE": "1",
        }
        if firmware is not None:
            env.pop("PVISOR_VM_FS_IRQ_READ_ACK", None)
            if irq_ack is not None:
                env["PVISOR_VM_FS_IRQ_READ_ACK"] = str(irq_ack)
            command.append("00" if irq_ack == 0 else ("10" if inline == 0 else "11"))
        env.pop("PVISOR_VM_FS_INLINE_METADATA", None)
        if inline is not None:
            env["PVISOR_VM_FS_INLINE_METADATA"] = str(inline)
        with tempfile.TemporaryFile() as errors, subprocess.Popen(
            command,
            env=env,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=errors,
        ) as process:
            deadline = time.monotonic() + 30
            pending = b""
            try:
                with selectors.DefaultSelector() as selector:
                    selector.register(process.stdout, selectors.EVENT_READ)
                    for step in (b"rewrite", b"rewrite-scatter", b"resize", b"grow-seek",
                                 b"truncate", b"remove", b"recreate"):
                        while b"\n" not in pending:
                            remaining = deadline - time.monotonic()
                            if remaining <= 0 or not selector.select(remaining):
                                raise TimeoutError(f"guest did not request {step!r}")
                            block = os.read(process.stdout.fileno(), 4096)
                            if not block:
                                raise RuntimeError(f"guest exited before {step!r}")
                            pending += block
                            if len(pending) > 4096:
                                raise RuntimeError("unexpectedly large guest handshake")
                        line, pending = pending.split(b"\n", 1)
                        if line != step:
                            raise RuntimeError(f"expected {step!r}, got {line!r}")
                        if step in (b"rewrite", b"rewrite-scatter"):
                            with subject.open("r+b") as file:
                                file.write(b"hot" if step == b"rewrite" else b"iov")
                            stamp = 2 if step == b"rewrite" else 3
                            os.utime(subject, (stamp, stamp))
                        elif step == b"grow-seek":
                            os.truncate(subject, 12)
                        elif step == b"truncate":
                            subject.write_bytes(b"ok")
                        elif step == b"remove":
                            subject.rename(root / "old-subject")
                        else:
                            subject.write_bytes(b"overgrown" if step == b"resize" else b"new!")
                            subject.chmod(0o600 if step == b"resize" else 0o640)
                        process.stdin.write(b"\n")
                        process.stdin.flush()
                output, _ = process.communicate(timeout=max(0, deadline - time.monotonic()))
                if process.returncode != 7 or pending or output:
                    raise RuntimeError(f"guest exit {process.returncode}, stdout={pending + output!r}")
            except Exception as error:
                process.kill()
                process.wait()
                errors.seek(0)
                details = errors.read().decode(errors="replace")
                raise RuntimeError(f"inline={inline}: {error}\n{details}") from error
            finally:
                errors.seek(0)
                profile = errors.read().decode(errors="replace")
                if log is not None:
                    log.write_text(profile)
        print(f"PASS: cpus={cpus}, workers={workers}, inline={inline}, irq_ack={irq_ack}: "
              "host stat and data changes visible")
        return profile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", required=True, type=Path, help="vm_api_case")
    parser.add_argument("--guest", required=True, type=Path, help="statically linked stat_visibility.c")
    parser.add_argument("--firmware", type=Path, help="test this Linux firmware instead of Rust native")
    args = parser.parse_args()
    binary = args.binary.resolve(strict=True)
    guest = args.guest.resolve(strict=True)
    if args.firmware is not None:
        firmware = args.firmware.resolve(strict=True)
        for cpus, inline, ack in ((1, None, None), (1, 0, None), (2, None, None), (1, None, 0)):
            profile = run(binary, guest, inline, workers=4, cpus=cpus, firmware=firmware, irq_ack=ack)
            expect_counter(profile, "virtio-fs-dispatch", "vcpu_inline_metadata",
                           cpus == 1 and inline is None and ack is None, "units")
        return
    for workers in (1, 4):
        for inline in (None, 0):
            run(binary, guest, inline, workers=workers)


if __name__ == "__main__":
    main()
