#!/usr/bin/env python3
"""Check generated docs links, images, locale navigation and Markdown rendering."""

import json
from html.parser import HTMLParser
from pathlib import Path
from urllib.parse import unquote, urlsplit

import tomllib

ROOT = Path(__file__).resolve().parents[1] / "docs/site"


class Page(HTMLParser):
    def __init__(self, path):
        super().__init__(convert_charrefs=True)
        self.ids, self.links, self.language = set(), [], ""
        self.feed(path.read_text())

    def handle_starttag(self, tag, attrs):
        attrs = dict(attrs)
        if "id" in attrs:
            self.ids.add(attrs["id"])
        if tag == "html":
            self.language = attrs.get("lang", "")
        for key in ("href", "src"):
            if key in attrs:
                self.links.append((tag, attrs[key], attrs.get("class", "")))


def check():
    paths = list(ROOT.rglob("*.html"))
    pages = {p.resolve(): Page(p) for p in paths}
    issues = []
    source = ROOT.parent / "src"
    en = {p.relative_to(source / "en") for p in (source / "en").rglob("*.md")}
    zh = {p.relative_to(source / "zh") for p in (source / "zh").rglob("*.md")}
    for missing in sorted(en ^ zh):
        issues.append(f"locale page is not paired: {missing}")

    def nav_pages(node):
        if isinstance(node, str):
            yield node
        elif isinstance(node, list):
            for child in node:
                yield from nav_pages(child)
        elif isinstance(node, dict):
            for child in node.values():
                yield from nav_pages(child)

    config = tomllib.loads((ROOT.parent / "zensical.toml").read_text())
    navigation = list(nav_pages(config["project"]["nav"]))
    for page in navigation:
        if not (source / page).is_file():
            issues.append(f"navigation page missing: {page}")
    if len(navigation) != len(set(navigation)):
        issues.append("navigation lists the same article more than once")
    for page in en:
        if f"en/{page}" not in navigation:
            issues.append(f"article missing from navigation: en/{page}")
    for old, new in json.loads((ROOT.parent / "redirects.json").read_text()).items():
        for locale in ("en", "zh"):
            if not (source / locale / new).is_file():
                issues.append(f"redirect target missing: {locale}/{old} -> {new}")
    for path, page in pages.items():
        rel = path.relative_to(ROOT)
        locale = rel.parts[0] if rel.parts[0] in ("en", "zh") else None
        if locale and page.language != locale:
            issues.append(f"{rel}: html lang={page.language}, expected {locale}")
        for tag, href, classes in page.links:
            url = urlsplit(href)
            if url.scheme or url.netloc:
                continue
            target = unquote(url.path)
            if target.startswith("/Persisting/"):
                target = target[len("/Persisting") :]
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
            if locale and "md-select__link" in classes and dest.is_relative_to(ROOT):
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
                    "md-tabs__link" in classes or "md-nav__link" in classes or "md-logo" in classes
                )
                and dest.is_relative_to(ROOT)
            ):
                dest_locale = dest.relative_to(ROOT).parts[0]
                if dest_locale in ("en", "zh") and dest_locale != locale:
                    issues.append(f"{rel}: navigation changes language: {href}")
    for locale in ("en", "zh"):
        product = (ROOT / locale / "start/what-is-pvisor/index.html").read_text()
        if 'class="admonition tip"' not in product or "<pre" not in product:
            issues.append(f"{locale}/start/what-is-pvisor: missing rendered callout or code block")
    if issues:
        raise SystemExit("\n".join(sorted(set(issues))))
    print(
        f"Checked {len(pages)} HTML pages: local links, anchors, images, language navigation, callouts and code blocks passed."
    )


if __name__ == "__main__":
    check()
