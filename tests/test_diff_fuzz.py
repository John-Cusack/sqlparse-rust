"""Differential equivalence: facade (Rust kernel) vs pure-Python fallback.

Toggles `SQLPARSE_PURE` per call (read fresh by `_bridge.use_rust`), so
both paths run in one process against the same vendored 0.6.0 tree.
Compares statement counts, full tree shapes (class/ttype/value), parent
links, `get_type`, `split`, and `format` across an option matrix.
"""

import os
import random

import pytest

import sqlparse

EDGE = [
    "",
    " ",
    "\n",
    ";",
    ";;;",
    "select 1",
    "select * from foo;",
    "SELECT a, b, c FROM t WHERE x > 1 AND y < 2 ORDER BY z;",
    "select a.b, t.* from sch.t as t;",
    "INSERT INTO t VALUES (1, 'a'), (2, 'b');",
    "UPDATE t SET a=1, b='x' WHERE id IN (1,2,3);",
    "DELETE FROM t WHERE EXISTS (SELECT 1 FROM u);",
    "CREATE TABLE t (a INT PRIMARY KEY, b VARCHAR(20) NOT NULL);",
    "CREATE OR REPLACE VIEW v AS SELECT 1;",
    "WITH cte AS (SELECT 1) SELECT * FROM cte;",
    "SELECT CASE WHEN a THEN 1 WHEN b THEN 2 ELSE 3 END FROM t;",
    "SELECT CASE x WHEN 1 THEN 'a' END;",
    "SELECT f(a, b), g(*) OVER (PARTITION BY x ORDER BY y) FROM t;",
    "SELECT CAST(x AS INT), y::TEXT, z AT TIME ZONE 'UTC' FROM t;",
    "SELECT * FROM a LEFT JOIN b ON a.i=b.i RIGHT OUTER JOIN c USING (i);",
    "SELECT * FROM t WHERE x BETWEEN 1 AND 5 AND y NOT LIKE 'a%';",
    "SELECT a AS x, b y, t.* FROM tbl AS t GROUP BY x HAVING COUNT(*)>1;",
    "BEGIN; SELECT 1; COMMIT;",
    "BEGIN TRANSACTION; SELECT 1;",
    "select $tag$body$tag$; select /* c */ 1; -- tail",
    "$$q$$; $a$b$c$a$;",
    "/*+ hint */ SELECT /*+ x */ 1;",
    "--+ hint\nselect 1",
    "select 'it''s', 'a\\'b', \"sym\", `bt`, ´q´ from t;",
    "select 1E5, 0x1F, .5, 5., -3, 1.5e-3 from t;",
    "select @v, :n, ?, %s, %(n)s, #t, ##t from t;",
    "select a+b*c/d-e, x||y, a AND b OR NOT c from t;",
    "select a->b, c#>d, e@>f, g?h from t;",
    "GO 2\ngo\nselect 1",
    "IF NOT EXISTS (SELECT 1) SELECT 2;",
    "FOR i IN 1..3 LOOP SELECT i; END LOOP;",
    "a := 1; b := f(x);",
    "values (1, 2), (3, 4);",
    "select null, NULL, nUlL from t;",
    "select ÀÉ, ÿ, K, ſ, ı, İ, Σ, Þ, àü, ٣ from t;",
    "select 'ÀÉſ', \"Ÿ\" from t where x='ſ';",
    "select (select max(x) from u) from t;",
    "select * from (select 1 as a) sub join (select 2 as b) s2 on true;",
    "select 1; -- comment only\n; select 2;",
    "\u00a0select\u20031\u2009;\u0085",
    "select 'a\nb', \"c\rd\" from t;",
    "select \"a\"\"b\", 'c''d' from t;",
    "select 'a\\', 'b\\\\' from t;",
    "select x from t limit 10 offset 5;",
    "select distinct top 5 * from t;",
    "handler read t first;",
    "lateral view explode(arr) t as x;",
    "select double precision '1.0';",
    "select interval '1 day' + interval '2 hours';",
    "select timestamp '2020-01-01', date '2020-01-02';",
    "select * from t where (a=1 and b=2) or c is null;",
    "select decode(a, 1, 'x', 'y') from t;",
    "select convert(varchar, a), cast(b as int) from t;",
]

FRAGMENTS = [
    "select", "from", "where", "and", "or", "not", "as", "join", "on",
    "group by", "order by", "limit", "values", "insert into", "update",
    "set", "create table", "delete from", "union all", "having", "case",
    "when", "then", "else", "end", "between", "like", "in", "is null",
    "a", "foo", "t1", "col_1", "_x", "t.*", "a.b", "*", "1", "42",
    "'s'", "'it''s'", "'a\\'b'", "3.14", ".5", "1e10", "0x1F", '"sym"',
    "`bt`", "@v", ":n", "?", "=", "<>", "!=", "<", ">", "+", "-", "/",
    "%", "::", ":=", "->", ",", "(", ")", ";", ".", " ",
    "\n", "-- c", "/* m */", "$$q$$", "$tag$b$tag$",
    "(SELECT 1)", "CASE WHEN a THEN 1 END",
    "ÀÉ", "ÿ", "ſ", "٣", "Σ", "\u0301", "\u093e",
]

