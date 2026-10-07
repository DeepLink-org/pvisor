"""Bilingual docs cannot silently lose a page or drift in examples/revisions."""

import runpy
from pathlib import Path

import pytest

pytest.importorskip("tomllib")  # Documentation builds require Python 3.11+.

ROOT = Path(__file__).resolve().parents[1]
check_translations = runpy.run_path(str(ROOT / "scripts/check-docs.py"))["check_translations"]
reference = runpy.run_path(str(ROOT / "scripts/check-reference.py"))


def test_reference_field_parser_honors_wire_names_and_skips():
    source = """pub struct Example {
    #[serde(rename = "max_size")]
    pub budget: Option<u64>,
    #[serde(rename = "path")]
    #[serde(skip)]
    pub internal: String,
    #[serde(alias = "old")]
    pub current: Vec<String>,
}"""
    assert reference["fields"](source, "Example") == {
        "max_size": "Option<u64>",
        "current": "Vec<String>",
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


@pytest.mark.parametrize("path", ["docs/src/assets", "docs/src/stylesheets"])
def test_asset_migration_keeps_old_git_paths_free_of_symlinks(path):
    # Editors stage deleted tracked files individually; Git refuses symlink parents.
    assert not (ROOT / path).is_symlink()
    assert (ROOT / "docs/overrides/assets/examples/json/run-bundle.json").is_file()
    assert (ROOT / "docs/overrides/assets/stylesheets/extra.css").is_file()


def test_native_locale_configs_keep_matching_navigation_and_redirects():
    import tomllib

    def pages(node):
        if isinstance(node, str):
            return [node]
        if isinstance(node, list):
            return [path for child in node for path in pages(child)]
        return [path for child in node.values() for path in pages(child)]

    configs = {
        locale: tomllib.loads((ROOT / f"docs/zensical.{locale}.toml").read_text())["project"]
        for locale in ("zh", "en")
    }
    canonical = tomllib.loads((ROOT / "docs/zensical.toml").read_text())["project"]
    expected_sections = {
        "zh": ["首页", "快速开始", "用户指南", "基础概念", "基准测试", "设计与研究", "参与开发"],
        "en": ["Home", "Quick start", "User guide", "Core concepts", "Benchmarks", "Design and research", "Contributing"],
    }
    for locale, config in configs.items():
        assert [label for section in config["nav"] for label in section] == expected_sections[locale]
        assert config["nav"][0][expected_sections[locale][0]] == "index.md"
        for section in config["nav"][1:]:
            for groups in section.values():
                for group in groups:
                    for entries in group.values():
                        assert isinstance(entries, list)
                        assert entries
                        assert all(isinstance(path, str) for entry in entries for path in entry.values())
        concepts = config["nav"][3][expected_sections[locale][3]]
        security_label = "安全" if locale == "zh" else "Security"
        security = next(section[security_label] for section in concepts if security_label in section)
        assert "security/index.md" in pages(security)
    assert pages(configs["zh"]["nav"]) == pages(configs["en"]["nav"])
    for locale, config in configs.items():
        assert config["docs_dir"] == f"src/{locale}"
        assert config["site_dir"] == f"site/{locale}"
        assert config["theme"]["language"] == locale
        assert config["plugins"]["search"]["lang"] == [locale]
        assert {alt["lang"]: alt["link"] for alt in config["extra"]["alternate"]} == {
            lang: canonical["site_url"] + lang + "/" for lang in ("en", "zh")
        }
        for path in pages(config["nav"]):
            assert (ROOT / "docs" / config["docs_dir"] / path).is_file()
        redirects = config["plugins"]["redirects"]["redirect_maps"]
        for source, target in redirects.items():
            assert (ROOT / "docs" / config["docs_dir"] / target).is_file()
    assert configs["zh"]["plugins"]["redirects"] == configs["en"]["plugins"]["redirects"]
    legacy = canonical["plugins"]["redirects"]["redirect_maps"]
    chinese = configs["zh"]["plugins"]["redirects"]["redirect_maps"]
    assert legacy.keys() == chinese.keys()
    for source, target in chinese.items():
        route = Path(target).with_suffix("")
        route = route.parent if route.name == "index" else route
        assert legacy[source] == canonical["site_url"] + "zh/" + route.as_posix() + "/"


def test_generated_site_check_rejects_success_without_articles(tmp_path, monkeypatch):
    for locale in ("zh", "en"):
        source = tmp_path / "src" / locale
        source.mkdir(parents=True)
        (source / "index.md").write_text("# Example\n")
    (tmp_path / "zensical.toml").write_text(
        '[project]\nnav = ["zh/index.md"]\n'
        '[project.plugins.redirects.redirect_maps]\n'
    )
    site = tmp_path / "site"
    site.mkdir()
    checker = runpy.run_path(str(ROOT / "scripts/check-docs.py"))["check"]
    monkeypatch.setitem(checker.__globals__, "ROOT", site)
    monkeypatch.setitem(
        checker.__globals__, "check_translations",
        lambda **kwargs: check_translations(tmp_path, **kwargs),
    )
    with pytest.raises(SystemExit, match="article not rendered: en/index.md"):
        checker()
