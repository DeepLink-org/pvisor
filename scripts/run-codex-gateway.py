#!/usr/bin/env python3
"""Run a ChatGPT-authenticated Codex through pVisor capture over HTTP/SSE.

All Codex provider overrides apply to this process only. No auth or config files
are rewritten. Additional arguments after -- are passed to Codex.
"""

import argparse
import os
from pathlib import Path
import shutil


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--upstream-proxy", required=True)
    parser.add_argument("--dns-over-https")
    parser.add_argument("--pvisor", default=str(root / "target/debug/pvisor"))
    local_codex = Path.home() / ".local/bin/codex"
    parser.add_argument("--codex", default=str(local_codex) if local_codex.exists() else shutil.which("codex"))
    parser.add_argument("codex_args", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if not args.codex:
        parser.error("Codex was not found; pass --codex /path/to/codex")
    extra = args.codex_args
    if extra[:1] == ["--"]:
        extra = extra[1:]
    command = [args.pvisor, "run", "--gateway-profile", "codex-chatgpt",
               "--overlaynet-upstream-proxy", args.upstream_proxy,
               "--gateway-level", "full", "--gateway-debug"]
    if args.dns_over_https:
        command += ["--overlaynet-dns-over-https", args.dns_over_https]
    command += ["--", str(Path(args.codex).absolute()), *extra]
    os.execv(args.pvisor, command)


if __name__ == "__main__":
    main()
