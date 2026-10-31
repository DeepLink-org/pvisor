"""Only stop VM units with matching output ownership in this prepared condition."""

from concurrent.futures import ThreadPoolExecutor

from arrival_batch import stop_owned
from arrival_common import ROOT, save


def main():
    jobs = [p.parent for p in (ROOT / "jobs").glob("*/owned-unit.json")]
    with ThreadPoolExecutor(max_workers=8) as pool:
        list(pool.map(stop_owned, jobs))
    save(ROOT / "cleanup-receipt.json", {"jobs": [p.name for p in jobs]})


if __name__ == "__main__":
    main()
