#!/usr/bin/env python3
"""Generate sqlparse-core/src/tables_gen.rs from installed sqlparse 0.6.0.

Provenance: andialbrecht/sqlparse 0.6.0 (BSD-3-Clause), `pip install
sqlparse==0.6.0`. Re-run: `python3 scripts/gen_tables.py` from the repo
root with sqlparse 0.6.0 importable. The output is checked in; drift is
caught by `test_tables_provenance` (pattern count + spot checks).

Emits:
  - `TType` enum: every token-type path referenced anywhere in the
    upstream tree (source scan of `T./tokens./Token.` uses + full walk of
    the tokens module), variant `TOKEN_<PARTS...>`, stable order (sorted).
  - `KEYWORDS`: merged default keyword map with first-dict-wins order
    (KEYWORDS_COMMON, ORACLE, MYSQL, PLPGSQL, HQL, MSACCESS, SNOWFLAKE,
    BIGQUERY, KEYWORDS) as (UPPER, TType) pairs; conflicting duplicates
    listed in a comment.
  - `PATTERN_SOURCES`: the 50 SQL_REGEX sources in order, with the
    PROCESS_AS_KEYWORD entry marked (hand-written matchers in lexer.rs
    implement these; the sources ride along for the provenance test).
"""

import pathlib
import re
import sys

import sqlparse.keywords as K

ROOT = pathlib.Path(__file__).resolve().parent.parent
OUT = ROOT / "sqlparse-core" / "src" / "tables_gen.rs"

SRC_DIR = pathlib.Path(K.__file__).resolve().parent
USE_RE = re.compile(r"\bT\.([A-Za-z_][\w.]*)")
TOKENS_RE = re.compile(r"\btokens\.([A-Za-z_][\w.]*)")
TOKEN_RE = re.compile(r"\bToken\.([A-Za-z_][\w.]*)")


def canonical(obj):
    """Dotted path of a _TokenType via its parent chain (aliases collapse:
    tokens.DML -> Token.Keyword.DML). Names are read with vars(), never
    created."""
    import sqlparse.tokens as T
    names = []
    while obj is not None:
        parent = obj.parent
        if parent is None:
            names.append("Token")
        else:
            d = vars(parent)
            nm = next(n for n, v in d.items() if v is obj)
            names.append(nm)
        obj = parent
    return ".".join(reversed(names))


def collect_module_paths():
    """Canonical paths of every _TokenType reachable as a module attr of
    sqlparse.tokens (getattr on the MODULE never auto-vivifies)."""
    import sqlparse.tokens as T
    found = set()
    for name in dir(T):
        if name.startswith("_"):
            continue
        obj = getattr(T, name)
        if isinstance(obj, T._TokenType):
            found.add(canonical(obj))
    return found


def scan_sources():
    paths = set()
    for py in sorted(SRC_DIR.rglob("*.py")):
        text = py.read_text()
        for m in USE_RE.finditer(text):
            paths.add("Token." + m.group(1))
        for m in TOKENS_RE.finditer(text):
            p = m.group(1)
            if p == "_TokenType":
                continue
            paths.add("Token." + p if not p.startswith("Token.") else p)
        for m in TOKEN_RE.finditer(text):
            paths.add("Token." + m.group(1))
    return paths


def is_plausible(p):
    """Lexical junk filter: real ttype parts are non-empty and start
    uppercase (drops docstring `Token.Comment...`, `tokens.index(node)`)."""
    parts = p.split(".")
    if parts[0] != "Token" or len(parts) < 2:
        return False
    return all(part[:1].isupper() for part in parts[1:])


