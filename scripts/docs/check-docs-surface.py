#!/usr/bin/env python3
"""check-docs-surface.py — accuracy checks for docs/** markdown.

Checks (against ``scripts/docs/cli-surface.json``, regenerated from
:file:`src/main.rs` + :file:`src/tui/commands.rs`):

1. Every ``nanosb <subcommand>`` invocation inside fenced code blocks must name
   a real subcommand (aliases like ``nanosb delete`` resolve via the aliases).
2. Every ``--flag``/``-x`` used after ``nanosb <subcommand>`` in fenced code
   blocks must exist either as a global flag or as a flag of that subcommand.
3. TUI slash commands (``/add``, ``/theme``, ...) that appear as the first
   token of a line inside shell fenced blocks must exist in the TUI surface.
4. Every relative markdown link under the checked directory must resolve to an
   existing file or directory. External ``http(s)/mailto`` links and same-doc
   anchors (``#...``) are skipped. Links inside fenced code blocks and inline
   code spans are not treated as doc references.
5. Platform guard: lines mentioning ``Windows|WHPX|HCS|install.ps1`` fail,
   unless the line itself says the support is archived / not supported.

Dependency-light: Python 3 standard library only. Deterministic: sorted walk,
stable output, exit 0 = clean, exit 1 = one or more failures (CI-friendly).

Usage: python3 scripts/docs/check-docs-surface.py [--docs-dir DIR] [--surface FILE]
"""

import argparse
import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
DEFAULT_SURFACE = os.path.join(ROOT, "scripts", "docs", "cli-surface.json")

FENCE_RE = re.compile(r"^(\s*)(`{3,}|~{3,})(.*)$")
SHELL_LANG_RE = re.compile(
    r"^(sh|bash|zsh|fish|ksh|dash|shell|console|powershell|pwsh|cmd|ps1|code|exit|\$)$",
    re.IGNORECASE,
)
WIN_RE = re.compile(r"\b(Windows|WHPX|HCS)\b|install\.ps1")
ARCHIVED_RE = re.compile(
    r"(?i)\barchiv|no longer supported|not supported|\bunsupported\b|"
    r"discontinued|retired|end.of.support|deprecated|\bEOL\b|not planned|"
    r"\bnot\s+(avail|offered)\b|removed"
)
LONG_RE = re.compile(r"^--[A-Za-z0-9][\w-]*")
SHORT_RE = re.compile(r"^-([A-Za-z0-9_])$")


def load_surface(path):
    try:
        with open(path, encoding="utf-8") as fh:
            return json.load(fh)
    except OSError as exc:
        print(f"check-docs-surface: cannot read surface {path}: {exc}", file=sys.stderr)
        sys.exit(1)
    except json.JSONDecodeError as exc:
        print(f"check-docs-surface: invalid JSON in {path}: {exc}", file=sys.stderr)
        sys.exit(1)


def split_fences(text):
    """Return (non_fence_lines, [fence_dicts]) preserving per-line content.

    Each fence dict carries ``start`` (0-based source line number of the first
    content line) so command errors can report real file:line positions."""
    lines = text.splitlines()
    outside = []
    fences = []
    cur = None  # {char, length, lang, lines, start}
    for lineno, raw in enumerate(lines):
        m = FENCE_RE.match(raw)
        if m:
            fence, info = m.group(2), m.group(3).strip()
            if cur is None:
                cur = {"char": fence[0], "length": len(fence), "lang": info,
                       "lines": [], "start": lineno + 1}
                continue
            if fence[0] == cur["char"] and len(fence) >= cur["length"]:
                fences.append(cur)
                cur = None
                continue
            cur["lines"].append(raw)
            continue
        if cur is None:
            outside.append(raw)
        else:
            cur["lines"].append(raw)
    if cur is not None:
        fences.append(cur)
    return outside, fences


def is_shell_fence(lang):
    return bool(SHELL_LANG_RE.match(lang.strip()))


