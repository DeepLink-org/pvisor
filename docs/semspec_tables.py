"""Read-only semspec Markdown preprocessing for the native Zensical renderer."""

import html
import logging
import os
import subprocess
from pathlib import Path

from markdown import Extension
from markdown.preprocessors import Preprocessor
from zensical.extensions.context import ContextPreprocessor

LOGGER = logging.getLogger(__name__)


class TablePreprocessor(Preprocessor):
    """Pass Markdown to semspec before tables and fenced blocks are parsed."""

    def run(self, lines):
        text = "\n".join(lines)
        if "{{ table(" not in text and "<!-- semspec:" not in text:
            return lines
        live = os.environ.get("PVISOR_DOCS_LIVE") == "1"
        try:
            return self.render(text, live)
        except (RuntimeError, OSError, subprocess.TimeoutExpired) as error:
            if not live:
                raise
            LOGGER.warning("Table rendering unavailable during live preview: %s", error)
            return [
                '!!! warning "Table rendering unavailable during live preview"',
                "",
                "    " + html.escape(str(error)).replace("\n", "<br>"),
                "",
                *lines,
            ]

    def render(self, text, live):
        context = ContextPreprocessor.from_markdown(self.md)
        if context is None:
            raise RuntimeError("semspec tables require Zensical page context")
        root = Path(context.config.get("root_dir", ".")).resolve()
        docs_dir = root / context.config["docs_dir"]
        source = docs_dir / context.page.path
        command = [
            os.environ.get("SEMSPEC_BIN", "semspec"),
            "filter",
            "--source",
            str(source),
            "--stdin",
        ]
        if os.environ.get("SEMSPEC_RESULTS"):
            command.extend(["--results", os.environ["SEMSPEC_RESULTS"]])
        if live:
            command.append("--best-effort")
        result = subprocess.run(
            command,
            input=text,
            text=True,
            encoding="utf-8",
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            check=False,
            timeout=30,
        )
        if result.returncode:
            raise RuntimeError(f"semspec filter failed for {source}: {result.stderr.strip()}")
        if result.stderr.strip():
            LOGGER.warning("%s", result.stderr.strip())
        return result.stdout.split("\n")


class TableExtension(Extension):
    """Register source substitution ahead of Python-Markdown parsing."""

    def extendMarkdown(self, md):
        md.preprocessors.register(TablePreprocessor(md), "semspec_tables", 35)


def makeExtension(**kwargs):
    return TableExtension(**kwargs)
