"""Seam tests: fallback paths stay exact when the Rust fast path declines.

Covers `SQLPARSE_PURE`, missing extension, mutated default `Lexer`
(custom keywords / regex), `Lexer` subclasses, custom `FilterStack`
filters, and input coercion (bytes / streams / encodings / TypeError).
"""

import io
import os
import sys

import pytest

import sqlparse
from sqlparse import _bridge
from sqlparse.lexer import Lexer


def setup_function(_):
    os.environ.pop("SQLPARSE_PURE", None)
    Lexer._default_instance = None


def teardown_function(_):
    os.environ.pop("SQLPARSE_PURE", None)
    Lexer._default_instance = None


def test_pure_env_fallback():
    os.environ["SQLPARSE_PURE"] = "1"
    assert not _bridge.use_rust()
    assert len(sqlparse.parse("select 1")) == 1
    assert sqlparse.format("select 1", keyword_case="upper") == "SELECT 1"


def test_missing_extension_fallback(monkeypatch):
    # Simulate an environment without the compiled extension, regardless
    # of import state: block the module AND drop any parent attribute.
    monkeypatch.setitem(sys.modules, "sqlparse._sqlparse", None)
    monkeypatch.delattr(sqlparse, "_sqlparse", raising=False)
    assert not _bridge.use_rust()
    assert not _bridge.core_available()
    assert len(sqlparse.parse("select 1")) == 1

def test_custom_keywords_fallback():
    assert _bridge.use_rust()
    Lexer.get_default_instance().add_keywords({"FOOSELECT": sqlparse.tokens.Keyword})
    assert not _bridge.use_rust()
    stmts = sqlparse.parse("select FOOSELECT from t")
    kinds = [str(t.ttype) for t in stmts[0].flatten()]
    assert "Token.Keyword" in kinds


def test_custom_regex_fallback():
    import re

    assert _bridge.use_rust()
    inst = Lexer.get_default_instance()
    inst.set_SQL_REGEX([(r"FOO", sqlparse.tokens.Keyword)])
    assert not _bridge.use_rust()
    assert sqlparse.parse("select FOO")[0].get_type() == "UNKNOWN"


def test_lexer_subclass_pure_path():
    class MyLexer(Lexer):
        pass

    lx = MyLexer()
    lx.default_initialization()
    toks = list(lx.get_tokens("select 1"))
    assert toks[0] == (sqlparse.tokens.Keyword.DML, "select")


def test_custom_filterstack_filter():
    from sqlparse import engine, filters

    class UpperAll:
        def process(self, stream):
            for ttype, value in stream:
                yield ttype, value.upper()

    stack = engine.FilterStack()
    stack.preprocess.append(UpperAll())
    out = list(stack.run("select 1"))
    assert str(out[0]) == "SELECT 1"


def test_bytes_and_encodings():
    assert sqlparse.parse("select 1".encode("utf-8"))[0].get_type() == "SELECT"
    assert sqlparse.parse("select 1".encode("utf-16"), encoding="utf-16")[0].get_type() == "SELECT"
    # utf-8 bytes with non-utf8 content fall back to unicode-escape
    stmts = sqlparse.parse(b"select '\xff'")
    assert len(stmts) == 1


def test_stream_input():
    assert sqlparse.parse(io.StringIO("select 1; select 2"))[0].get_type() == "SELECT"
    assert len(list(sqlparse.parsestream(io.StringIO("select 1;")))) == 1


def test_type_error():
    with pytest.raises(TypeError):
        sqlparse.parse(123)


def test_pristine_after_reinit():
    Lexer.get_default_instance().clear()
    Lexer.get_default_instance().default_initialization()
    assert _bridge.use_rust()
