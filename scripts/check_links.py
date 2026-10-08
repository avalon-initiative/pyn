#!/usr/bin/env python3
"""Verify that every relative markdown link in this repo points at an existing file and anchor."""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
LINK = re.compile(r"(?<!!)\[[^\]]*\]\(([^)\s]+)\)")
FENCE = re.compile(r"^```.*?^```", re.S | re.M)


def slug(heading: str) -> str:
    s = re.sub(r"[`*_]", "", heading.strip().lower())
    s = re.sub(r"[^\w\- ]", "", s)
    return s.replace(" ", "-")


def anchors(path: pathlib.Path) -> set[str]:
    text = FENCE.sub("", path.read_text())
    return {slug(m.group(1)) for m in re.finditer(r"^#{1,6}\s+(.*)$", text, re.M)}


bad = []
for md in sorted(ROOT.rglob("*.md")):
    if {".git", "node_modules", "target"} & set(md.parts):
        continue
    text = FENCE.sub("", md.read_text())
    for target in LINK.findall(text):
        if re.match(r"^[a-z]+:", target):
            continue
        file_part, _, frag = target.partition("#")
        dest = (md.parent / file_part).resolve() if file_part else md
        if not dest.exists():
            bad.append(f"{md.relative_to(ROOT)}: missing file {target}")
        elif frag and dest.suffix == ".md" and frag not in anchors(dest):
            bad.append(f"{md.relative_to(ROOT)}: missing anchor {target}")

print("\n".join(bad) if bad else "links ok")
sys.exit(1 if bad else 0)
