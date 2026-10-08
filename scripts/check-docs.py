#!/usr/bin/env python3
"""Check generated docs links, images, locale navigation and Markdown rendering."""

import hashlib
import json
import re
import sys
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urljoin, urlsplit

import tomllib

ROOT = Path(__file__).resolve().parents[1] / "docs/site"
FENCES = re.compile(r"(?m)^([ \t]*)(`{3,}|~{3,})([^\n]*)\n((?s:.*?))^\1\2[ \t]*$")


def check_translations(docs=ROOT.parent, record=False, strict=False):
    """Pin reviewed pairs so a later one-language edit cannot pass unnoticed.

    Structural disagreements (a missing page, divergent anchors, examples, IDs,
    status or search exclusion) are always fatal. Pending revisions are a warning
    by default so a local build and serve can run while the pairs are reviewed;
    pass ``strict=True`` (CLI ``--require-recorded``) to make them fatal in CI.
    """
    source = docs / "src"
    locales = {
        locale: {p.relative_to(source / locale) for p in (source / locale).rglob("*.md") if ".data" not in p.parts}
        for locale in ("en", "zh")
    }
    issues, revisions = [], {}
    for locale, other in (("en", "zh"), ("zh", "en")):
        for missing in sorted(locales[other] - locales[locale]):
            issues.append(f"missing {locale} translation: {missing}")
    for path in sorted(locales["en"] & locales["zh"]):
        zh, en = ((source / locale / path).read_text() for locale in ("zh", "en"))
        for pattern, label in ((r"^status: (\w+)$", "page status"),
                               (r"^  exclude: (true|false)$", "search exclusion"),
                               (r"\{#([^}]+)\}", "explicit anchors"),
                               (r"\bS-(?:DOC|STAGE|USE)-\d+\b", "semantic case IDs")):
            if set(re.findall(pattern, zh, re.M)) != set(re.findall(pattern, en, re.M)):
                issues.append(f"{path}: translations disagree on {label}")
        def examples(text):
            return [(m[3].strip(), m[4]) for m in FENCES.finditer(text)
                    if m[3].strip() not in ("text", "markdown", "md")]

        if examples(zh) != examples(en):
            issues.append(f"{path}: translations disagree on executable examples")
        revisions[str(path)] = hashlib.sha256((zh + "\0" + en).encode()).hexdigest()
    manifest = docs / "translations.json"
    pending = []
    if record:
        if issues:
            raise SystemExit("\n".join(issues))
        manifest.write_text(json.dumps(revisions, indent=2, ensure_ascii=False) + "\n")
    else:
        recorded = json.loads(manifest.read_text()) if manifest.exists() else {}
        for path in sorted(recorded.keys() | revisions.keys()):
            if recorded.get(path) != revisions.get(path):
                pending.append(path)
    if issues:
        raise SystemExit("\n".join(issues))
    if pending:
        if strict:
            raise SystemExit("\n".join(
                f"{path}: bilingual revision changed; review both languages and record translations"
                for path in pending
            ))
        print(
            f"{len(pending)} bilingual pair(s) changed and await review; continuing. "
            "After reviewing both languages run: python3 scripts/check-docs.py --record-translations"
        )
    return len(revisions)


class Page(HTMLParser):
    def __init__(self, path):
        super().__init__(convert_charrefs=True)
        self.ids, self.links, self.language = set(), [], ""
        self.redirect = False
        self.feed(path.read_text())

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if "id" in attrs:
            self.ids.add(attrs["id"])
        if tag == "html":
            self.language = attrs.get("lang", "")
        if tag == "meta" and attrs.get("http-equiv", "").lower() == "refresh":
            self.redirect = True
        for key in ("href", "src"):
            if key in attrs:
                self.links.append((tag, attrs[key], attrs.get("class", "")))


