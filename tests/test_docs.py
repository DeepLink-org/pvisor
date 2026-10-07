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


def test_native_locale_configs_keep_matching_navigation_without_legacy_redirects():
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
    assert {path.name for path in (ROOT / "docs").glob("zensical*.toml")} == {
        "zensical.zh.toml", "zensical.en.toml"
    }
    published_root = "https://deeplink-org.github.io/pvisor/"
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
        assert config["site_url"] == published_root + locale + "/"
        assert config["theme"]["language"] == locale
        assert config["plugins"]["search"]["lang"] == [locale]
        assert {alt["lang"]: alt["link"] for alt in config["extra"]["alternate"]} == {
            lang: published_root + lang + "/" for lang in ("en", "zh")
        }
        navigation = pages(config["nav"])
        assert len(navigation) == len(set(navigation))
        for path in navigation:
            assert (ROOT / "docs" / config["docs_dir"] / path).is_file()
        assert "redirects" not in config["plugins"]
        assert "redirect_maps" not in (ROOT / f"docs/zensical.{locale}.toml").read_text()


def test_root_current_language_entry():
    entry = ROOT / "docs/index.html"
    parser = runpy.run_path(str(ROOT / "scripts/check-docs.py"))["Page"](entry)
    assert not parser.redirect
    from urllib.parse import urljoin

    published_root = "https://deeplink-org.github.io/pvisor/"
    links = {urljoin(published_root, href) for tag, href, _ in parser.links if tag == "a"}
    assert {published_root + locale + "/" for locale in ("en", "zh")} <= links


def test_generated_site_check_rejects_success_without_articles(tmp_path, monkeypatch):
    for locale in ("zh", "en"):
        source = tmp_path / "src" / locale
        source.mkdir(parents=True)
        (source / "index.md").write_text("# Example\n")
    write_locale_configs(tmp_path)
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


def write_locale_configs(docs):
    for locale in ("en", "zh"):
        (docs / f"zensical.{locale}.toml").write_text(
            f'[project]\ndocs_dir = "src/{locale}"\nsite_dir = "site/{locale}"\n'
            f'site_url = "https://example.org/pvisor/{locale}/"\n'
            'nav = ["index.md", "start/what-is-pvisor.md"]\n'
            '[project.extra]\nalternate = [\n'
            '{lang = "en", link = "https://example.org/pvisor/en/"},\n'
            '{lang = "zh", link = "https://example.org/pvisor/zh/"}]\n'
        )


@pytest.fixture
def generated_site(tmp_path, monkeypatch):
    import json

    write_locale_configs(tmp_path)
    site = tmp_path / "site"
    site.mkdir()
    entry = '<html lang="en"><a href="zh/">中文</a><a href="en/">English</a></html>'
    (tmp_path / "index.html").write_text(entry)
    (site / "index.html").write_text(entry)
    for locale in ("en", "zh"):
        for route in ("", "start/what-is-pvisor/"):
            source = tmp_path / "src" / locale / ("index.md" if not route else "start/what-is-pvisor.md")
            source.parent.mkdir(parents=True, exist_ok=True)
            source.write_text("# Example {#shared}\n")
            dest = site / locale / route / "index.html"
            dest.parent.mkdir(parents=True, exist_ok=True)
            dest.write_text(
                f'<html lang="{locale}"><h1 id="shared">Example</h1>'
                '<div class="admonition tip">Tip</div><pre>example</pre>'
                + ''.join(
                    f'<a class="md-select__link" href="https://example.org/pvisor/{lang}/{route}">{lang}</a>'
                    for lang in ("en", "zh")
                )
                + f'<a class="md-nav__link" href="/pvisor/{locale}/">Home</a></html>'
            )
        (site / locale / "search.json").write_text(json.dumps({
            "config": {"lang": [locale]},
            "items": [{"location": "#shared"}, {"location": "start/what-is-pvisor/#shared"}],
        }))
    checker = runpy.run_path(str(ROOT / "scripts/check-docs.py"))["check"]
    monkeypatch.setitem(checker.__globals__, "ROOT", site)
    monkeypatch.setitem(
        checker.__globals__, "check_translations",
        lambda **kwargs: check_translations(tmp_path, **kwargs),
    )
    return tmp_path, checker


def test_generated_site_accepts_only_locale_configs_and_copied_current_entry(generated_site):
    docs, checker = generated_site
    assert not (docs / "zensical.toml").exists()
    checker()


def test_generated_site_derives_published_root_from_locale_urls(generated_site):
    docs, checker = generated_site
    for config in docs.glob("zensical.*.toml"):
        config.write_text(config.read_text().replace("example.org/pvisor/", "docs.example.net/manual/"))
    for page in (docs / "site").rglob("*.html"):
        page.write_text(page.read_text().replace("example.org/pvisor/", "docs.example.net/manual/").replace("/pvisor/", "/manual/"))
    checker()


