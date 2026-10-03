#!/usr/bin/env python3
"""Fail-closed repository documentation contract checks."""

from __future__ import annotations

import json
import os
import re
import subprocess
import sys
import unicodedata
from pathlib import Path
from urllib.parse import unquote

ROOT = Path(__file__).resolve().parent.parent
ERRORS: list[str] = []


def fail(message: str) -> None:
    ERRORS.append(message)


def read(relative: str) -> str:
    path = ROOT / relative
    try:
        return path.read_text(encoding="utf-8")
    except OSError as exc:
        fail(f"cannot read {relative}: {exc}")
        return ""


def markdown_files() -> list[Path]:
    tracked = subprocess.run(
        ["git", "ls-files", "*.md"],
        cwd=ROOT,
        check=True,
        capture_output=True,
        text=True,
    ).stdout.splitlines()
    paths = {ROOT / item for item in tracked}
    for path in ROOT.rglob("*.md"):
        if not any(part in {".git", ".codegraph", "target", "node_modules"} for part in path.parts):
            paths.add(path)
    return sorted(path for path in paths if path.is_file())


def without_code(text: str) -> str:
    text = re.sub(r"(?ms)^\s*(```|~~~).*?^\s*\1\s*$", "", text)
    return re.sub(r"`[^`\n]*`", "", text)


def without_fenced_code(text: str) -> str:
    return re.sub(r"(?ms)^\s*(```|~~~).*?^\s*\1\s*$", "", text)


def slug_base(heading: str) -> str:
    heading = re.sub(r"<[^>]+>", "", heading).strip().lower()
    heading = heading.replace("`", "")
    kept: list[str] = []
    for char in heading:
        category = unicodedata.category(char)
        if char in {"-", "_", " "} or char.isspace() or category[0] in {"L", "N", "M"}:
            kept.append(char)
    # github-slugger replaces each whitespace character after punctuation is
    # removed; it does not collapse the two spaces left around an `&`.
    return re.sub(r"\s", "-", "".join(kept).strip())


def anchors(path: Path) -> set[str]:
    values: set[str] = set()
    counts: dict[str, int] = {}
    for line in without_fenced_code(path.read_text(encoding="utf-8")).splitlines():
        match = re.match(r"^ {0,3}#{1,6}\s+(.+?)\s*#*\s*$", line)
        if not match:
            continue
        base = slug_base(match.group(1))
        if not base:
            continue
        count = counts.get(base, 0)
        counts[base] = count + 1
        values.add(base if count == 0 else f"{base}-{count}")
    values.update(
        match.group(1)
        for match in re.finditer(
            r"<a\s+(?:name|id)=[\"']([^\"']+)[\"']", path.read_text(encoding="utf-8"), re.I
        )
    )
    return values


def check_links(paths: list[Path]) -> None:
    link_re = re.compile(r"(?<!!)\[[^\]]*\]\(([^)]+)\)")
    anchor_cache: dict[Path, set[str]] = {}
    for source in paths:
        text = without_code(source.read_text(encoding="utf-8"))
        for match in link_re.finditer(text):
            target_text = match.group(1).strip()
            if target_text.startswith("<") and ">" in target_text:
                target_text = target_text[1 : target_text.index(">")]
            else:
                target_text = target_text.split(maxsplit=1)[0]
            if not target_text or target_text.startswith(("http://", "https://", "mailto:", "data:")):
                continue
            if "{" in target_text or "}" in target_text:
                continue
            path_text, separator, anchor = target_text.partition("#")
            target = source if not path_text else (source.parent / unquote(path_text)).resolve()
            line = text.count("\n", 0, match.start()) + 1
            if not target.exists():
                fail(f"{source.relative_to(ROOT)}:{line}: missing local link target {target_text}")
                continue
            if separator and anchor and target.suffix.lower() == ".md":
                anchor = unquote(anchor).lower()
                target_anchors = anchor_cache.setdefault(target, anchors(target))
                if anchor not in target_anchors:
                    fail(
                        f"{source.relative_to(ROOT)}:{line}: missing anchor #{anchor} "
                        f"in {target.relative_to(ROOT)}"
                    )


def require_headings(path: str, expected: list[str]) -> None:
    text = read(path)
    found = {
        match.group(1).strip()
        for match in re.finditer(r"^##\s+(.+?)\s*$", without_code(text), re.MULTILINE)
    }
    missing = [heading for heading in expected if heading not in found]
    if missing:
        fail(f"{path}: missing required headings: {', '.join(missing)}")


