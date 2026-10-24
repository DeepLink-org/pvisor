"""Direct replay preparation requires real native-prefix and host-state fidelity."""

import hashlib
import importlib.util
import json
import os
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

BINARY = Path(os.environ.get("PVISOR_REPLAY_BIN", ROOT.parent / "target/release/pvisor-replay"))
SEMSPEC = Path(os.environ.get("SEMSPEC_BIN", ROOT.parent / "target/semspec/debug/semspec"))
SOURCE = ROOT / "replay/replay.md"


@unittest.skipUnless(SEMSPEC.is_file(), "built semspec required")
class ReplaySpecificationTests(unittest.TestCase):
    def test_markdown_preserves_all_argv_hooks_and_sampling(self):
        import common

        config = common.command_definition(SOURCE)
        # Golden of the retired JSON plan, treating absent env as the same empty map.
        # This binds every argv, hook and timeout as well as the 0/60 sampling plan.
        canonical = json.dumps(config, sort_keys=True, separators=(",", ":")).encode()
        self.assertEqual(
            hashlib.sha256(canonical).hexdigest(),
            "4f71503989c2c06c00b216007470315ea8a998ff2ab2340787a7639e667abe3c",
        )
        self.assertEqual((config["warmup"], config["runs"]), (0, 60))
        self.assertEqual(
            list(config["commands"]),
            [
                "claude-code",
                "codex",
                "opencode",
                "mini-swe-agent",
                "openhands",
                "pi-agent",
                "swe-agent",
            ],
        )
        self.assertEqual(list(common.table_template(SOURCE)), list(config["commands"]))
        self.assertEqual(common.registered(SOURCE)["role"], "user-facing")
        self.assertFalse(SOURCE.with_suffix(".json").exists())
        self.assertFalse(SOURCE.with_suffix(".commands.json").exists())

    def test_new_clock_cannot_overwrite_historical_public_results(self):
        import common

        spec = importlib.util.spec_from_file_location("replay_documentation_gate", ROOT / "docs.py")
        docs = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(docs)
        inventory = json.loads((ROOT / "docs.json").read_text())
        self.assertEqual(
            inventory["bindings"]["replay/replay.md"]["metric_scope"],
            "direct subject command; hyperfine hooks and semspec excluded",
        )
        with tempfile.TemporaryDirectory() as temporary:
            batch = Path(temporary)
            (batch / "manifest.json").write_text(
                json.dumps(
                    dict(
                        entry="replay/replay.md",
                        benchmark_id="B-REPLAY",
                        role="user-facing",
                        metric_scope=common.METRIC_SCOPE,
                    )
                )
            )
            with patch.object(docs, "validate_batch") as validate:
                with self.assertRaisesRegex(ValueError, "measurement scope differs"):
                    docs.publish_docs(batch)
                validate.assert_not_called()


@unittest.skipUnless(BINARY.is_file() and SEMSPEC.is_file(), "built replay and semspec required")
class ReplayTests(unittest.TestCase):
    def test_all_native_prefixes_pass_and_tampering_is_rejected(self):
        with tempfile.TemporaryDirectory() as tmp:
            batch = Path(tmp) / ".data"
            batch.mkdir()
            (batch / "exports").mkdir()
            with patch.object(PREPARE, "create_batch", return_value=batch):
                PREPARE.prepare_batch(SOURCE, BINARY)
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
