"""Bilingual docs cannot silently lose a page or drift in examples/revisions."""

import runpy
from pathlib import Path

import pytest

pytest.importorskip("tomllib")  # Documentation builds require Python 3.11+.

ROOT = Path(__file__).resolve().parents[1]
check_translations = runpy.run_path(str(ROOT / "scripts/check-docs.py"))["check_translations"]


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
