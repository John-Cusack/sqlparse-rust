"""Lexer parity: `sqlparse._sqlparse.tokenize_flat` vs vendored pure lexer.

Runs the installed facade's own pure `sqlparse.lexer.tokenize` as oracle
(same 0.6.0 tree the fallback path uses). Deterministic: fixed edge cases
+ seeded generated inputs (keywords, non-ASCII identifiers, numbers,
quoted strings, comments, dollar quotes, DoS shapes).
"""

import random
import sys

import pytest

from sqlparse import _sqlparse
from sqlparse import lexer as pure_lexer

EDGE = [
    "",
    " ",
    "\n",
    "\r\nselect 1;\r",
    "select * from foo;",
    ".select",
    "select a.b from t",
    "select [a] from t",
    "select x[a] from t",
    "select 01a from t",
    "select .03 from t",
    "select 1E5, 1e-5, 2.5E-3, -0xFF, -5, 5., .5 from t",
    "select * from t where x between 1 and 2",
    "-- c\nselect 1",
    "# c\nselect 1",
    "--+ hint\nselect 1",
    "/* comment */ select 1",
    "/*+ hint */ select 1",
    "/**/ select 1",
    "/*/ select 1",
    "select $$dollar $tag$ body $tag$$$ end",
    "$$ $$",
    "select $a$x, $1$y from t",
    "select 'it''s', 'a\\'b', \"sym\"\", \"a\" from t",
    "select `a``b`, ´c´ from t",
    "@var, ##tmp, #temp, :name, $1, ?, %s, %(n)s from t",
    "\\dt, \\x from t",
    "select CASE WHEN a THEN 1 ELSE 0 END, CASEVALUE from t",
    "select * from t1 LEFT JOIN t2 ON t1.i=t2.i RIGHT OUTER JOIN t3 USING (i)",
    "select * from t1 NATURAL JOIN t2 CROSS JOIN t3 STRAIGHT_JOIN t4",
    "IF NOT EXISTS, NOT NULL, NULLS FIRST, ASC NULLS LAST DESC from t",
    "UNION ALL, GROUP BY, ORDER BY, PRIMARY KEY, HANDLER FOR from t",
    "CREATE OR REPLACE VIEW v AS SELECT 1",
    "DOUBLE PRECISION, LATERAL VIEW EXPLODE(a) t AS x from t",
    "AT TIME ZONE 'UTC', WITH' TIME ZONE 'X' from t",
    "x NOT LIKE 'a%', y ILIKE 'b', z REGEXP BINARY 'c' from t",
    "a->'b', c #> '{d}', e @> f, g <@ h, i ? j, k ?| l, m ?& n, o #- p from t",
    "a<>=~!b, c +/@#%^&|- d, e::int, f:=g from t",
    "GO 2\ngo\nselect 1",
    "BEGIN; select 1; END; select 2",
    "select ÀÉÎ, Ÿ, K, ſ, ı, İ, Σ, Þ, àü from t",
    "select 'ÀÉ' , \"Ÿ\" from t where x='ſ'",
    "select ٣, \"٣\" from t",
    "--c1\n--c2\nselect 1 -- trailing",
    "select 1;select 2; select 3;",
    "select (a), f(x, y), [i], func() OVER (PARTITION BY x) from t",
    "insert into t values (1, 'a'), (2, 'b');",
    "update t set a=1 where b=2; -- x",
    "select * from t where (a=1 and b=2) or c is null;",
    "select a as x, b y, t.* from tbl as t group by x having count(*)>1;",
    "select 1 /* unclosed comment",
    "select $tag$unclosed dollar",
    "select 'unclosed string",
    'select "unclosed sym',
    "select `unclosed bt",
    "select 'a\\\\' from t",
    "select \"a\\\"\" from t",
    "select 0x, 0xZZ, 1E, 1E+, .e5, 5E- from t",
    "FROMX, SELECTX, ENDING, GO2, GOO, NULLSX from t",
    "from-foo, a-b, x::int::text from t",
    "\u00a0select\u20031\u2009;\u0085",
]

FRAGMENTS = [
    "select", "SELECT", "SeLeCt", "from", "FROM", "where", "WHERE",
    "insert into", "update", "delete from", "create table", "and", "or",
    "not", "null", "NULL", "as", "AS", "join", "JOIN", "left join",
    "on", "group by", "order by", "limit", "values", "set",
    "a", "foo", "t1", "col_1", "_x", "ÀÉ", "ÿ", "ſ", "t",
    "*", "a.b", "t.*", "[col]", "@v", ":n", "?",
    "1", "42", "-7", "3.14", ".5", "5.", "1e10", "1E-5", "0x1F", "-0xA",
    "::", ":=", "->", "->>", "#>", "@>", "?", "?|", "&",
    "(", ")", ",", ";", ".",
    " ", "  ", "\n", "\r\n", "\t", " \n ",
    "-- c", "# c", "--+ h", "/* m */", "/*+ h */",
    "$$q$$", "$tag$body$tag$",
    "(", "(SELECT 1)", "CASE WHEN a THEN 1 END",
]

OTHER_FRAGMENTS = [
    "\u00e0", "\u00ff", "\u00de", "\u00fe", "\u03a3", "\u03c2", "\u03c3",
    "\u212a", "\u017f", "\u0130", "\u0131", "\u00df", "\u00aa", "\u02b0",
    "\u2167", "\u00b2", "\u0301", "\u093e", "\u203f", "\u200c", "\u00b7",
    "\u00a0", "\u2003", "\u0085", "\u000b", "\u000c",
    "é", "ü", "ñ", "ü_", "_ü", "aé", "é9",
]


def gen(rng, n):
    return "".join(rng.choice(FRAGMENTS + OTHER_FRAGMENTS) for _ in range(n))


def check(sql):
    want = [(str(t), v) for t, v in pure_lexer.tokenize(sql)]
    got = _sqlparse.tokenize_flat(sql)
    assert got == want, f"divergence on {sql!r}:\n pure={want}\n rust={got}"


def test_edge_cases():
    for sql in EDGE:
        check(sql)


def test_generated():
    rng = random.Random(0x515345)
    for i in range(400):
        check(gen(rng, 1 + i % 24))
    for i in range(120):
        check(gen(rng, 25 + i % 60))
