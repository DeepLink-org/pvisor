#!/usr/bin/env python3
"""Build both native Zensical navigation trees into one bilingual site."""

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
    # content and search index, rendering Chinese HTML with its native zh theme.
    config = (DOCS / "zensical.toml").read_text()
    before, nav = config.split("nav = [", 1)
    nav, after = nav.split("\n[project.theme]", 1)
    nav = nav.replace('"en/', '"zh/')
    for en, zh in {
        "Home": "首页",
        "Get started": "开始使用",
        "Concepts": "概念与边界",
        "Guides": "任务指南",
        "Design": "实现设计",
        "Reference": "参考",
        "Development": "参与开发",
    }.items():
        nav = nav.replace('"' + en + '"', '"' + zh + '"')
    config = before + "nav = [" + nav + "\n[project.theme]" + after
    config = config.replace('language = "en"', 'language = "zh"').replace(
        'homepage = "en/"', 'homepage = "zh/"'
    )
    with tempfile.TemporaryDirectory(prefix=".zensical-zh-", dir=DOCS) as temp:
        temp = Path(temp)
        config = config.replace('site_dir = "site"', f'site_dir = "{temp.name}/site"')
        config = config.replace("[project]\n", f'[project]\ncache_dir = "{temp.name}/cache"\n', 1)
        with tempfile.NamedTemporaryFile(
            mode="w", suffix=".toml", prefix=".zensical-zh-", dir=DOCS
        ) as config_file:
            config_file.write(config)
            config_file.flush()
            subprocess.run(
                [zensical, "build", "--strict", "-f", config_file.name], cwd=DOCS, check=True
            )
        shutil.copytree(temp / "site/zh", DOCS / "site/zh", dirs_exist_ok=True)
    # Keep published article URLs usable after reorganizing the source tree.
    for old, new in json.loads((DOCS / "redirects.json").read_text()).items():
        for locale in ("en", "zh", ""):
            source = DOCS / "site" / locale / old.removesuffix(".md")
            target = DOCS / "site" / (locale or "en") / new.removesuffix(".md")
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
