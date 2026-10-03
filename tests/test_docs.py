"""Bilingual docs cannot silently lose a page or drift in examples/revisions."""

import runpy
from pathlib import Path

import pytest

pytest.importorskip("tomllib")  # Documentation builds require Python 3.11+.

ROOT = Path(__file__).resolve().parents[1]
check_translations = runpy.run_path(str(ROOT / "scripts/check-docs.py"))["check_translations"]
reference = runpy.run_path(str(ROOT / "scripts/check-reference.py"))


def test_reference_field_parser_honors_wire_names_and_skips():
    source = '''pub struct Example {
    #[serde(rename = "max_size")]
    pub budget: Option<u64>,
    #[serde(rename = "path")]
    #[serde(skip)]
    pub internal: String,
    #[serde(alias = "old")]
    pub current: Vec<String>,
}'''
    assert reference["fields"](source, "Example") == {
        "max_size": "Option<u64>", "current": "Vec<String>"
    }
    with pytest.raises(ValueError, match="unsupported field syntax"):
        reference["fields"](source.replace("pub budget", "budget"), "Example")


def test_config_reference_covers_serialized_fields():
    assert reference["check"]() >= 98


@pytest.mark.parametrize("mutation", ["missing", "type"])
def test_reference_check_rejects_incomplete_or_stale_tables(tmp_path, mutation):
    import shutil

    for source, _, _ in reference["GROUPS"]:
        target = tmp_path / source
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(ROOT / source, target)
    for locale in ("en", "zh"):
        target = tmp_path / f"docs/src/{locale}/reference/config.md"
        target.parent.mkdir(parents=True)
        text = (ROOT / target.relative_to(tmp_path)).read_text()
        if mutation == "missing":
            text = text.replace("| `vm.cpus` | `u16` |", "| `removed.cpus` | `u16` |")
        else:
            text = text.replace("| `vm.cpus` | `u16` |", "| `vm.cpus` | `u32` |")
        target.write_text(text)
    with pytest.raises(SystemExit, match="config reference drift"):
        reference["check"](tmp_path)


def test_bilingual_docs_guard(tmp_path):
    article = "---\nstatus: todo\nsearch:\n  exclude: true\n---\n# Title {#shared}\nS-DOC-001\n\n```sh\npvisor -- true\n```\n"
    for locale in ("en", "zh"):
        folder = tmp_path / "src" / locale
        folder.mkdir(parents=True)
        (folder / "guide.md").write_text(article)
    english = tmp_path / "src/en/guide.md"
    assert check_translations(tmp_path, record=True) == 1
    assert check_translations(tmp_path) == 1

    english.unlink()
    with pytest.raises(SystemExit, match="missing en translation"):
        check_translations(tmp_path, record=True)
    for old, new, message in (
        ("status: todo", "status: ready", "page status"),
        ("exclude: true", "exclude: false", "search exclusion"),
        ("{#shared}", "{#other}", "explicit anchors"),
        ("S-DOC-001", "S-DOC-002", "semantic case IDs"),
        ("pvisor -- true", "pvisor -- false", "executable examples"),
    ):
        english.write_text(article.replace(old, new))
        with pytest.raises(SystemExit, match=message):
            check_translations(tmp_path, record=True)
    english.write_text(article + "\nUpdated prose\n")
    assert check_translations(tmp_path) == 1  # pending revisions warn, not fail, locally
    with pytest.raises(SystemExit, match="bilingual revision changed"):
        check_translations(tmp_path, strict=True)
    assert check_translations(tmp_path, record=True) == 1
    assert check_translations(tmp_path) == 1


def test_bilingual_navigation_uses_matching_articles():
    import tomllib

    english_nav = runpy.run_path(str(ROOT / "scripts/build-docs.py"))["english_nav"]
    nav = [
        {"指南": ["zh/guides/index.md", {"并行 Agent（规划中）": "zh/guides/parallel-agents.md"}]}
    ]
    translated = tomllib.loads("nav = " + english_nav(nav))["nav"]
    assert translated == [
        {
            "Guides": [
                "en/guides/index.md",
                {"Parallel agents (planned)": "en/guides/parallel-agents.md"},
            ]
        }
    ]


def test_native_search_stays_in_its_locale(tmp_path):
    import json

    localize_search = runpy.run_path(str(ROOT / "scripts/build-docs.py"))["localize_search"]
    page = tmp_path / "en/guides/index.html"
    page.parent.mkdir(parents=True)
    page.write_text('<script id="__config" type="application/json">{"base":"../.."}</script>')
    (tmp_path / "search.json").write_text(
        json.dumps(
            {
                "config": {"lang": ["en"]},
                "items": [{"location": "en/guides/#example"}, {"location": "zh/guides/"}],
            }
        )
    )
    localize_search(tmp_path, "en")
    assert json.loads((tmp_path / "en/search.json").read_text())["items"] == [
        {"location": "guides/#example"}
    ]
    assert '"base": ".."' in page.read_text()