def main():
    import sqlparse
    assert sqlparse.__version__ == "0.6.0", sqlparse.__version__

    import sqlparse.tokens as T
    walked = collect_module_paths()
    scanned = scan_sources()

    # Resolve scanned refs to canonical paths. getattr here MAY vivify lazy
    # children (e.g. Comment.Multiline, created on first lex) — intended:
    # they are real code references; lexical junk is filtered first.
    resolved = set()
    for p in sorted(scanned):
        if not is_plausible(p):
            print(f"drop (junk): {p}")
            continue
        obj = T.Token
        for part in p.split(".")[1:]:
            obj = getattr(obj, part)
            if not isinstance(obj, T._TokenType):
                break
        else:
            resolved.add(canonical(obj))
            continue
        print(f"drop (not a ttype): {p}")
    paths = sorted(walked | resolved)

    # Nd ranges for is_digit (Python re \d = Nd exactly; Rust to_digit is
    # ASCII-only). Derived from unicodedata, emitted as closed ranges.
    import unicodedata
    nd_ranges = []
    start = None
    prev = None
    for cp in range(0x110000):
        ch = chr(cp)
        if unicodedata.category(ch) == "Nd":
            if start is None:
                start = prev = cp
            elif cp == prev + 1:
                prev = cp
            else:
                nd_ranges.append((start, prev))
                start = prev = cp
    if start is not None:
        nd_ranges.append((start, prev))

    # Python str \w = L* + N* + U+005F exactly (marks and other Pc
    # excluded; Rust is_alphanumeric over-covers Other_Alphabetic marks
    # like U+093E). Derived from unicodedata, emitted as closed ranges.
    w_ranges = []
    start = None
    prev = None
    for cp in range(0x110000):
        ch = chr(cp)
        if unicodedata.category(ch) in (
            "Lu", "Ll", "Lt", "Lm", "Lo", "Nd", "Nl", "No",
        ) or cp == 0x5F:
            if start is None:
                start = prev = cp
            elif cp == prev + 1:
                prev = cp
            else:
                w_ranges.append((start, prev))
                start = prev = cp
    if start is not None:
        w_ranges.append((start, prev))


    def variant(p):
        return "TOKEN_" + "_".join(part.upper() for part in p.split(".")[1:])

    assert len({variant(p) for p in paths}) == len(paths), "variant collision"

    # Merged keyword map, first-dict-wins.
    dicts = [K.KEYWORDS_COMMON, K.KEYWORDS_ORACLE, K.KEYWORDS_MYSQL,
             K.KEYWORDS_PLPGSQL, K.KEYWORDS_HQL, K.KEYWORDS_MSACCESS,
             K.KEYWORDS_SNOWFLAKE, K.KEYWORDS_BIGQUERY, K.KEYWORDS]
    merged = {}
    conflicts = []
    for d in dicts:
        for key, val in d.items():
            path = "Token." + ".".join(val)
            assert path in paths, path
            if key in merged:
                if merged[key] != path:
                    conflicts.append((key, merged[key], path))
            else:
                merged[key] = path

    lines = []
    lines.append("// PROVENANCE: generated by scripts/gen_tables.py from")
    lines.append("// andialbrecht/sqlparse 0.6.0 (BSD-3-Clause, see NOTICE).")
    lines.append("// DO NOT EDIT: re-run the generator. Upstream drift is caught")
    lines.append("// by test_tables_provenance (pattern count + spot checks).")
    lines.append("// Conflicting cross-dict keywords (first dict wins, as upstream):")
    for key, first, later in sorted(conflicts):
        lines.append(f"//   {key}: {first} shadows {later}")
    lines.append("")
    lines.append("#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]")
    lines.append("#[allow(non_camel_case_types)]")
    lines.append("#[repr(u16)]")
    lines.append("pub enum TType {")
    for p in paths:
        lines.append(f"    {variant(p)},")
    lines.append("}")
    lines.append("")
    lines.append("impl TType {")
    lines.append("    pub fn as_str(self) -> &'static str {")
    lines.append("        match self {")
    for p in paths:
        lines.append(f"            TType::{variant(p)} => \"{p}\",")
    lines.append("        }")
    lines.append("    }")
    lines.append("")
    lines.append("    pub fn from_path(path: &str) -> Option<TType> {")
    lines.append("        Some(match path {")
    for p in paths:
        lines.append(f"            \"{p}\" => TType::{variant(p)},")
    lines.append("            _ => return None,")
    lines.append("        })")
    lines.append("    }")
    lines.append("}")
    lines.append("")
    lines.append("/// Merged default keyword map (UPPER -> TType), first-dict-wins.")
    lines.append(f"/// {len(merged)} entries from {sum(len(d) for d in dicts)}.")
    lines.append("pub const KEYWORDS: &[(&str, TType)] = &[")
    for key in sorted(merged):
        lines.append(f"    (\"{key}\", TType::{variant(merged[key])}),")
    lines.append("];")
    lines.append("")
    lines.append("/// SQL_REGEX sources in order; `None` = PROCESS_AS_KEYWORD.")
    lines.append("pub const PATTERN_SOURCES: &[Option<&str>] = &[")
    for rx, tt in K.SQL_REGEX:
        lit = '"' + rx.replace("\\", "\\\\").replace('"', '\\"') + '"'
        if tt is K.PROCESS_AS_KEYWORD:
            lines.append(f"    None, // {lit}")
        else:
            lines.append(f"    Some({lit}), // Token.{'.'.join(tt)}")
    lines.append("];")
    lines.append("")
    lines.append("/// Closed General_Category=Nd ranges (Python `re` `\\d`).")
    lines.append(f"/// {sum(e - s + 1 for s, e in nd_ranges)} code points in "
                 f"{len(nd_ranges)} ranges, from unicodedata.")
    lines.append("pub const ND_RANGES: &[(u32, u32)] = &[")
    for s, e in nd_ranges:
        lines.append(f"    (0x{s:04X}, 0x{e:04X}),")
    lines.append("];")
    lines.append("")
    lines.append("/// Closed ranges of Python `str` `\\w` (L* + N* + `_`).")
    lines.append(f"/// {sum(e - s + 1 for s, e in w_ranges)} code points in "
                 f"{len(w_ranges)} ranges, from unicodedata.")
    lines.append("pub const W_RANGES: &[(u32, u32)] = &[")
    for s, e in w_ranges:
        lines.append(f"    (0x{s:04X}, 0x{e:04X}),")
    lines.append("];")
    lines.append("")
    OUT.write_text("\n".join(lines))
    print(f"wrote {OUT}: {len(paths)} ttypes, {len(merged)} keywords, "
          f"{len(K.SQL_REGEX)} patterns, {len(nd_ranges)} Nd ranges, "
          f"{len(w_ranges)} W ranges")


if __name__ == "__main__":
    sys.exit(main())
