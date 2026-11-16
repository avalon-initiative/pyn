#!/usr/bin/env python3
"""Fail when a test compares git's message text or branches on git's version (decision #1032, docs/ci.md).

Scanned (minus the shared comparator, common/behavior.rs): crates/*/tests/**/*.rs and the `#[cfg(test)]` tail of crates/*/src/**/*.rs. Comment lines are ignored.
Rules (a line of the file, or the statement it starts, is reported with the rule name):
  git-stderr   an assert_eq!/assert_ne! whose first two operands hold git's stderr (`g.stderr`, `want.err()`, `oracle.stderr`)
               next to anything, or stderr of both tools; or an assert! testing git's stderr with contains/==/starts_with.
               `.is_empty()` and `.len()` operands are exempt (that is `Stderr::Quiet` territory).
  git-stdout   an assert_eq!/assert_ne! of git's stdout against another stdout outside `assert_same_behavior`.
  git-version  `git --version`, `git_version`, `GIT_VERSION`, or a `version >=`/`>` comparison against a number or list.
  replace-pyn  `.replace("pyn", "git")`-style rewriting of pyn's spelling to make text match.
Git side = an identifier named git*/oracle*/want*/expected*/theirs*/g/go/gout (`GIT_SIDE`).
Escape: `// oracle-text: allowed <reason>` on the offending line or the line above it; the reason is required.
Allowlist: scripts/oracle_text_allowlist.txt (`path  # reason`). A file off the list that violates fails, and a listed
file that no longer violates fails too, so the list only shrinks. `--generate` prints a fresh list from the tree.
"""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
ALLOWLIST = ROOT / "scripts" / "oracle_text_allowlist.txt"
POINTER = 'see docs/ci.md, "Comparing pyn with git in tests" (decision #1032)'

GIT_SIDE = r"(?:\w*(?:git|oracle|want|expected|theirs)\w*|g|go|gout)"
SIDE_ERR = re.compile(rf"(?<![\w.]){GIT_SIDE}\s*\.\s*(?:stderr\b|err\s*\(\s*\))", re.I)
SIDE_OUT = re.compile(rf"(?<![\w.]){GIT_SIDE}\s*\.\s*(?:stdout\b|out\s*\(\s*\))", re.I)
ANY_ERR = re.compile(r"\.\s*(?:stderr\b|err\s*\(\s*\))")
ANY_OUT = re.compile(r"\.\s*(?:stdout\b|out\s*\(\s*\))")
SIZE_ONLY = re.compile(r"\.\s*(?:is_empty|len)\s*\(")
TEXT_TEST = re.compile(r"\.(?:contains|starts_with|ends_with)\s*\(|[!=]=")
MACRO = re.compile(r"\b(assert_eq|assert_ne|assert)!\s*\(")
VERSION_LINE = [
    re.compile(r"\bgit_version\w*|\bGIT_VERSION\b", re.I),
    re.compile(r"\bversion\s*(?:>=|<=|>|<)\s*(?:vec!\s*)?[\[(]?\s*\d"),
    re.compile(r"\bgit_\d_\d+\b|\bsince_git_\d"),
]
GIT_VERSION_CALL = re.compile(r'Command::new\(\s*"git"\s*\)\s*\.args?\(\s*(?:&?\[\s*)?"--version"')
REPLACE = re.compile(r'\.replace\(\s*"[^"]*pyn[^"]*"\s*,\s*"[^"]*git[^"]*"\s*\)')
ESCAPE = re.compile(r"//\s*oracle-text:\s*allowed\b(.*)")


def blank_strings_comments(text: str) -> str:
    """Same-length copy with string and char literal bodies and comments blanked, so parens and commas can be counted."""
    out, i, n = [], 0, len(text)
    while i < n:
        c = text[i]
        m = re.compile(r'r(#*)"').match(text, i) if c == "r" and (i == 0 or not text[i - 1].isalnum()) else None
        if m:
            end = text.find('"' + m.group(1), m.end())
            end = n if end < 0 else end + 1 + len(m.group(1))
            out.append(re.sub(r"[^\n]", " ", text[i:end]))
            i = end
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            out.append('"' + re.sub(r"[^\n]", " ", text[i + 1 : j]) + '"')
            i = j + 1
        elif text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            out.append(" " * (j - i))
            i = j
        elif c == "'" and re.compile(r"'(?:\\.[^']*|[^\\'])'").match(text, i):
            m = re.compile(r"'(?:\\.[^']*|[^\\'])'").match(text, i)
            out.append("' '".ljust(m.end() - i))
            i = m.end()
        else:
            out.append(c)
            i += 1
    return "".join(out)


