#!/usr/bin/env python3
"""Build matching Chinese and English documentation with native locale themes."""

import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
from html import escape
from pathlib import Path

import tomllib

DOCS = Path(__file__).resolve().parents[1] / "docs"

EN_NAV_LABELS = {
    "Daemon 单机执行": "Daemon single-node execution",
    "Daemon 上手": "Daemon quickstart",
    "Cluster 扩展性（退役历史）": "Cluster scalability (retired history)",
    "Cluster 实验问题": "Cluster experiment questions",
    "任务学习路线": "Learning path",
    "首页": "Home", "为什么": "Why pVisor", "开始使用": "Get started",
    "指南": "Guides", "接入你的 Agent": "Connect your agent", "策略": "Policies",
    "执行器": "Executors", "概念": "Concepts", "基准与对比": "Benchmarks and comparisons",
    "安全": "Security", "参考": "Reference", "设计与研究": "Design and research",
    "VM 内存节约与开销": "VM memory savings and overhead",
    "内存优化": "Memory optimization",
    "内存卸载": "RAM offload",
    "内存压缩": "Memory compression",
    "研究方向": "Research directions", "VM RAM offload": "VM RAM offload", "社区": "Community", "对比": "Comparisons",
    "Gemini CLI": "Gemini CLI", "aider": "aider", "OpenCode": "OpenCode",
    "在 CI 中运行": "Run in CI", "并行 Agent": "Parallel agents", "RL rollout": "RL rollouts",
    "文件系统开销": "Filesystem overhead", "网络开销": "Network overhead",
    "apply/drop 成本": "apply/drop cost", "端到端任务": "End-to-end tasks",
    "监督成本": "Supervision cost", "并发密度": "Concurrency density",
    "隔离有效性": "Isolation effectiveness", "Agent 自带沙箱": "Agent-native sandboxes",
    "Docker / devcontainer": "Docker / devcontainer", "云端沙箱": "Cloud sandboxes",
    "隔离基座": "Isolation runtimes", "RL 基础设施": "RL infrastructure",
    "第三方审计": "Third-party audits", "外部编排与单机执行": "External orchestration and single-node execution",
    "RL 执行基座": "RL execution substrate", "论文与报告": "Publications and reports",
    "治理": "Governance", "使用者": "Adopters",
}


def english_nav(node):
    if isinstance(node, str):
        return json.dumps(node.replace("zh/", "en/", 1))
    if isinstance(node, list):
        return "[" + ", ".join(english_nav(child) for child in node) + "]"
    entries = []
    for label, child in node.items():
        planned = label.endswith("（规划中）")
        label = EN_NAV_LABELS[label.removesuffix("（规划中）")]
        if planned:
            label += " (planned)"
        entries.append(json.dumps(label) + " = " + english_nav(child))
    return "{" + ", ".join(entries) + "}"


def localize_search(site, locale):
    """Keep the native search index and result links inside the current locale."""
    index = json.loads((site / "search.json").read_text())
    prefix = locale + "/"
    index["items"] = [
        {**item, "location": item["location"].removeprefix(prefix)}
        for item in index["items"] if item["location"].startswith(prefix)
    ]
    (site / locale / "search.json").write_text(json.dumps(index, ensure_ascii=False))
    for page in (site / locale).rglob("*.html"):
        def configure(match):
            config = json.loads(match[2])
            config["base"] = os.path.relpath(site / locale, page.parent)
            return match[1] + json.dumps(config, ensure_ascii=False) + match[3]

        page.write_text(re.sub(
            r'(<script id="__config"[^>]*>)(.*?)(</script>)', configure, page.read_text(),
            flags=re.S,
        ))


def copy_public_source(source, destination):
    """Never give raw evidence to the site generator, even at nested depths."""
    shutil.copytree(source, destination, ignore=shutil.ignore_patterns(".data"))


def build() -> None:
    from importlib import import_module

    import_module("check-docs").check_translations()
    import_module("check-reference").check()
    zensical = shutil.which("zensical") or str(Path(sys.executable).with_name("zensical"))
    # The generator skips hidden .data trees. Give it a fresh visible source
    # directory while still excluding raw evidence from the copied contents.
    with tempfile.TemporaryDirectory(prefix="pvisor-docs-source-", dir=DOCS) as temporary:
        public_source = Path(temporary) / "source"
        copy_public_source(DOCS / "src", public_source)
        build_public_source(zensical, public_source)


def build_public_source(zensical, public_source):
    config = (DOCS / "zensical.toml").read_text().replace(
        'docs_dir = "src"', 'docs_dir = ' + json.dumps(public_source.relative_to(DOCS).as_posix())
    )
    shutil.rmtree(DOCS / "site", ignore_errors=True)
    with tempfile.NamedTemporaryFile(mode="w", suffix=".toml", prefix=".zensical-zh-", dir=DOCS) as zh_config:
        zh_config.write(config.replace('[project]\n', '[project]\ncache_dir = ' +
                                       json.dumps((public_source.parent / 'cache').relative_to(DOCS).as_posix()) + '\n', 1))
        zh_config.flush()
        subprocess.run([zensical, "build", "--clean", "--strict", "-f", zh_config.name], cwd=DOCS, check=True)
    localize_search(DOCS / "site", "zh")
    # Zensical has one language/navigation per build; mirror the source navigation.
    before, nav = config.split("nav = [", 1)
    nav, after = nav.split("\n[project.theme]", 1)
    nav = english_nav(tomllib.loads(config)["project"]["nav"])
    config = before + "nav = " + nav + "\n\n[project.theme]" + after
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
                [zensical, "build", "--clean", "--strict", "-f", config_file.name], cwd=DOCS, check=True
            )
        localize_search(temp / "site", "en")
        shutil.copytree(temp / "site/en", DOCS / "site/en", dirs_exist_ok=True)
    # Keep published article URLs usable after reorganizing the source tree.
    redirects = json.loads((DOCS / "redirects.json").read_text())
    for old, new in redirects.items():
        for locale in ("en", "zh", ""):
            source = DOCS / "site" / locale / old.removesuffix(".md")
            target_locale = locale or "zh"
            if old == new:
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