def source_contracts() -> None:
    migrations = read("crates/codegraph-store/src/migrations.rs")
    schema_match = re.search(r"CURRENT_SCHEMA_VERSION:\s*i64\s*=\s*(\d+)", migrations)
    if not schema_match:
        fail("could not derive CURRENT_SCHEMA_VERSION")
    elif f"Current schema version: **{schema_match.group(1)}**" not in read("docs/data-model.md"):
        fail("docs/data-model.md does not match CURRENT_SCHEMA_VERSION")

    types = read("crates/codegraph-core/src/types.rs")
    language_match = re.search(r"LANGUAGE_STRINGS:\s*\[&str;\s*(\d+)\]", types)
    if not language_match:
        fail("could not derive LANGUAGE_STRINGS length")
    else:
        expected = language_match.group(1)
        for doc in ("docs/languages.md", "docs/grammar-manifest.md"):
            if expected not in read(doc):
                fail(f"{doc} does not mention the derived language-id count {expected}")

    try:
        tools = json.loads(read("crates/codegraph-mcp/src/tools_list.json"))
    except json.JSONDecodeError as exc:
        fail(f"invalid embedded MCP tools JSON: {exc}")
        tools = []
    mcp_doc = read("docs/mcp.md")
    if f"All {len(tools)} Tools" not in mcp_doc:
        fail(f"docs/mcp.md does not match embedded tool count {len(tools)}")
    for tool in tools:
        if tool.get("name", "") not in mcp_doc:
            fail(f"docs/mcp.md omits {tool.get('name', '<unnamed tool>')}")


def main() -> int:
    claude = ROOT / "CLAUDE.md"
    if claude.is_symlink():
        fail("CLAUDE.md must be a regular @import file, not a symlink")
    if read("CLAUDE.md") != "@AGENTS.md\n":
        fail("CLAUDE.md must contain exactly @AGENTS.md plus a newline")

    require_headings(
        "README.md",
        ["Why CodeGraph", "Install", "Quickstart", "CLI", "MCP", "Agents and IDEs", "Browser viewer", "Performance", "Development", "Community", "License"],
    )
    require_headings(
        "docs/readme/README.zh-CN.md",
        ["为什么选择 CodeGraph", "安装", "快速上手", "CLI", "MCP", "Agents 与 IDE", "浏览器查看器", "性能", "开发", "交流与反馈", "许可证"],
    )

    english = read("README.md")
    chinese = read("docs/readme/README.zh-CN.md")
    for snippet in (
        "scripts/install.sh",
        "scripts/install.ps1",
        "cargo install --locked --git",
        "codegraph init .",
        "codegraph search",
        "codegraph explore",
        "gh attestation verify",
    ):
        if snippet not in english or snippet not in chinese:
            fail(f"README mirrors must both contain {snippet!r}")
    if re.search(r"sub[- ]?millisecond|亚毫秒", english + chinese, re.I):
        fail("landing pages contain an unversioned sub-millisecond performance claim")

    if not (ROOT / ".github/workflows/release.yml").is_file():
        fail("current release workflow is missing")
    if (ROOT / ".github/workflows/release-please.yml").exists():
        fail("retired release-please.yml still exists")
    if (ROOT / "cliff.toml").exists() or (ROOT / "version.txt").exists():
        fail("retired git-cliff/version.txt release surfaces still exist")

    current_docs = [
        ROOT / "README.md",
        ROOT / "AGENTS.md",
        ROOT / "CONTRIBUTING.md",
        ROOT / "changelog/README.md",
        *[path for path in (ROOT / "docs").glob("*.md")],
        *[path for path in (ROOT / "docs/readme").glob("*.md")],
    ]
    for path in current_docs:
        text = path.read_text(encoding="utf-8")
        for retired in ("release-please.yml", "cliff.toml", "version.txt"):
            if retired in text:
                fail(f"{path.relative_to(ROOT)} references retired {retired}")

    upstream = read("docs/upstream-sync/UPSTREAM.md")
    if "f4ddf508516332419ea3c95702810765936cf679..origin/main" not in upstream:
        fail("upstream ledger does not carry the audited next-discovery boundary")
    if "Tracked colby release:** `v1.6.1`" not in upstream:
        fail("upstream ledger tracked release changed without a new formal tag")

    paths = markdown_files()
    check_links(paths)
    source_contracts()

    if ERRORS:
        for error in ERRORS:
            print(f"docs-check: ERROR: {error}", file=sys.stderr)
        print(f"docs-check: FAIL ({len(ERRORS)} problem(s))", file=sys.stderr)
        return 1
    print(f"docs-check: OK ({len(paths)} markdown files; links, anchors, mirrors, and source contracts)")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
