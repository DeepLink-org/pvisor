import tempfile
import unittest
from contextlib import ExitStack
from pathlib import Path

from v1.supervision import validate_review, validate_target


class SupervisionTests(unittest.TestCase):
    def test_review_requires_both_sides_of_every_content_change(self):
        text = "".join(f"files/f{i:06d}\n-old-{i}\n+new-{i}\n" for i in range(20))
        validate_review(text)
        for fragment in ("files/f000019", "-old-19\n", "+new-19\n"):
            with self.assertRaises(ValueError):
                validate_review(text.replace(fragment, ""))

    def test_selective_result_rejects_applying_unselected_file(self):
        with ExitStack() as resources:
            tmp_path = Path(
                resources.enter_context(tempfile.TemporaryDirectory(prefix="pvisor-test-"))
            ).resolve()
            (tmp_path / "files").mkdir()
            for i in range(20):
                (tmp_path / "files" / f"f{i:06d}").write_text(f"{'new' if i < 10 else 'old'}-{i}\n")
            validate_target(tmp_path)
            (tmp_path / "files/f000019").write_text("new-19\n")
            with self.assertRaises(ValueError):
                validate_target(tmp_path)