def merge_continuations(lines):
    """Join shell lines that end with a trailing backslash (line continuation)."""
    merged = []
    buf = None
    for raw in lines:
        line = raw.rstrip("\n")
        stripped = line.strip()
        if buf is None:
            if not stripped or stripped.startswith("#"):
                continue
            buf = line
        else:
            buf += " " + line
        if line.rstrip().endswith("\\"):
            buf = buf.rstrip()[:-1]
            continue
        merged.append(buf)
        buf = None
    if buf is not None:
        merged.append(buf)
    return merged


def norm(tok):
    t = tok.strip()
    while t and t[0] in "$'\"`":
        t = t[1:]
    while t and t[-1] in "'\"`,;)]}>":
        t = t[:-1]
    return t


def flag_key(tok):
    """Return ('long', base) / ('short', char) for a flag token, else None."""
    if tok.startswith("--") and len(tok) > 2:
        return ("long", tok.split("=")[0][2:])
    m = SHORT_RE.match(tok)
    if m:
        return ("short", m.group(1))
    return None


class Surface:
    def __init__(self, data):
        cli = data["cli"]
        self.global_longs = {f["long"] for f in cli["global_flags"] if f["long"]}
        self.global_shorts = {f["short"] for f in cli["global_flags"] if f["short"]}
        sub_longs, sub_shorts = {}, {}
        self.sub_names = set()
        nested = []
        for c in cli["subcommands"]:
            self.sub_names.add(c["name"])
            sub_longs.setdefault(c["name"], set())
            sub_shorts.setdefault(c["name"], set())
            for f in c["flags"]:
                if f["long"]:
                    sub_longs[c["name"]].add(f["long"])
                if f["short"]:
                    sub_shorts[c["name"]].add(f["short"])
            for a in c["aliases"]:
                self.sub_names.add(a)
            for s in c.get("subcommands", []):
                nested.append((c["name"], s))
        for parent, s in nested:
            key = f"{parent} {s['name']}"
            self.sub_names.add(key)
            sub_longs[key], sub_shorts[key] = set(), set()
            for a in s["aliases"]:
                self.sub_names.add(f"{parent} {a}")
                self.sub_names.add(a)
            for f in s["flags"]:
                if f["long"]:
                    sub_longs[key].add(f["long"])
                if f["short"]:
                    sub_shorts[key].add(f["short"])
        self.sub_longs = sub_longs
        self.sub_shorts = sub_shorts
        self.tui_commands = set(data.get("tui", {}).get("commands", []))

    def flag_ok(self, sub, kind, value):
        if kind == "long":
            return value in self.global_longs or value in self.sub_longs.get(sub, set())
        return value in self.global_shorts or value in self.sub_shorts.get(sub, set())


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--docs-dir", default="docs",
                    help="markdown tree to check (default: docs, relative to repo root)")
    ap.add_argument("--surface", default=DEFAULT_SURFACE)
    args = ap.parse_args()

    docs_dir = args.docs_dir
    if not os.path.isabs(docs_dir):
        docs_dir = os.path.join(ROOT, docs_dir)
    if not os.path.isdir(docs_dir):
        print(f"check-docs-surface: not a directory: {docs_dir}", file=sys.stderr)
        sys.exit(1)

    surface = Surface(load_surface(args.surface))
    errors = []

    md_files = []
    for dirpath, _dirnames, filenames in os.walk(docs_dir):
        for fn in sorted(filenames):
            if fn.endswith(".md"):
                md_files.append(os.path.join(dirpath, fn))
    md_files.sort()

    for fpath in md_files:
        with open(fpath, encoding="utf-8") as fh:
            text = fh.read()
        rel = os.path.relpath(fpath, ROOT)
        outside, fences = split_fences(text)

        # ---- command / flag existence (fenced blocks only) ----
        for fence in fences:
            if not is_shell_fence(fence["lang"]):
                continue
            for offset, line in enumerate(merge_continuations(fence["lines"])):
                tokens = line.split()
                if not tokens:
                    continue
                # skip leading shell-prompt tokens ($, >, etc.)
                i = 0
                while i < len(tokens) and norm(tokens[i]) == "":
                    i += 1
                if i >= len(tokens):
                    continue
                first = norm(tokens[i])
                lineno = fence["start"] + offset + 1
                if re.match(r"^/[a-z][\w-]*$", first) and first not in ("/bin",):
                    if first not in surface.tui_commands:
                        errors.append(
                            f"{rel}:{lineno}: unknown TUI slash command '{first}'"
                        )
                n = len(tokens)
                while i < n:
                    t = norm(tokens[i])
                    if t == "nanosb":
                        j = i + 1
                        while j < n:
                            tk = norm(tokens[j])
                            if tk == "--" or not (tk.startswith("--") or SHORT_RE.match(tk)):
                                break
                            fkey = flag_key(tk)
                            if fkey and not surface.flag_ok("__global__", *fkey):
                                errors.append(
                                    f"{rel}:{lineno}: unknown global flag '{tk}'"
                                )
                            j += 1
                        if j < n and norm(tokens[j]) not in ("--", "&&", ";", "|", "||"):
                            sub = norm(tokens[j])
                            j += 1
                            if sub not in surface.sub_names:
                                errors.append(
                                    f"{rel}:{lineno}: unknown subcommand "
                                    f"'{sub}' in fenced block: {line.strip()}"
                                )
                            else:
                                k = j
                                while k < n and norm(tokens[k]) != "--":
                                    tk = norm(tokens[k])
                                    if tk in ("&&", ";", "|", "||", "\\"):
                                        break
                                    fkey = flag_key(tk)
                                    if fkey and not surface.flag_ok(sub, *fkey):
                                        errors.append(
                                            f"{rel}:{lineno}: unknown flag for "
                                            f"'nanosb {sub}': '{tk}'"
                                        )
                                    k += 1
                                j = k
                        i = j + 1
                        continue
                    i += 1

        # ---- link check (non-fence text only) ----
        doc = "\n".join(outside)
        doc = re.sub(r"`[^`]*`", "", doc)          # drop inline code spans
        doc = re.sub(r"<!--.*?-->", "", doc, flags=re.S)  # drop HTML comments
        for m in re.finditer(r"!?\[[^\]]*\]\(([^)\s]+)(?:[ \t]+[^)]*)?\)", doc):
            _check_target(m.group(1), fpath, rel, errors)
        for m in re.finditer(r"(?:^|\s)\[([^\]]+)\]:\s*<?([^<>\s]+)>?", doc):
            _check_target(m.group(2), fpath, rel, errors)

        # ---- platform guard (whole file) ----
        for lineno, line in enumerate(text.splitlines(), 1):
            if WIN_RE.search(line) and not ARCHIVED_RE.search(line):
                errors.append(
                    f"{rel}:{lineno}: Windows-support instruction present "
                    f"(Windows/WHPX/HCS/install.ps1); mark the support as "
                    f"archived/not supported on this line if intended"
                )

    for e in errors:
        print(f"FAIL {e}")
    print(f"check-docs-surface: {len(md_files)} markdown file(s) in {os.path.relpath(docs_dir, ROOT)}, "
          f"{len(errors)} error(s)")
    if errors:
        print("docs accuracy check FAILED")
        sys.exit(1)
    print("docs accuracy check passed")


def _check_target(target, fpath, rel, errors):
    tgt = target.strip()
    if tgt.startswith(("http://", "https://", "mailto:", "#", "{{")):
        return
    tgt = re.sub(r"^<|>$", "", tgt)
    tgt = tgt.split("#")[0].rstrip("/")
    if not tgt:
        return
    resolved = os.path.normpath(os.path.join(os.path.dirname(fpath), tgt))
    if not os.path.exists(resolved):
        errors.append(f"{rel}: dead relative link '{target}' -> {os.path.relpath(resolved, ROOT)}")


if __name__ == "__main__":
    main()
