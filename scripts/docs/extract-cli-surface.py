#!/usr/bin/env python3
"""extract-cli-surface.py — derive the machine-readable CLI + TUI surface.

Reads the clap definitions in ``src/main.rs`` (subcommands, long/short flags,
global flags, aliases, nested subcommands) and the TUI slash-command surface in
``src/tui/commands.rs`` (``HELP_ENTRIES`` + the slash-command parser literals)
and writes ``scripts/docs/cli-surface.json``.

Deterministic: the same source files always produce the same JSON (no
timestamps, keys sorted). Used by ``scripts/check-docs.sh`` and consumed as a CI
artifact by the website repo. Dependency-light: Python 3 standard library only.

Usage: python3 scripts/docs/extract-cli-surface.py [OUTPUT_PATH]
"""

import json
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
CLI_RS = os.path.join(ROOT, "src", "main.rs")
TUI_RS = os.path.join(ROOT, "src", "tui", "commands.rs")
DEFAULT_OUT = os.path.join(ROOT, "scripts", "docs", "cli-surface.json")


def fail(msg: str) -> "NoReturn":
    print(f"extract-cli-surface: {msg}", file=sys.stderr)
    sys.exit(1)


def read(path: str) -> str:
    try:
        with open(path, encoding="utf-8") as fh:
            return fh.read()
    except OSError as exc:
        fail(f"cannot read {path}: {exc}")


def kebab(name: str) -> str:
    return re.sub(r"(?<!^)(?=[A-Z])", "-", name).lower()


# --------------------------------------------------------------------------
# Rust tokenizer (good enough for the clap surface; not a full Rust parser)
# --------------------------------------------------------------------------

def tokenize(text: str):
    """Yield (kind, value) tokens: ATTR / IDENT / PUNCT / STR / CHAR / OTHER."""
    tokens = []
    i, n = 0, len(text)
    while i < n:
        c = text[i]
        if c.isspace():
            i += 1
        elif text.startswith("//", i) or text.startswith("/*", i):
            if text.startswith("//", i):
                j = text.find("\n", i)
                i = n if j < 0 else j + 1
            else:
                j = text.find("*/", i + 2)
                i = n if j < 0 else j + 2
        elif text.startswith("#[", i):
            depth, j = 0, i
            while j < n:
                if text[j] == "[":
                    depth += 1
                elif text[j] == "]":
                    depth -= 1
                    if depth == 0:
                        break
                j += 1
            tokens.append(("ATTR", text[i : j + 1]))
            i = j + 1
        elif c in "{}(),;:":
            tokens.append(("PUNCT", c))
            i += 1
        elif c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                if text[j] == "\\":
                    j += 1
                j += 1
            tokens.append(("STR", text[i + 1 : j]))
            i = min(j + 1, n)
        elif c == "'":
            j = i + 1
            while j < n and text[j] != "'":
                if text[j] == "\\":
                    j += 1
                j += 1
            tokens.append(("CHAR", text[i + 1 : j]))
            i = min(j + 1, n)
        else:
            m = re.match(r"[A-Za-z_][A-Za-z0-9_]*", text[i:])
            if m:
                tokens.append(("IDENT", m.group(0)))
                i += len(m.group(0))
            else:
                tokens.append(("OTHER", c))
                i += 1
    return tokens


def attr_inner(attr: str) -> str:
    return attr[2:-1].strip()


def parse_arg_attr(attr_inner_text: str, field_name: str):
    """E.g. ``arg(short = 'r', long = 'resume', global = true)`` -> dict."""
    long = short = None
    m = re.search(r'long\s*=\s*"([^"]+)"', attr_inner_text)
    if m:
        long = m.group(1)
    elif re.search(r'\blong\b', attr_inner_text):
        long = kebab(field_name)
    m = re.search(r"short\s*=\s*'([^'])'", attr_inner_text)
    if m:
        short = m.group(1)
    elif re.search(r'\bshort\b', attr_inner_text):
        short = field_name[0]
    return {"long": long, "short": short}