@pytest.mark.parametrize("mutation,message", [
    ("root_config", "only the two native locale configurations"),
    ("missing_nav", "en: navigation page missing"),
    ("duplicate_nav", "en: navigation lists the same article more than once"),
    ("mismatched_nav", "locale navigation article paths do not match"),
    ("redirect_plugin", "legacy redirect plugin is forbidden"),
    ("redirect_maps", "legacy redirect plugin is forbidden"),
    ("redirect_page", "meta refresh redirect is forbidden"),
    ("root_redirect", "meta refresh redirect is forbidden"),
    ("root_not_copied", "root index.html must be copied"),
    ("root_old_entry", "missing current zh language entry"),
    ("site_url", "site_url must be an absolute published locale URL"),
    ("alternate", "alternate links must use absolute published locale URLs"),
    ("raw_data", "raw .data evidence included"),
    ("language", "html lang=zh, expected en"),
    ("search_language", "missing native locale search index"),
    ("search_target", "search result leaves locale"),
    ("search_anchor", "search result anchor missing"),
    ("selector_origin", "language selector leaves published site"),
    ("selector_relative", "language selector must use an absolute published URL"),
    ("selector_article", "language selector loses current article"),
    ("navigation_locale", "navigation changes language"),
    ("breadcrumb_locale", "breadcrumb leaves current language"),
    ("missing_link", "missing a missing/"),
    ("missing_anchor", "missing anchor #missing"),
    ("callout", "missing rendered callout or code block"),
])
def test_generated_site_rejects_invalid_native_output(generated_site, mutation, message):
    import json

    docs, checker = generated_site
    config = docs / "zensical.en.toml"
    article = docs / "site/en/start/what-is-pvisor/index.html"
    entry = docs / "site/index.html"

    def replace(path, old, new):
        text = path.read_text()
        assert old in text
        path.write_text(text.replace(old, new))

    if mutation == "root_config":
        (docs / "zensical.toml").write_text("[project]\n")
    elif mutation in ("missing_nav", "duplicate_nav", "mismatched_nav"):
        target = {"missing_nav": "missing.md", "duplicate_nav": "index.md", "mismatched_nav": "extra.md"}[mutation]
        if mutation == "mismatched_nav":
            (docs / "src/en/extra.md").write_text("# Example\n")
            (docs / "src/zh/extra.md").write_text("# Example\n")
        replace(config, '"start/what-is-pvisor.md"]', f'"start/what-is-pvisor.md", "{target}"]')
    elif mutation in ("redirect_plugin", "redirect_maps"):
        table = "project.plugins.redirects.redirect_maps" if mutation == "redirect_plugin" else "project.redirect_maps"
        with config.open("a") as stream:
            stream.write(f'\n[{table}]\n"old.md" = "index.md"\n')
    elif mutation in ("redirect_page", "root_redirect"):
        page = article if mutation == "redirect_page" else entry
        with page.open("a") as stream:
            stream.write('<meta http-equiv="Refresh" content="0; url=en/">')
        if mutation == "root_redirect":
            (docs / "index.html").write_bytes(entry.read_bytes())
    elif mutation == "root_not_copied":
        entry.write_text(entry.read_text() + "\n")
    elif mutation == "root_old_entry":
        replace(entry, 'href="zh/"', 'href="legacy/"')
        (docs / "index.html").write_bytes(entry.read_bytes())
    elif mutation == "site_url":
        replace(config, 'site_url = "https://example.org/pvisor/en/"', 'site_url = "/pvisor/en/"')
    elif mutation == "alternate":
        replace(config, 'link = "https://example.org/pvisor/zh/"', 'link = "/pvisor/zh/"')
    elif mutation == "raw_data":
        (docs / "site/en/.data").mkdir()
    elif mutation == "language":
        replace(article, 'lang="en"', 'lang="zh"')
    elif mutation.startswith("search_"):
        search = docs / "site/en/search.json"
        index = json.loads(search.read_text())
        if mutation == "search_language":
            index["config"]["lang"] = ["zh"]
        else:
            index["items"][0]["location"] = "../zh/" if mutation == "search_target" else "#missing"
        search.write_text(json.dumps(index))
    elif mutation.startswith("selector_"):
        old = 'href="https://example.org/pvisor/zh/start/what-is-pvisor/"'
        new = {
            "selector_origin": 'href="https://elsewhere.org/pvisor/zh/start/what-is-pvisor/"',
            "selector_relative": 'href="/pvisor/zh/start/what-is-pvisor/"',
            "selector_article": 'href="https://example.org/pvisor/zh/"',
        }[mutation]
        replace(article, old, new)
    elif mutation == "navigation_locale":
        replace(article, 'href="/pvisor/en/"', 'href="/pvisor/zh/"')
    elif mutation == "breadcrumb_locale":
        replace(article, 'class="md-nav__link" href="/pvisor/en/"', 'class="md-path__link" href="/pvisor/"')
    elif mutation in ("missing_link", "missing_anchor"):
        with article.open("a") as stream:
            stream.write('<a href="' + ("missing/" if mutation == "missing_link" else "#missing") + '">Broken</a>')
    elif mutation == "callout":
        replace(article, 'class="admonition tip"', 'class="admonition note"')
    else:
        raise AssertionError(mutation)
    with pytest.raises(SystemExit, match=message):
        checker()