def split_args(body: str) -> list[str]:
    args, depth, cur = [], 0, []
    for c in body:
        if c in "([{":
            depth += 1
        elif c in ")]}":
            depth -= 1
        if c == "," and depth == 0:
            args.append("".join(cur))
            cur = []
        else:
            cur.append(c)
    args.append("".join(cur))
    return args


def operands(raw: str, blank: str, start: int):
    """Top-level arguments of the macro whose `(` ends at `start`, taken from raw text; and the end offset."""
    depth, i = 1, start
    while i < len(blank) and depth:
        depth += blank[i] in "([{"
        depth -= blank[i] in ")]}"
        i += 1
    return split_args(raw[start : i - 1]), i


def check_text(text: str) -> list[tuple[int, str]]:
    """Returns (line, rule) pairs for a source text, escapes already applied."""
    lines = text.split("\n")
    blank = blank_strings_comments(text)
    found: list[tuple[int, str]] = []

    def line_of(off: int) -> int:
        return text.count("\n", 0, off) + 1

    for m in MACRO.finditer(blank):
        args, _ = operands(text, blank, m.end())
        kind, ops = m.group(1), [a for a in args[:2]]
        sized = [bool(SIZE_ONLY.search(a)) for a in ops]
        if kind == "assert":
            a = ops[0]
            if SIDE_ERR.search(a) and TEXT_TEST.search(a) and not sized[0]:
                found.append((line_of(m.start()), "git-stderr"))
            continue
        if len(ops) < 2 or any(sized):
            continue
        a, b = ops
        if (SIDE_ERR.search(a) or SIDE_ERR.search(b)) and (ANY_ERR.search(a) or ANY_ERR.search(b)):
            found.append((line_of(m.start()), "git-stderr"))
        elif ANY_ERR.search(a) and ANY_ERR.search(b):
            found.append((line_of(m.start()), "git-stderr"))
        elif (SIDE_OUT.search(a) and ANY_OUT.search(b)) or (SIDE_OUT.search(b) and ANY_OUT.search(a)):
            found.append((line_of(m.start()), "git-stdout"))

    code = re.sub(r"(?m)^[ \t]*//.*$", lambda m: " " * len(m.group()), text)
    pats = [("git-version", p) for p in VERSION_LINE + [GIT_VERSION_CALL]] + [("replace-pyn", REPLACE)]
    for rule, pat in pats:
        found += [(line_of(m.start()), rule) for m in pat.finditer(code)]

    kept = []
    for ln, rule in sorted(set(found)):
        near = lines[max(ln - 2, 0) : ln]
        esc = [ESCAPE.search(x) for x in near]
        esc = [e for e in esc if e]
        if esc and esc[-1].group(1).strip():
            continue
        kept.append((ln, rule + (" (escape needs a reason)" if esc else "")))
    return kept


def sources() -> list[str]:
    out = subprocess.run(["git", "ls-files", "-z", "crates"], cwd=ROOT, capture_output=True, check=True).stdout
    names = [p for p in out.decode().split("\0") if p.endswith(".rs")]
    return sorted(p for p in names if re.match(r"crates/[^/]+/tests/", p) or re.match(r"crates/[^/]+/src/", p))


SHARED = {"crates/pyn-cli/tests/common/behavior.rs"}


def scan(name: str) -> list[tuple[int, str]]:
    if name in SHARED:
        return []
    text = (ROOT / name).read_text()
    if "/src/" in name:
        at = text.find("#[cfg(test)]")
        if at < 0:
            return []
        text = "\n" * text.count("\n", 0, at) + text[at:]
    return check_text(text)


def read_allowlist() -> dict[str, str]:
    listed = {}
    for raw in ALLOWLIST.read_text().splitlines():
        if raw.strip() and not raw.startswith("#"):
            path, _, why = raw.partition("#")
            listed[path.strip()] = why.strip()
    return listed


def main() -> int:
    hits = {n: h for n in sources() if (h := scan(n))}
    if "--generate" in sys.argv:
        for n, h in sorted(hits.items()):
            print(f"{n}  # {', '.join(sorted({r.split(' ')[0] for _, r in h}))}")
        return 0
    listed = read_allowlist()
    problems = []
    for n, h in sorted(hits.items()):
        if n not in listed:
            problems += [f"  {n}:{ln}: {rule}" for ln, rule in h]
    for n in sorted(listed):
        if n not in hits:
            problems.append(f"  {n}: on the allowlist but no longer violates; remove the line (the list only shrinks)")
    if problems:
        print(f"oracle-text check failed: {POINTER}")
        print("\n".join(problems))
        return 1
    print(f"oracle-text check ok ({len(listed)} files allowlisted)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