def parse_struct_body(tokens, i):
    """Parse ``{ pub name: Type, ... }`` starting at token ``i`` (which is '{').

    Returns (flags, nested_enum_type, token_index_after_closing_brace)."""
    depth, i = 1, i + 1
    fields, nested, pending = [], None, []
    while depth > 0 and i < len(tokens):
        k, v = tokens[i]
        if k == "ATTR":
            pending.append(v)
            i += 1
        elif k == "IDENT" and v == "pub":
            name = (
                tokens[i + 1][1]
                if i + 1 < len(tokens) and tokens[i + 1][0] == "IDENT"
                else None
            )
            i += 2
            type_idents = []
            while i < len(tokens):
                kk, vv = tokens[i]
                if kk == "PUNCT" and vv in (",", "}"):
                    break
                if kk == "IDENT":
                    type_idents.append(vv)
                i += 1
            inners = [attr_inner(a) for a in pending]
            pending = []
            arg = next((x for x in inners if x.startswith("arg(")), None)
            if arg is not None and name is not None:
                flag = parse_arg_attr(arg[4:-1], name)
                if flag["long"] or flag["short"]:
                    fields.append(flag)
            if arg is None and any("subcommand" in x for x in inners) and type_idents:
                nested = type_idents[-1]
            if i >= len(tokens):
                break
            if tokens[i] == ("PUNCT", ","):
                i += 1
            elif tokens[i] == ("PUNCT", "}"):
                depth -= 1
                i += 1
        elif k == "PUNCT" and v == "}":
            depth -= 1
            i += 1
        else:
            i += 1
    return fields, nested, i


def parse_enum_body(tokens, i):
    """Parse ``pub enum X { Variant, Variant { fields }, ... }`` at '}' start.

    Returns a list of variant dicts: {name, attrs, fields, nested}."""
    depth, i = 1, i + 1
    variants, pending, current = [], [], None
    while depth > 0 and i < len(tokens):
        k, v = tokens[i]
        if k == "ATTR":
            pending.append(v)
            i += 1
        elif k == "PUNCT" and v == "{":
            if current is None:
                fail("unexpected '{' in enum body")
            fields, nested, i = parse_struct_body(tokens, i)
            current["fields"] = fields
            current["nested"] = nested
            variants.append(current)
            current = None
        elif k == "PUNCT" and v == ",":
            if current is not None:
                variants.append(current)
                current = None
            i += 1
        elif k == "PUNCT" and v == "}":
            if current is not None:
                variants.append(current)
                current = None
            depth -= 1
            i += 1
        elif k == "IDENT" and depth == 1:
            if v in ("pub", "use", "fn", "const", "static", "impl", "mod",
                    "super", "self", "crate", "extern", "trait", "type"):
                i += 1
                continue
            if current is not None:
                fail(f"unexpected identifier in enum body: {v}")
            current = {"name": v, "attrs": pending, "fields": [], "nested": None}
            pending = []
            i += 1
        else:
            i += 1
    return variants


def collect_defs(tokens):
    """Return {name: (kind, brace_token_index)} for every pub struct/enum."""
    items = {}
    i = 0
    while i < len(tokens) - 2:
        t0, t1, t2 = tokens[i], tokens[i + 1], tokens[i + 2]
        if (t0[0], t1[0], t2[0]) == ("IDENT", "IDENT", "IDENT") and \
                t0[1] == "pub" and t1[1] in ("struct", "enum"):
            kind, name = t1[1], t2[1]
            j = i + 3
            while j < len(tokens) and tokens[j] != ("PUNCT", "{"):
                j += 1
            if j >= len(tokens):
                fail(f"cannot find body of pub {kind} {name}")
            items[name] = (kind, j)
            i = j + 1
            continue
        i += 1
    return items


