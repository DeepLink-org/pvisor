import json
import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

from v1.replay import trajectory, validate_prefix


class ReplayFidelityTests(unittest.TestCase):
    def test_native_prefix_checks_source_observation_and_boundary(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            (tmp_path / "native").mkdir()
            path = tmp_path / "native/prepared-prefix.jsonl"
            manifest = dict(
                agent=dict(name="codex"),
                source=dict(sha256="source"),
                boundary=dict(after_step=1, tool_calls=1, complete_tool_batch=True),
                batches=[dict(tool_calls=[dict(arguments=dict(cmd="first"))])],
            )
            good = [
                dict(arguments=json.dumps(dict(cmd="first"))),
                dict(output="historical observation"),
            ]
            path.write_text("\n".join(json.dumps(x) for x in good))
            validate_prefix(manifest, tmp_path, "codex", "first", "second", "source")
            for events in (good[:1], good + [dict(cmd="second")]):
                path.write_text("\n".join(json.dumps(x) for x in events))
                with self.assertRaisesRegex(ValueError, "native prefix"):
                    validate_prefix(manifest, tmp_path, "codex", "first", "second", "source")
            path.write_text("\n".join(json.dumps(x) for x in good))
            with self.assertRaisesRegex(ValueError, "source"):
                validate_prefix(manifest, tmp_path, "codex", "first", "second", "changed")

    def test_swe_fixture_has_two_original_batches_and_tool_observations(self):
        source, jsonl = trajectory("swe-agent", 2, "first")
        assert not jsonl and len(source["trajectory"]) == 2
        assert source["trajectory"][0]["action"] == "first"
        assert source["trajectory"][0]["observation"] == "historical observation"
