"""Check readiness accounting without requiring Hypervisor.framework."""
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace

from vm_ready import checkpoints, trial


class ReadinessTests(unittest.TestCase):
    def test_marker_and_closed_accounting(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            (output / 'trials').mkdir()
            row = trial(SimpleNamespace(output=output), ('direct', 'direct', None, None, None), 0)
            self.assertEqual(row['exit'], 0)
            self.assertGreater(row['ready_ms'], 0)
            self.assertGreater(row['completion_ms'], 0)
        stages = ['process.entry', 'vm.spawn_begin', 'process.entry', 'runner.krun_enter',
                  'runner.vmm_built', 'session.storage_begin', 'session.storage_ready',
                  'storage.record_write_begin', 'storage.record_write_ready',
                  'storage.overlay_begin', 'storage.overlay_ready', 'session.begin',
                  'session.agentctl_ready', 'vm.ram_backing_begin', 'vm.ram_backing_ready',
                  'vm.spec_write_begin', 'vm.spec_write_ready', 'runner.devices_configured',
                  'runner.attestation_ready']
        marks = [dict(stage=s, monotonic_us=str((i + 1) * 1000)) for i, s in enumerate(stages)]
        result = checkpoints(marks, 0, 20_000_000)
        self.assertEqual(sum(result['phases'].values()), 20)
        with self.assertRaises(ValueError):
            checkpoints(marks[:-1], 0, 20_000_000)


if __name__ == '__main__':
    unittest.main()
