"""Real KVM adapter smoke on a prepared rootfs; no model API is called."""
import argparse
import asyncio
import json
import os
from pathlib import Path

from environment import PVisorEnvironment


async def run(args):
    env = PVisorEnvironment(rootfs=args.rootfs, firmware=args.firmware, binary=args.binary,
                            python=args.python, output=args.output, memory_mib=1024,
                            apparmor_profile=args.apparmor_profile,
                            krun_enomem_workaround=args.krun_enomem_workaround)
    try:
        await env.start()
        await env.run_healthcheck()
        result = await env.exec('echo resident > /tmp/swe-state; echo err >&2; exit 9')
        assert result.return_code == 9 and result.stderr == 'err\n'
        result = await env.exec('cat /tmp/swe-state')
        assert result.stdout == 'resident\n'
        result = await env.exec('sleep 20', timeout_sec=.2)
        assert result.timed_out
        await env.run_healthcheck()
        source = Path(args.output) / 'upload.bin'
        source.write_bytes(bytes(range(256)) * 8192)
        await env.upload_file(source_path=source, target_path='/tmp/roundtrip.bin')
        target = Path(args.output) / 'download.bin'
        await env.download_file(source_path='/tmp/roundtrip.bin', target_path=target)
        assert source.read_bytes() == target.read_bytes()
        directory = Path(args.output) / 'upload-tree'
        directory.mkdir()
        (directory / 'file').write_text('tree')
        await env.upload_dir(source_dir=directory, target_dir='/tmp/roundtrip-tree')
        await env.download_dir(source_dir='/tmp/roundtrip-tree',
                               target_dir=Path(args.output) / 'download-tree')
        assert (Path(args.output) / 'download-tree/file').read_text() == 'tree'
        if args.force_stop:
            # Exercise service SIGTERM while the worker is still running.
            # systemd signals only the driver first; it must cancel and retain RunResult.
            proc = await asyncio.create_subprocess_exec(
                'systemctl', '--user', 'kill', '--kill-whom=main', '--signal=SIGTERM', env.unit)
            assert await proc.wait() == 0
            await asyncio.wait_for(env.process.wait(), 15)
    finally:
        await env.stop()
    driver = json.loads((Path(args.output) / 'run-result.json').read_text())
    shutdown = json.loads((Path(args.output) / 'driver-shutdown.json').read_text())
    assert driver['state'] == ('cancelled' if args.force_stop else 'completed'), driver
    assert shutdown['signal'] == ('SIGTERM' if args.force_stop else None), shutdown
    result = dict(driver_state=driver['state'], signal=shutdown['signal'], correctness='passed', checks=['persistent VM', 'stdout/stderr/exit',
                  'timeout and subsequent exec', 'binary upload/download', 'directory transfer'],
                  model_calls=0)
    (Path(args.output) / 'smoke-result.json').write_text(json.dumps(result, indent=2))
    print(json.dumps(result))


if __name__ == '__main__':
    os.umask(0o077)
    parser = argparse.ArgumentParser()
    for name in ('rootfs', 'firmware', 'binary', 'output', 'python'):
        parser.add_argument('--' + name, required=True)
    parser.add_argument('--apparmor-profile')
    parser.add_argument('--force-stop', action='store_true',
                        help='validate SIGTERM cancellation and retained driver result')
    parser.add_argument('--krun-enomem-workaround', action=argparse.BooleanOptionalAction,
                        default=None)
    asyncio.run(run(parser.parse_args()))