def check(strict=False):
    pairs = check_translations(strict=strict)
    paths = list(ROOT.rglob("*.html"))
    pages = {p.resolve(): Page(p) for p in paths}
    issues = []
    source = ROOT.parent / "src"

    def nav_pages(node):
        if isinstance(node, str):
            yield node
        elif isinstance(node, list):
            for child in node:
                yield from nav_pages(child)
        elif isinstance(node, dict):
            for child in node.values():
                yield from nav_pages(child)

    def has_redirect_maps(node):
        if isinstance(node, dict):
            return "redirect_maps" in node or any(has_redirect_maps(value) for value in node.values())
        if isinstance(node, list):
            return any(has_redirect_maps(value) for value in node)
        return False

    configs = {
        locale: tomllib.loads((ROOT.parent / f"zensical.{locale}.toml").read_text())["project"]
        for locale in ("en", "zh")
    }
    if {path.name for path in ROOT.parent.glob("zensical*.toml")} != {
        "zensical.en.toml", "zensical.zh.toml"
    }:
        issues.append("only the two native locale configurations are allowed")
    navigation = {}
    published_sites = {}
    for locale, config in configs.items():
        locale_source = ROOT.parent / config["docs_dir"]
        navigation[locale] = list(nav_pages(config["nav"]))
        for page in navigation[locale]:
            if page.startswith("/") or urlsplit(page).scheme:
                continue
            if not (locale_source / page).is_file():
                issues.append(f"{locale}: navigation page missing: {page}")
        if len(navigation[locale]) != len(set(navigation[locale])):
            issues.append(f"{locale}: navigation lists the same article more than once")
        if "redirects" in config.get("plugins", {}) or has_redirect_maps(config):
            issues.append(f"{locale}: legacy redirect plugin is forbidden")
        published_sites[locale] = urlsplit(config["site_url"])
        url = published_sites[locale]
        if url.scheme not in ("http", "https") or not url.netloc or not url.path.endswith(f"/{locale}/"):
            issues.append(f"{locale}: site_url must be an absolute published locale URL")
    if navigation["en"] != navigation["zh"]:
        issues.append("locale navigation article paths do not match")
    published = published_sites["en"]._replace(path=published_sites["en"].path.removesuffix("en/"))
    for locale, config in configs.items():
        if published_sites[locale] != published._replace(path=published.path + locale + "/"):
            issues.append(f"{locale}: site_url does not share the published site root")
        if {alt["lang"]: alt["link"] for alt in config["extra"]["alternate"]} != {
            lang: published._replace(path=published.path + lang + "/").geturl()
            for lang in ("en", "zh")
        }:
            issues.append(f"{locale}: alternate links must use absolute published locale URLs")
    entry = ROOT / "index.html"
    entry_source = ROOT.parent / "index.html"
    if not entry_source.is_file() or not entry.is_file() or entry.read_bytes() != entry_source.read_bytes():
        issues.append("root index.html must be copied from docs/index.html")
    if entry.resolve() in pages:
        entry_links = {
            urljoin(published.geturl(), href)
            for tag, href, _ in pages[entry.resolve()].links if tag == "a"
        }
        for locale in ("en", "zh"):
            if published._replace(path=published.path + locale + "/").geturl() not in entry_links:
                issues.append(f"root index.html: missing current {locale} language entry")
    for path, page in pages.items():
        if page.redirect:
            issues.append(f"{path.relative_to(ROOT)}: meta refresh redirect is forbidden")
    article_paths = set()
    for locale in ("en", "zh"):
        for article in (source / locale).rglob("*.md"):
            if ".data" in article.parts:
                continue
            relative = article.relative_to(source).with_suffix("")
            target = ROOT / (relative.parent if relative.name == "index" else relative) / "index.html"
            article_paths.add(target.resolve())
            if target.resolve() not in pages:
                issues.append(f"article not rendered: {article.relative_to(source)}")
    if any(".data" in path.parts for path in ROOT.rglob("*")):
        issues.append("raw .data evidence included in generated site")
    if issues:
        raise SystemExit("\n".join(sorted(set(issues))))

    for locale in ("en", "zh"):
        index = json.loads((ROOT / locale / "search.json").read_text())
        if index["config"]["lang"] != [locale] or not index["items"]:
            issues.append(f"{locale}: missing native locale search index")
        for item in index["items"]:
            url = urlsplit(item["location"])
            dest = (ROOT / locale / unquote(url.path) / "index.html").resolve()
            if not dest.is_relative_to(ROOT / locale) or dest not in pages:
                issues.append(f"{locale}: search result leaves locale: {item['location']}")
            elif url.fragment and unquote(url.fragment) not in pages[dest].ids:
                issues.append(f"{locale}: search result anchor missing: {item['location']}")
    for path, page in pages.items():
        rel = path.relative_to(ROOT)
        locale = rel.parts[0] if rel.parts[0] in ("en", "zh") else None
        if locale and page.language != locale:
            issues.append(f"{rel}: html lang={page.language}, expected {locale}")
        for tag, href, classes in page.links:
            url = urlsplit(href)
            if "md-select__link" in classes and not (url.scheme and url.netloc):
                issues.append(f"{rel}: language selector must use an absolute published URL: {href}")
            if url.scheme or url.netloc:
                # Language links use the published origin so single-locale previews
                # can switch languages. Still validate their generated counterparts.
                if "md-select__link" not in classes:
                    continue
                if url.scheme != published.scheme or url.netloc != published.netloc or not url.path.startswith(published.path):
                    issues.append(f"{rel}: language selector leaves published site: {href}")
                    continue
            target = unquote(url.path)
            if target.startswith(published.path):
                target = "/" + target[len(published.path) :]
            dest = (
                (
                    (ROOT / target.lstrip("/"))
                    if target.startswith("/")
                    else (path.parent / target)
                ).resolve()
                if target
                else path
            )
            if dest.is_dir():
                dest /= "index.html"
            # A 404 has no matching article and returns to the language homepage.
            if path in article_paths and "md-select__link" in classes and dest.is_relative_to(ROOT):
                target_rel = dest.relative_to(ROOT)
                if target_rel.parts[1:] != rel.parts[1:]:
                    issues.append(f"{rel}: language selector loses current article: {href}")
            if not dest.exists():
                issues.append(f"{rel}: missing {tag} {href}")
            elif url.fragment and dest in pages and unquote(url.fragment) not in pages[dest].ids:
                issues.append(f"{rel}: missing anchor {href}")
            if (
                locale
                and (
                    "md-tabs__link" in classes or "md-nav__link" in classes
                    or "md-logo" in classes or "md-path__link" in classes
                )
                and dest.is_relative_to(ROOT)
            ):
                dest_locale = dest.relative_to(ROOT).parts[0]
                if dest_locale in ("en", "zh") and dest_locale != locale:
                    issues.append(f"{rel}: navigation changes language: {href}")
                if "md-path__link" in classes and dest_locale != locale:
                    issues.append(f"{rel}: breadcrumb leaves current language: {href}")
    for locale in ("en", "zh"):
        product = (ROOT / locale / "start/what-is-pvisor/index.html").read_text()
        if 'class="admonition tip"' not in product or "<pre" not in product:
            issues.append(f"{locale}/start/what-is-pvisor: missing rendered callout or code block")
    if issues:
        raise SystemExit("\n".join(sorted(set(issues))))
    print(
        f"Checked {pairs} bilingual pairs and {len(pages)} HTML pages: local links, anchors, images, language navigation/search, callouts and code blocks passed."
    )


if __name__ == "__main__":
    if sys.argv[1:] == ["--record-translations"]:
        print(f"Recorded {check_translations(record=True)} reviewed bilingual pairs.")
    elif sys.argv[1:] == ["--require-recorded"]:
        check(strict=True)
    elif sys.argv[1:]:
        raise SystemExit("usage: check-docs.py [--record-translations | --require-recorded]")
    else:
        check()
