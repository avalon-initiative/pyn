#!/usr/bin/env python3
"""Fail when the public tree references private code or packages, or carries secrets (decision #140)."""
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
SELF = pathlib.Path(__file__).resolve().relative_to(ROOT).as_posix()

PRIVATE_NAME = re.compile(r"pyn-cloud", re.I)
CARGO_EXTERNAL = re.compile(r"\b(git|registry|registry-index)\s*=\s*\"")
CARGO_PATH = re.compile(r'path\s*=\s*"([^"]+)"')
LOCK_SOURCE = re.compile(r'^source = "(?!registry\+https://github\.com/rust-lang/crates\.io-index)(.+)"', re.M)
SECRET_NAME = re.compile(r"(^|/)(\.env(\..*)?|\.npmrc|\.netrc|id_(rsa|dsa|ecdsa|ed25519)|.*\.(pem|p12|pfx|key))$")
SECRET_ALLOWED = {".env.example"}
SECRET_CONTENT = [
    re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----\s*\n[A-Za-z0-9+/=]{20,}"),
    re.compile(r"\bAKIA[0-9A-Z]{16}\b"),
    re.compile(r"\bgh[pousr]_[A-Za-z0-9]{30,}\b"),
    re.compile(r"\bxox[abprs]-[A-Za-z0-9-]{10,}\b"),
    re.compile(r"_authToken\s*="),
]


def tracked() -> list[str]:
    out = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, capture_output=True, check=True).stdout
    return sorted(p for p in out.decode().split("\0") if p and (ROOT / p).is_file())


problems = []
files = tracked()

for name in files:
    base = name.rsplit("/", 1)[-1]
    if base == ".gitmodules":
        problems.append(f"{name}: submodules are not allowed")
    if base in {"config", "config.toml"} and "/.cargo/" in f"/{name}" and re.search(
        r"^\s*\[(registries|source)", (ROOT / name).read_text(), re.M
    ):
        problems.append(f"{name}: custom cargo registries or source replacement")
    if SECRET_NAME.search(name) and name not in SECRET_ALLOWED:
        problems.append(f"{name}: looks like a secret file")

for name in files:
    path = ROOT / name
    try:
        text = path.read_text()
    except (UnicodeDecodeError, OSError):
        continue
    base = path.name
    if base == "Cargo.toml":
        if CARGO_EXTERNAL.search(text):
            problems.append(f"{name}: git or alternate-registry dependency")
        for target in CARGO_PATH.findall(text):
            if not (path.parent / target).resolve().is_relative_to(ROOT):
                problems.append(f"{name}: path dependency outside the repository ({target})")
    if base == "Cargo.lock":
        for m in LOCK_SOURCE.finditer(text):
            problems.append(f"{name}: non-crates.io source {m.group(1)}")
    if base in {"package.json", "package-lock.json"} and re.search(r"git\+|github:|ssh://", text):
        problems.append(f"{name}: git dependency")
    if name != SELF and not name.endswith(".md") and PRIVATE_NAME.search(text):
        problems.append(f"{name}: references private code")
    for pat in SECRET_CONTENT:
        if pat.search(text):
            problems.append(f"{name}: secret-like content ({pat.pattern})")

if problems:
    print("public-boundary check failed (decision #140):")
    for p in sorted(set(problems)):
        print(f"  {p}")
    sys.exit(1)
print(f"public-boundary check ok ({len(files)} files)")
