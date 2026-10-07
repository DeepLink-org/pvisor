#!/usr/bin/env python3
"""Real Linux whole-machine save/exit/restore experiment (macOS aarch64)."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
DRIVER = ROOT / 'target/debug/examples/vm_linux_cold_restore'
FIRMWARE = Path(os.environ.get('PVISOR_CASE_VM_LIBRARY_DIR', str(ROOT / 'target/release')))

def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--report", type=Path, default=ROOT / "target/vm-validation/linux-machine.json")
    args = parser.parse_args()
    if not (FIRMWARE / 'libkrunfw.5.dylib').is_file():
        raise RuntimeError('Set PVISOR_CASE_VM_LIBRARY_DIR to a local built firmware directory')
    subprocess.run(['cargo', 'build', '-p', 'pvisor', '--example', 'vm_linux_cold_restore', '--locked', '--offline'], cwd=ROOT, check=True)
    subprocess.run(['codesign', '--force', '--sign', '-', '--entitlements', str(ROOT / 'crates/pvisor/macos-hypervisor.entitlements'), str(DRIVER)], check=True)
    with tempfile.TemporaryDirectory(prefix='pvisor-linux-cold-restore-') as directory:
        base = Path(directory)
        root = base / 'rootfs'
        root.mkdir()
        subprocess.run(['rustc', '--edition', '2024', '--target', 'aarch64-unknown-linux-musl', '-C', 'linker=rust-lld', '-C', 'opt-level=2', str(ROOT / 'crates/pvisor-vm/src/probes/guest_linux.rs'), '-o', str(root / 'init.krun')], check=True)
        env = dict(os.environ, PVISOR_CASE_VM_LIBRARY_DIR=str(FIRMWARE.resolve()))
        started = time.monotonic()
        source = subprocess.run([str(DRIVER), 'save', str(base)], env=env, text=True, capture_output=True, timeout=45)
        print('SOURCE:', source.returncode, source.stdout, source.stderr, flush=True)
        if source.returncode != 0:
            raise RuntimeError('source save failed')
        original = (root / 'ready').read_text()
        saved = json.loads((base / 'state.json').read_text())
        assert 'linux-save-runner-exiting' in source.stdout
        manifest_path = base / 'state.json'
        pristine = manifest_path.read_bytes()
        rejected = []
        for field, message in [('boot', 'snapshot host/build mismatch'), ('state', 'machine state digest mismatch')]:
            invalid = json.loads(pristine)
            if field == 'boot':
                invalid['boot'] = 'different-boot'
            else:
                invalid['state']['version'] = 99
            manifest_path.write_text(json.dumps(invalid))
            bad = subprocess.run([str(DRIVER), 'restore', str(base)], env=env, capture_output=True, text=True, timeout=10)
            assert bad.returncode != 0 and message in bad.stderr, bad.stderr
            rejected.append(message)
            manifest_path.write_bytes(pristine)
        with (base / 'ram.bin').open('r+b') as ram:
            byte = ram.read(1)
            ram.seek(0)
            ram.write(bytes([byte[0] ^ 1]))
        bad = subprocess.run([str(DRIVER), 'restore', str(base)], env=env, capture_output=True, text=True, timeout=10)
        assert bad.returncode != 0 and 'RAM snapshot digest mismatch' in bad.stderr
        rejected.append('RAM snapshot digest mismatch')
        with (base / 'ram.bin').open('r+b') as ram:
            ram.write(byte)
        # subprocess.run has reaped the old runner before the new one is spawned.
        out = base / 'restore.stdout'
        err = base / 'restore.stderr'
        with out.open('w') as stdout, err.open('w') as stderr:
            target = subprocess.Popen([str(DRIVER), 'restore', str(base)], env=env, stdout=stdout, stderr=stderr, text=True)
            try:
                deadline = time.monotonic() + 30
                while not (root / 'ready').exists() or (root / 'ready').read_text() == original:
                    if target.poll() is not None:
                        raise RuntimeError('restore exited: ' + out.read_text() + err.read_text())
                    if time.monotonic() >= deadline:
                        raise TimeoutError('restored Linux heartbeat: ' + out.read_text() + err.read_text())
                    time.sleep(0.05)
                resumed = (root / 'ready').read_text()
                assert resumed.split()[:2] == original.split()[:2], (original, resumed)
                assert int(resumed.split()[2]) > int(original.split()[2])
                contender = subprocess.run([str(DRIVER), 'restore', str(base)], env=env, capture_output=True, text=True, timeout=10)
                assert contender.returncode != 0 and 'another runner owns' in contender.stderr
                (root / 'release').write_text('go')
                while not (root / 'result').exists():
                    if target.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError('guest result missing: ' + out.read_text() + err.read_text())
                    time.sleep(0.05)
                result = (root / 'result').read_text()
                assert result.startswith('linux-cold-restore-ok ')
                assert len((root / 'starts').read_text().splitlines()) == 1
                evidence = {'scope': 'real Linux full-machine same-host cross-runner experiment', 'elapsed_seconds': time.monotonic()-started, 'firmware_sha256': hashlib.sha256((FIRMWARE/'libkrunfw.5.dylib').read_bytes()).hexdigest(), 'guest_sha256': hashlib.sha256((root/'init.krun').read_bytes()).hexdigest(), 'cpu_count': len(saved['state']['cpus']), 'device_inventory': [{'base': item['base'], 'len': item['len'], 'kind': item['device']['kind'], 'virtio_type': item['device']['state'].get('device_type') if item['device']['kind'] == 'Virtio' else None} for item in saved['state']['devices']], 'state_hash': saved['state_hash'], 'ram_hash': saved['ram_hash'], 'negative_checks': rejected, 'source_pid': saved['source_pid'], 'restore_pid': target.pid, 'source_reaped_before_restore': True, 'guest_before': original, 'guest_after': resumed, 'result': result, 'starts': (root/'starts').read_text(), 'source_stdout': source.stdout, 'source_stderr': source.stderr, 'restore_stdout': out.read_text(), 'restore_stderr': err.read_text(), 'ram_bytes': (base/'ram.bin').stat().st_size, 'driver_sha256': hashlib.sha256(DRIVER.read_bytes()).hexdigest(), 'same_execution_lease_rejected_second_runner': True}
                destination = args.report
                destination.parent.mkdir(parents=True, exist_ok=True)
                destination.write_text(json.dumps(evidence, ensure_ascii=False, indent=2)+'\n')
                print(json.dumps({'result': result, 'evidence': str(destination)}, ensure_ascii=False), flush=True)
            finally:
                if target.poll() is None:
                    target.terminate()
                    try:
                        target.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        target.kill()
                        target.wait()

if __name__ == '__main__':
    main()
