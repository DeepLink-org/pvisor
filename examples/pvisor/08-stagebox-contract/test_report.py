"""Existing tests, including the user's report of lost fractional hours."""

from decimal import Decimal
from pathlib import Path
import tempfile
import unittest

from weekly_report import total_hours


def report_total(rows):
    with tempfile.TemporaryDirectory() as directory:
        source = Path(directory) / "hours.csv"
        source.write_text("day,hours\n" + rows, encoding="utf-8")
        return total_hours(source)


class ReportTests(unittest.TestCase):
    def test_whole_hours(self):
        self.assertEqual(report_total("Mon,1\nTue,2\n"), Decimal("3"))

    def test_fractional_hours(self):
        self.assertEqual(report_total("Mon,1.5\nTue,2.0\n"), Decimal("3.5"))
