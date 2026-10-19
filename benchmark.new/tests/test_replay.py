"""Direct replay preparation requires real native-prefix and host-state fidelity."""

import importlib.util
import json
import sys
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "replay"))
sys.path.insert(0, str(ROOT))
spec = importlib.util.spec_from_file_location("replay_prepare", ROOT / "replay/prepare.py")
PREPARE = importlib.util.module_from_spec(spec)
spec.loader.exec_module(PREPARE)
from replay_hooks import prepare, verify  # noqa: E402

BINARY = ROOT.parent / "target/release/pvisor-replay"


@unittest.skipUnless(BINARY.exists(), "built replay required")
class ReplayTests(unittest.TestCase):
    def test_all_native_prefixes_pass_and_tampering_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = Path(tmp) / ".data"
            batch.mkdir()
            (batch / "exports").mkdir()
            with patch.object(PREPARE, "create_batch", return_value=batch):
                PREPARE.prepare_batch(ROOT / "replay/replay.json", BINARY)
            protocol = json.loads((batch / "protocol.json").read_text())
            self.assertEqual(len(protocol), 7)
            for agent in protocol:
                prepare(batch, agent)
                archived = batch / "trials" / agent / "0001"
                self.assertTrue(verify(archived, protocol[agent], agent))
            sentinel = batch / "fixtures/codex/workspace/existing"
            sentinel.write_text("unexpected")
            with self.assertRaisesRegex(ValueError, "contents"):
                prepare(batch, "codex")
            sentinel.write_text(PREPARE.SENTINEL)
            source = batch / "fixtures/codex/trajectory.json"
            source.write_text(source.read_text() + " ")
            with self.assertRaisesRegex(ValueError, "trajectory"):
                prepare(batch, "codex")
            # All archived outcomes still have to match the exact expected prefix.
            result = next((batch / "trials/claude-code/0001/output").glob("*/result.json"))
            data = json.loads(result.read_text())
            data["replayed_tool_calls"] = 1
            result.write_text(json.dumps(data))
            with self.assertRaisesRegex(ValueError, "executed tools"):
                verify(result.parents[2], protocol["claude-code"], "claude-code")
            commands = json.loads((batch / "commands.json").read_text())
            for config in commands.values():
                self.assertEqual(Path(config["argv"][0]).name, "pvisor-replay")
                self.assertEqual(config["argv"][1], "--agent")
                self.assertIn("prepare", config)
                self.assertIn("conclude", config)


if __name__ == "__main__":
    unittest.main()
