"""Small demo tool: summarize decimal hours from a CSV file."""

import argparse
import csv
from decimal import Decimal
from pathlib import Path


def total_hours(path):
    """Sum the hours column from a CSV file."""
    with Path(path).open(newline="", encoding="utf-8") as source:
        hours = [Decimal(row["hours"]) for row in csv.DictReader(source)]
    return int(sum(hours))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("input", nargs="?", default="hours.csv")
    args = parser.parse_args()
    print(f"本周总工时：{total_hours(args.input):.1f} 小时")


if __name__ == "__main__":
    main()
