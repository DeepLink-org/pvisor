#!/usr/bin/env python3
"""Build canonical Chinese docs and the smaller English entry points."""

import json
import os
import shutil
import subprocess
import sys
import tempfile
from html import escape
from pathlib import Path

DOCS = Path(__file__).resolve().parents[1] / "docs"


def build() -> None:
    zensical = shutil.which("zensical") or str(Path(sys.executable).with_name("zensical"))
    subprocess.run([zensical, "build", "--strict"], cwd=DOCS, check=True)
    # Zensical has one canonical language/navigation per build. Reuse the same
    # content and search index, rendering English HTML with its native en theme.
    config = (DOCS / "zensical.toml").read_text()
    before, nav = config.split("nav = [", 1)
    nav, after = nav.split("\n[project.theme]", 1)
    nav = '\n  { "Home" = "en/index.md" },\n  { "Get started" = ["en/start/index.md", "en/start/what-is-pvisor.md", "en/start/installation.md", "en/start/first-run.md"] },\n  { "CLI reference" = "en/reference/cli.md" },\n]\n'
    config = before + "nav = [" + nav + "\n[project.theme]" + after
    config = config.replace('language = "zh"', 'language = "en"').replace(
        'homepage = "zh/"', 'homepage = "en/"'
    )
    with tempfile.TemporaryDirectory(prefix=".zensical-en-", dir=DOCS) as temp:
        temp = Path(temp)
        config = config.replace('site_dir = "site"', f'site_dir = "{temp.name}/site"')
        config = config.replace("[project]\n", f'[project]\ncache_dir = "{temp.name}/cache"\n', 1)
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".toml", prefix=".zensical-en-", dir=DOCS
        ) as config_file:
            config_file.write(config)
            config_file.flush()
            subprocess.run(
                [zensical, "build", "--strict", "-f", config_file.name], cwd=DOCS, check=True
            )
        shutil.copytree(temp / "site/en", DOCS / "site/en", dirs_exist_ok=True)
    # Keep published article URLs usable after reorganizing the source tree.
    redirects = json.loads((DOCS / "redirects.json").read_text())
    # Former English translations now lead to the authoritative Chinese article.
    for page in (DOCS / "src/zh").rglob("*.md"):
        name = str(page.relative_to(DOCS / "src/zh"))
        if not (DOCS / "src/en" / name).exists():
            redirects.setdefault(name, name)
    for old, new in redirects.items():
        for locale in ("en", "zh", ""):
            source = DOCS / "site" / locale / old.removesuffix(".md")
            target_locale = locale or "zh"
            if not (DOCS / "src" / target_locale / new).exists():
                target_locale = "zh"
            if old == new and locale != "en":
                continue
            target = DOCS / "site" / target_locale / new.removesuffix(".md")
            source = source.parent if source.name == "index" else source
            target = target.parent if target.name == "index" else target
            source.mkdir(parents=True, exist_ok=True)
            href = escape(os.path.relpath(target, source) + "/", quote=True)
            (source / "index.html").write_text(
                f'<!doctype html><html lang="{locale or "en"}"><head><meta charset="utf-8">'
                f'<meta http-equiv="refresh" content="0; url={href}"><title>Page moved</title>'
                f'<link rel="canonical" href="{href}"></head><body>'
                f'<a href="{href}">Continue to the current article / 查看新版文档</a></body></html>'
            )


if __name__ == "__main__":
    build()
