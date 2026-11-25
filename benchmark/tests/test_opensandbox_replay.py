"""Contracts for the opt-in Kata Firecracker replay transport."""

import sys
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "replay"))
from opensandbox_backend import OpenSandboxBackend  # noqa: E402


class KataReplayTests(unittest.TestCase):
    def setUp(self):
        self.config = dict(runtime_class="kata-fc", fc_handler="kata-fc", image="image@digest",
                           vm_timeout_s=14400, vcpus=2, memory="8Gi")
        self.backend = OpenSandboxBackend(self.config)
        self.sdk = Mock()
        self.sandbox = self.sdk.create.return_value
        self.sandbox.id = "test-id"
        self.pod = {"metadata": {"uid": "pod-uid", "name": "guest"},
                    "spec": {"runtimeClassName": "kata-fc", "nodeName": "node"}}
        modules = {"opensandbox": SimpleNamespace(SandboxSync=self.sdk),
                   "opensandbox.models.sandboxes": SimpleNamespace(NetworkPolicy=dict)}
        self.addCleanup(patch.stopall)
        patch.dict(sys.modules, modules).start()
        patch("opensandbox_backend.sdk_connection", return_value=object()).start()
        self.kube = patch.object(self.backend, "kube").start()

    def test_placement_and_cleanup(self):
        self.kube.side_effect = [{"handler": "kata-fc"}, {"items": [self.pod]}]
        self.backend.start()
        self.assertEqual(self.backend.pod["uid"], "pod-uid")
        self.assertEqual(self.sdk.create.call_args.kwargs["resource"], {"cpu": "2", "memory": "8Gi"})
        self.kube.assert_called_with("pods", "-l", "opensandbox.io/id=test-id")
        self.assertTrue(self.backend.stop()["api_deleted"])
        self.sandbox.destroy.assert_called_once_with()

    def test_wrong_handler_stops_before_create(self):
        self.kube.return_value = {"handler": "runc"}
        with self.assertRaisesRegex(RuntimeError, "handler"):
            self.backend.start()
        self.sdk.create.assert_not_called()

    def test_failed_pod_check_still_cleans_owned_sandbox(self):
        self.kube.side_effect = [{"handler": "kata-fc"}, {"items": []}]
        with self.assertRaisesRegex(RuntimeError, "unique"):
            self.backend.start()
        self.assertTrue(self.backend.stop()["api_deleted"])

    def test_create_error_keeps_unknown_outcome(self):
        self.kube.return_value = {"handler": "kata-fc"}
        self.sdk.create.side_effect = TimeoutError("create")
        with self.assertRaises(TimeoutError):
            self.backend.start()
        self.assertEqual(self.backend.stop()["creation_outcome"], "unknown")
