#!/usr/bin/env python3
"""Exercise the actual pvisor product CLI with a Linux command workload."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import signal
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]

def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--ram-storage', choices=['raw', 'compressed'], default='raw')
    parser.add_argument('--fork', action='store_true')
    args = parser.parse_args()
    subprocess.run(['cargo', 'build', '-p', 'pvisor', '--bin', 'pvisor', '--locked', '--offline'], cwd=ROOT, check=True)
    with tempfile.TemporaryDirectory(prefix='pvisor-cli-snapshot-') as directory:
        base = Path(directory)
        binary = base / 'pvisor'
        shutil.copyfile(ROOT/'target/debug/pvisor', binary)
        binary.chmod(0o700)
        subprocess.run(['codesign', '--force', '--sign', '-', '--entitlements', str(ROOT/'crates/pvisor/macos-hypervisor.entitlements'), str(binary)], check=True)
        source = base/'input-rootfs'
        source.mkdir()
        subprocess.run(['rustc', '--edition', '2024', '--target', 'aarch64-unknown-linux-musl', '-C', 'linker=rust-lld', '-C', 'opt-level=2', str(ROOT/'tools/experiments/macos-cold-restore/guest_cli.rs'), '-o', str(source/'workload')], check=True)
        store = base/'store'
        prefix = [str(binary), 'snapshot', '--store', str(store)]
        def command(*args):
            result = subprocess.run(prefix+list(args), text=True, capture_output=True, timeout=180)
            assert result.returncode == 0, result.stdout+result.stderr
            return result.stdout.strip()
        processes = []
        handles = []
        def spawn(*args):
            stdout = (base/f'runner-{len(processes)}.stdout').open('w')
            stderr = (base/f'runner-{len(processes)}.stderr').open('w')
            handles.extend([stdout, stderr])
            process = subprocess.Popen(prefix+list(args), stdout=stdout, stderr=stderr, stdin=subprocess.DEVNULL, start_new_session=True)
            processes.append(process)
            return process
        def wait_file(path, process, previous=None, valid=lambda value: bool(value)):
            deadline = time.monotonic()+45
            while True:
                try:
                    value = path.read_text()
                except FileNotFoundError:
                    value = ''
                # Guest writes truncate then refill: existence alone is not completion.
                if value != previous and valid(value):
                    return value
                if process.poll() is not None or time.monotonic()>deadline:
                    logs = '\n'.join(p.read_text() for p in base.glob('runner-*.*'))
                    logs += '\n'.join(p.read_text() for p in store.glob('runs/*/rootfs/.pvisor-guest-error'))
                    raise RuntimeError('CLI VM did not progress: '+logs)
                time.sleep(0.05)
        def valid_progress(value):
            fields = value.split()
            return len(fields) == 3 and fields[1].isdigit() and fields[2].isdigit()
        try:
            parent = spawn('run', '--name', 'original', '--rootfs', str(source), '--ram-storage', args.ram_storage, '--', '/workload', 'space arg', '--literal')
            private = store/'runs/original/rootfs'
            original = wait_file(private/'ready', parent, valid=valid_progress)
            identity = command('save', 'original')
            parent.wait(timeout=15)
            assert parent.returncode == 0
            assert identity in command('list').splitlines()
            shutil.rmtree(source)
            shutil.rmtree(private)
            branches = [('continued', 'continue')]
            if args.fork:
                branches = [('branch-a', 'branch-a'), ('branch-b', 'branch-b')]
            restored = []
            resumed = []
            for name, marker in branches:
                process = spawn('fork' if args.fork else 'restore', identity, '--name', name)
                restored.append((process, store/'runs'/name/'rootfs', marker))
            for process, work, marker in restored:
                progress = wait_file(work/'ready', process, original, valid=valid_progress)
                assert progress.split()[:2] == original.split()[:2]
                assert int(progress.split()[2]) > int(original.split()[2])
                resumed.append(progress)
            if args.fork:
                assert restored[0][1].stat().st_ino != restored[1][1].stat().st_ino
                assert (restored[0][1]/'held').stat().st_ino != (restored[1][1]/'held').stat().st_ino
            persistent = store/'objects'/identity
            manifest = json.loads((persistent/'manifest.json').read_text())
            content_stats = None
            if args.ram_storage == 'compressed':
                blobs = list((store/'content').iterdir())
                encoded = sum(p.stat().st_size for p in blobs)
                logical = manifest['ram_blocks']['length']
                assert encoded < logical  # fixture-specific encoding, not a density benchmark
                assert not (persistent/'ram.bin').exists()
                assert any(p.stat().st_nlink >= 2 for p in blobs)
                content_stats = {'logical_ram_bytes': logical, 'unique_encoded_frame_bytes': encoded, 'unique_blocks': len(blobs), 'references': len(manifest['ram_blocks']['blocks'])}
            command('delete', identity)
            assert identity not in command('list').splitlines()
            # Reclaim backing blobs before either guest executes its final checks.
            command('gc')
            assert not list((store/'content').iterdir())
            results = []
            for index, (process, work, marker) in enumerate(restored):
                # Branch A has modified RAM and its already-open file before B proceeds.
                if index:
                    assert not (work/'private-branch').exists()
                    assert (work/'held').read_bytes() == b'before-resume-after'
                (work/'release').write_text(marker)
                result = wait_file(work/'result', process, valid=lambda value: value.startswith('cli-snapshot-ok ') and len(value.split()) == 4)
                assert result.startswith('cli-snapshot-ok ')
                assert len((work/'starts').read_text().splitlines()) == 1
                assert (work/'private-branch').read_text() == marker
                assert (work/'held').read_bytes().startswith(marker.encode())
                assert (work/'heap-marker').read_text() == ('29' if marker == 'branch-b' else '19')
                results.append(result)
            for process, work, marker in restored:
                alive = wait_file(work/'alive', process)
                wait_file(work/'alive', process, alive)
                assert (work/'private-branch').read_text() == marker
                assert (work/'held').read_bytes().startswith(marker.encode())
            result = results[0]
            command('gc')
            record = {'scope':'actual product CLI run/save/restore/fork/list/delete/gc, standard guest launcher and argv', 'ram_storage':args.ram_storage, 'concurrent_branches':len(restored), 'branch_results':results, 'persistent_content':content_stats, 'content_gc_before_guest_checks':True, 'snapshot_id':identity, 'guest_before':original, 'guest_after':resumed, 'result':result, 'source_input_and_private_trees_deleted':True, 'published_snapshot_deleted_before_final_check':True, 'source_frontend_exit_code':parent.returncode, 'cli_sha256':hashlib.sha256(binary.read_bytes()).hexdigest(), 'logs':{p.name:p.read_text() for p in base.glob('runner-*.*')}}
            destination = ROOT/'review_project/06-evidence/macos-cold-restore'/f'product-cli-{args.ram_storage}-{"fork" if args.fork else "restore"}-20261003.json'
            destination.write_text(json.dumps(record, ensure_ascii=False, indent=2)+'\n')
            print(json.dumps({'result':result,'evidence':str(destination)},ensure_ascii=False))
        finally:
            for process in processes:
                if process.poll() is None:
                    os.killpg(process.pid, signal.SIGTERM)
                    try: process.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        os.killpg(process.pid, signal.SIGKILL)
                        process.wait()
            for handle in handles: handle.close()

if __name__ == '__main__':
    main()
