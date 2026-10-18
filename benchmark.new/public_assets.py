"""A small explicit inventory of public benchmark downloads.

Static tables and engineering details stay in Markdown or local evidence.
Only data needed by current publication contracts is exported to the site.
"""

from pathlib import Path

PUBLIC_CSV = frozenset(
    {
        "startup-timing.csv",
        "replay-semspec.csv",
        "filesystem-layered-references-levels.csv",
        "filesystem-layered-references-ready.csv",
        "filesystem-layered-references-failures.csv",
        "lazy-stage-summary.csv",
        "lazy-stage-comparisons.csv",
    }
)
PUBLIC_PROVENANCE = frozenset(
    {
        "startup-timing.provenance.json",
        "replay-semspec.provenance.json",
        "lazy-stage.provenance.json",
        "filesystem-layered-references/provenance.json",
    }
)
ARTICLE_RENDER_JSON = frozenset(
    {
        "startup-timing.ready.render.json",
        "startup-timing.exit.render.json",
        "measurement-coverage.render.json",
    }
)


def is_public_csv(filename):
    """Whether a dataset is needed by a current public measurement contract."""
    return str(filename) in PUBLIC_CSV


LOCALIZED_CSV = {"index.csv", "supervision-cost.csv"}
FILESYSTEM_SERIES = (
    "guest-writeback-tuning",
    "guest-writeback-cli",
    "guest-writeback",
    "layered-references",
    "layered",
    "workloads-v2",
    "io",
)
STATISTIC_SUFFIXES = (
    "-comparisons",
    "-provenance",
    "-summary",
    "-counters",
    "-owner-totals",
    "-rpc-summary",
    "-probe-phases",
    "-probes",
    "-cases",
    "-verification",
)


def csv_path(repository, filename, locale=None):
    """Return the single public CSV destination within a repository.

    Names must be plain CSV filenames. Translated historical CSVs require a
    locale and receive separate subdirectories; numeric CSVs ignore locale.
    This function does not create directories or modify measurement data.
    """
    filename = str(filename)
    if Path(filename).name != filename or not filename.endswith(".csv"):
        raise ValueError("public CSV requires a plain .csv filename")
    stem = Path(filename).stem
    if stem.startswith("filesystem"):
        study = stem.removeprefix("filesystem").removeprefix("-") or "tools"
        series = next(
            (s for s in FILESYSTEM_SERIES if study == s or study.startswith(s + "-")), None
        )
        if series is None:
            previous = None
            while previous != study:
                previous = study
                for suffix in STATISTIC_SUFFIXES:
                    study = study.removesuffix(suffix)
            series = study or "tools"
        group = Path("filesystem") / series
    else:
        group = Path(
            {"index": "overview", "measurement": "overview", "evidence": "overview"}.get(
                stem.split("-")[0], stem.split("-")[0]
            )
        )
    if filename in LOCALIZED_CSV:
        if locale not in ("zh", "en"):
            raise ValueError("translated CSV requires zh or en locale")
        group /= locale
    return Path(repository) / "docs/overrides/assets/benchmarks/data" / group / filename


def check_public_assets(repository):
    """Reject accidental publication of intermediate or duplicate evidence.

    Existing article and provenance checks verify the required files and their
    hashes. This check bounds the downloadable surface to its declared inputs.
    """
    repository = Path(repository)
    assets = repository / "docs/overrides/assets/benchmarks"
    for path in assets.rglob("*.csv"):
        if not is_public_csv(path.name) or path != csv_path(repository, path.name):
            raise ValueError(f"unneeded or duplicate public CSV: {path}")
    for path in assets.rglob("*.json"):
        relative = path.relative_to(assets).as_posix()
        if relative not in PUBLIC_PROVENANCE | {"replay-semspec.render.json"}:
            raise ValueError(f"unneeded public JSON: {path}")
    for path in assets.rglob("*.pdf"):
        raise ValueError(f"PDF belongs in local evidence: {path}")
    for locale in ("zh", "en"):
        articles = repository / "docs/src" / locale / "benchmarks"
        for path in articles.rglob("*"):
            if not path.is_file() or path.suffix not in {".csv", ".json", ".pdf"}:
                continue
            if path.parent == articles and path.name in ARTICLE_RENDER_JSON:
                continue
            raise ValueError(f"benchmark evidence belongs outside articles: {path}")