FORMATS = [
    {},
    {"keyword_case": "upper"},
    {"keyword_case": "lower"},
    {"keyword_case": "capitalize"},
    {"identifier_case": "upper"},
    {"identifier_case": "lower"},
    {"truncate_strings": 3},
    {"truncate_strings": 5, "truncate_char": ".."},
    {"use_space_around_operators": True},
    {"strip_comments": True},
    {"strip_whitespace": True},
    {"strip_comments": True, "strip_whitespace": True},
    {"reindent": True},
    {"reindent": True, "indent_width": 4},
    {"reindent": True, "indent_tabs": True},
    {"reindent": True, "indent_after_first": True},
    {"reindent": True, "indent_columns": True},
    {"reindent": True, "wrap_after": 10},
    {"reindent": True, "comma_first": True},
    {"reindent": True, "compact": True},
    {"reindent_aligned": True},
    {"reindent": True, "reindent_aligned": True},
    {"reindent": True, "keyword_case": "upper", "strip_comments": True},
    {
        "keyword_case": "upper",
        "identifier_case": "lower",
        "strip_comments": True,
        "reindent": True,
        "wrap_after": 20,
    },
    {"output_format": "python"},
    {"output_format": "php"},
    {"reindent": True, "output_format": "python"},
    {"strip_comments": True, "output_format": "php"},
]


def rust(fn, *a, **k):
    os.environ.pop("SQLPARSE_PURE", None)
    assert not os.environ.get("SQLPARSE_PURE")
    from sqlparse import _bridge

    assert _bridge.use_rust(), "rust path not active"
    return fn(*a, **k)


def pure(fn, *a, **k):
    os.environ["SQLPARSE_PURE"] = "1"
    try:
        return fn(*a, **k)
    finally:
        del os.environ["SQLPARSE_PURE"]


def shape(node):
    if node.is_group:
        return (
            type(node).__name__,
            None,
            None,
            [shape(t) for t in node.tokens],
        )
    return (type(node).__name__, str(node.ttype), node.value, [])


def check_parents(node, parent=None):
    assert node.parent is parent
    if node.is_group:
        for t in node.tokens:
            check_parents(t, node)


def check(sql):
    r, p = rust(sqlparse.parse, sql), pure(sqlparse.parse, sql)
    assert len(r) == len(p), f"stmt count {sql!r}"
    for rs, ps in zip(r, p):
        assert str(rs) == str(ps), f"str {sql!r}"
        assert rs.get_type() == ps.get_type(), f"get_type {sql!r}"
        assert shape(rs) == shape(ps), f"shape {sql!r}"
        check_parents(rs)
        check_parents(ps)


def check_format(sql, **kw):
    r, p = rust(sqlparse.format, sql, **kw), pure(sqlparse.format, sql, **kw)
    assert r == p, f"format {kw} {sql!r}:\n rust={r!r}\n pure={p!r}"


def check_split(sql, **kw):
    assert rust(sqlparse.split, sql, **kw) == pure(sqlparse.split, sql, **kw), f"split {sql!r}"


def gen(rng, n):
    return "".join(rng.choice(FRAGMENTS) for _ in range(n))


def test_edge_parse():
    for sql in EDGE:
        check(sql)


def test_generated_parse():
    rng = random.Random(0x515345)
    for i in range(600):
        check(gen(rng, 1 + i % 30))


def test_larger_generated_parse():
    rng = random.Random(0xF0771E)
    for i in range(150):
        check(gen(rng, 30 + i % 90))


def test_edge_format_matrix():
    for sql in EDGE:
        for kw in FORMATS:
            check_format(sql, **kw)


def test_generated_format_matrix():
    rng = random.Random(0xBEEC)
    subset = FORMATS[::3]
    for i in range(150):
        sql = gen(rng, 3 + i % 20)
        for kw in subset:
            check_format(sql, **kw)


def test_split_matrix():
    for sql in EDGE:
        check_split(sql)
        check_split(sql, strip_semicolon=True)
    rng = random.Random(0x5EED)
    for i in range(120):
        sql = gen(rng, 2 + i % 16)
        check_split(sql)
        check_split(sql, strip_semicolon=True)


def test_error_parity():
    from sqlparse.exceptions import SQLParseError

    dos = [
        "SELECT " + "(" * 200 + "1" + ")" * 200,
        "SELECT " + ", ".join(f"col{i}" for i in range(15000)) + " FROM t",
    ]
    for sql in dos:
        for fn in (lambda s: sqlparse.format(s, reindent=True), sqlparse.parse):
            with pytest.raises(SQLParseError) as re_:
                rust(fn, sql)
            with pytest.raises(SQLParseError) as pe_:
                pure(fn, sql)
            assert str(re_.value) == str(pe_.value)
    with pytest.raises(NotImplementedError):
        rust(sqlparse.format, "select 1", right_margin=20)
    with pytest.raises(NotImplementedError):
        pure(sqlparse.format, "select 1", right_margin=20)
