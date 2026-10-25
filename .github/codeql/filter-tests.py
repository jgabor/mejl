#!/usr/bin/env python3
"""Drops CodeQL results that sit in test code from SARIF files.

Test code is any file under a crate's tests/ or benches/ folder, and in a
Rust source file everything from the `#[cfg(test)]` that opens an inline
test module to the end of the file. mejl keeps inline test modules last in
each file (see AGENTS.md), so that is the whole module.

Usage: filter-tests.py <sarif dir>
"""

import json
import pathlib
import re
import sys

TEST_DIRS = re.compile(r"^crates/[^/]+/(tests|benches)/")
TEST_MOD = re.compile(r"^(pub(\([a-z]+\))?\s+)?mod\s+\w+\s*\{")


def test_start(path, cache={}):
    """The 1-based line where the inline test module starts, or None."""
    if path not in cache:
        start = None
        try:
            lines = pathlib.Path(path).read_text(encoding="utf-8").split("\n")
        except OSError:
            lines = []
        for i, line in enumerate(lines):
            if line.strip() != "#[cfg(test)]":
                continue
            rest = [l for l in lines[i + 1 :] if l.strip()]
            if rest and TEST_MOD.match(rest[0].strip()):
                start = i + 1
                break
        cache[path] = start
    return cache[path]


def in_tests(result):
    for loc in result.get("locations", []):
        phys = loc.get("physicalLocation", {})
        uri = phys.get("artifactLocation", {}).get("uri", "")
        line = phys.get("region", {}).get("startLine")
        if TEST_DIRS.match(uri):
            return True
        if uri.endswith(".rs") and line is not None:
            start = test_start(uri)
            if start is not None and line >= start:
                return True
    return False


def main(folder):
    for sarif in pathlib.Path(folder).glob("*.sarif"):
        doc = json.loads(sarif.read_text(encoding="utf-8"))
        dropped = 0
        for run in doc.get("runs", []):
            results = run.get("results", [])
            kept = [r for r in results if not in_tests(r)]
            dropped += len(results) - len(kept)
            run["results"] = kept
        sarif.write_text(json.dumps(doc), encoding="utf-8")
        print(f"{sarif.name}: dropped {dropped} result(s) in test code")


if __name__ == "__main__":
    main(sys.argv[1])
