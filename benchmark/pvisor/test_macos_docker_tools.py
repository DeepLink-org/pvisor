"""Reject incomplete or invalid worker observations as benchmark samples."""

import unittest

import macos_docker_tools as bench


class MacosDockerToolsTests(unittest.TestCase):
    def test_worker_record_allows_unrelated_tool_output(self):
        assert (
            bench.worker_result(
                b'npm output\nPVISOR_TOOL_WORKER {"case":"rg-2048","worker_ms":1.5}\n', "rg-2048"
            )
            == 1.5
        )

    def test_invalid_worker_is_not_a_performance_sample(self):
        for output in [
            b"no worker record",
            b'PVISOR_TOOL_WORKER {"case":"other","worker_ms":1}\n',
            b'PVISOR_TOOL_WORKER {"case":"rg-2048","worker_ms":-1}\n',
            b'PVISOR_TOOL_WORKER {"case":"rg-2048","worker_ms":NaN}\n',
            b'PVISOR_TOOL_WORKER {"case":"rg-2048","worker_ms":true}\n',
            b'PVISOR_TOOL_WORKER {"case":"rg-2048","worker_ms":1}\n' * 2,
        ]:
            with self.subTest(output=output):
                with self.assertRaises(ValueError):
                    bench.worker_result(output, "rg-2048")
