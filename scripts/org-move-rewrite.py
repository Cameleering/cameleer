#!/usr/bin/env python3
"""One-off rewrite for the move of the repository to the `cameleering` organization.

Rewrites the repository, image and sibling-repo references from `pmsudhi` to `cameleering`
in every tracked text file, except the files listed in EXCEPTIONS, which record history.
Run from the repository root:  python3 scripts/org-move-rewrite.py [--check]

--check rewrites nothing and exits 1 when a reference outside the exceptions is left.
The workflows' Docker Hub login and namespace are edited by hand (see the commit).
"""
import re
import subprocess
import sys

# History stays as recorded: GitHub redirects the old URLs and old image tags keep resolving.
EXCEPTIONS = (
    "CHANGELOG.md",                         # release history
    "benchmarks/results/",                  # recorded runs
    "docs/design/github-organization.md",   # the superseded plan, kept as written
    "docs/website-plan.md",                 # records the pmsudhi.github.io redirect
    "docs/design/cameleering-org-move.md",  # the move plan, which describes the state before it
    "scripts/org-move-rewrite.py",          # this script
)

REWRITES = [
    (re.compile(r"\bpmsudhi/cameleer\b"), "cameleering/cameleer"),  # GitHub, Docker Hub, ghcr, gitmcp, deepwiki
    (re.compile(r"\bpmsudhi/simoom\b"), "cameleering/simoom"),
    (re.compile(r"\bpmsudhi/karez\b"), "cameleering/karez"),
    (re.compile(r"https?://pmsudhi\.github\.io/cameleer/?"), "https://cameleer.ing/"),
]


def tracked():
    out = subprocess.run(["git", "ls-files", "-z"], capture_output=True, check=True).stdout
    return [p for p in out.decode().split("\0") if p and not p.startswith(EXCEPTIONS)]


def main():
    check = "--check" in sys.argv
    changed, left = 0, []
    for path in tracked():
        try:
            data = open(path, "rb").read()
            text = data.decode("utf-8")
        except (UnicodeDecodeError, FileNotFoundError, IsADirectoryError):
            continue
        new = text
        for pat, rep in REWRITES:
            new = pat.sub(rep, new)
        if new != text:
            if check:
                left.append(path)
            else:
                open(path, "wb").write(new.encode("utf-8"))
                changed += 1
    if check:
        for p in left:
            print("left:", p)
        sys.exit(1 if left else 0)
    print(f"rewrote {changed} files")


if __name__ == "__main__":
    main()
