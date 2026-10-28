"""Rust fast-path bridge (port addition, not upstream code).

Routes the default-configuration entry points (`parse`, `parsestream`,
`Lexer.get_tokens`) through `sqlparse._sqlparse` with exactly one string
crossing each way. Everything else — tree classes, filters, validation,
the pure-Python fallback — is byte-identical upstream code.

Fallback triggers (all documented in the mirror README seam section):
- the extension is not importable,
- `SQLPARSE_PURE` is set in the environment,
- the default `Lexer` singleton was reconfigured (`clear`,
  `set_SQL_REGEX`, `add_keywords` with non-default arguments),
- the input is not `str`/`bytes`/text-stream coercible (the pure path
  raises the original `TypeError`).
"""

import io
import os

_TTYPE_CACHE = {}
_CLASS_CACHE = {}


def coerce_text(stream, encoding=None):
    """Mirror of `Lexer.get_tokens` input normalization.

    Returns the `str` or `None` when the input is not coercible (the pure
    path then raises the original `TypeError`).
    """
    if isinstance(stream, io.TextIOBase):
        stream = stream.read()
    if isinstance(stream, str):
        return stream
    if isinstance(stream, bytes):
        if encoding:
            return stream.decode(encoding)
        try:
            return stream.decode("utf-8")
        except UnicodeDecodeError:
            return stream.decode("unicode-escape")
    return None


def core_available():
    try:
        from sqlparse import _sqlparse  # noqa: F401
    except ImportError:
        return False
    return True


def lexer_pristine():
    """True when the default lexer singleton holds the default
    configuration (same comparison the upstream methods would produce)."""
    from sqlparse import keywords
    from sqlparse.lexer import Lexer

    inst = Lexer._default_instance
    if inst is None:
        return True
    default_dicts = [
        keywords.KEYWORDS_COMMON,
        keywords.KEYWORDS_ORACLE,
        keywords.KEYWORDS_MYSQL,
        keywords.KEYWORDS_PLPGSQL,
        keywords.KEYWORDS_HQL,
        keywords.KEYWORDS_MSACCESS,
        keywords.KEYWORDS_SNOWFLAKE,
        keywords.KEYWORDS_BIGQUERY,
        keywords.KEYWORDS,
    ]
    if inst._keywords != default_dicts:
        return False
    # `_SQL_REGEX` holds bound `Pattern.match` methods; compare the
    # underlying patterns plus ttypes.
    if [rx.__self__.pattern for rx, _ in inst._SQL_REGEX] != [rx for rx, _ in keywords.SQL_REGEX]:
        return False
    if [tt for _, tt in inst._SQL_REGEX] != [tt for _, tt in keywords.SQL_REGEX]:
        return False
    return True


def use_rust():
    if os.environ.get("SQLPARSE_PURE"):
        return False
    return core_available() and lexer_pristine()


def resolve_ttype(path):
    """Canonical dotted path (as emitted by the core) to the tokens object."""
    try:
        return _TTYPE_CACHE[path]
    except KeyError:
        pass
    from sqlparse import tokens

    obj = tokens.Token
    for part in path.split(".")[1:]:
        obj = getattr(obj, part)
    _TTYPE_CACHE[path] = obj
    return obj


def assemble(node):
    """Build genuine `sql.py` objects from one core `(kind, payload)` tuple."""
    from sqlparse import sql

    kind, payload = node
    if isinstance(payload, list):
        try:
            cls = _CLASS_CACHE[kind]
        except KeyError:
            cls = getattr(sql, kind)
            _CLASS_CACHE[kind] = cls
        return cls([assemble(c) for c in payload])
    return sql.Token(resolve_ttype(kind), payload)