def command_name(variant):
    """Resolve clap name override, aliases, and hidden flag for a variant."""
    name, aliases, hidden = None, [], False
    for a in variant["attrs"]:
        m = re.match(r"^\s*(?:clap|command)\(([\s\S]*)\)\s*[\s]*$", attr_inner(a))
        if not m:
            continue
        body = m.group(1)
        m2 = re.search(r'name\s*=\s*"([^"]+)"', body)
        if m2:
            name = m2.group(1)
        aliases.extend(re.findall(r'alias\s*=\s*"([^"]+)"', body))
        if re.search(r"hide\s*=\s*true", body):
            hidden = True
    if name is None:
        name = kebab(variant["name"])
    return name, aliases, hidden


def build_cli(tokens):
    items = collect_defs(tokens)

    if "Cli" not in items:
        fail("src/main.rs: no `pub struct Cli` clap definition found")
    _, brace = items["Cli"]
    global_flags, _, _ = parse_struct_body(tokens, brace)

    if "Commands" not in items or items["Commands"][0] != "enum":
        fail("src/main.rs: no `pub enum Commands` clap subcommand set found")
    _, brace = items["Commands"]
    enum_variants = {name: parse_enum_body(tokens, idx)
                     for name, (kind, idx) in items.items() if kind == "enum"}

    subcommands = []
    for var in enum_variants["Commands"]:
        name, aliases, hidden = command_name(var)
        cmd = {
            "name": name,
            "aliases": aliases,
            "hidden": hidden,
            "flags": var["fields"],
            "subcommands": [],
        }
        if var["nested"] and var["nested"] in enum_variants:
            for sub in enum_variants[var["nested"]]:
                sname, saliases, shidden = command_name(sub)
                cmd["subcommands"].append({
                    "name": sname,
                    "aliases": saliases,
                    "hidden": shidden,
                    "flags": sub["fields"],
                    "subcommands": [],
                })
        subcommands.append(cmd)

    alias_of = {}
    for c in subcommands:
        for a in c["aliases"]:
            alias_of[a] = c["name"]
        for s in c["subcommands"]:
            for a in s["aliases"]:
                alias_of[a] = s["name"]

    return {
        "prog": "nanosb",
        "global_flags": global_flags,
        "subcommands": subcommands,
        "alias_of": alias_of,
    }


def build_tui(text):
    """Slash commands: base name of every '/...' string literal in the TUI
    source (HELP_ENTRIES patterns, ALL_COMMANDS autocomplete list, and the
    slash-command parser match arms all advertise real commands)."""
    strings = re.findall(r'"(?:\\.|[^"\\])*"', text)
    bases = []
    for s in strings:
        lit = s[1:-1]
        if lit.startswith("/"):
            bases.append(lit.split()[0])

    entries = []
    m = re.search(r"pub\s+static\s+HELP_ENTRIES:[\s\S]*?=\s*&?\s*\[([\s\S]*?)\];", text)
    if m:
        for e in re.findall(r"CommandHelpEntry\s*\{([^{}]*)\}", m.group(1)):
            p = re.search(r'pattern:\s*"((?:[^"\\]|\\.)*)"', e)
            d = re.search(r'description:\s*"((?:[^"\\]|\\.)*)"', e)
            entries.append({
                "pattern": p.group(1) if p else "",
                "description": d.group(1) if d else "",
            })

    return {
        "commands": sorted(set(bases)),
        "help_entries": entries,
    }


def main():
    out = sys.argv[1] if len(sys.argv) > 1 else DEFAULT_OUT
    surface = {
        "about": "Machine-readable CLI + TUI surface for docs-accuracy checks.",
        "sources": {
            "cli": "src/main.rs",
            "tui": "src/tui/commands.rs",
            "extractor": "scripts/docs/extract-cli-surface.py",
        },
        "cli": build_cli(tokenize(read(CLI_RS))),
        "tui": build_tui(read(TUI_RS)),
    }
    os.makedirs(os.path.dirname(out) or ".", exist_ok=True)
    with open(out, "w", encoding="utf-8") as fh:
        json.dump(surface, fh, indent=2, sort_keys=True)
        fh.write("\n")
    print(f"extract-cli-surface: wrote {os.path.relpath(out, ROOT)}")


if __name__ == "__main__":
    main()
