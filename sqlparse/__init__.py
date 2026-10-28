#
# Copyright (C) 2009-2020 the sqlparse authors and contributors
# <see AUTHORS file>
#
# This module is part of python-sqlparse and is released under
# the BSD License: https://opensource.org/licenses/BSD-3-Clause

"""Parse SQL statements."""

# Setup namespace
from collections.abc import Generator
from typing import IO, Any

from sqlparse import cli, engine, filters, formatter, sql, tokens

__version__ = "0.6.0"
__all__ = ["cli", "engine", "filters", "formatter", "sql", "tokens"]


def parse(
    sql: str, encoding: str | None = None
) -> tuple[sql.Statement, ...]:
    """Parse sql and return a list of statements.

    :param sql: A string containing one or more SQL statements.
    :param encoding: The encoding of the statement (optional).
    :returns: A tuple of :class:`~sqlparse.sql.Statement` instances.
    """
    from sqlparse import _bridge

    text = _bridge.coerce_text(sql, encoding)
    if text is not None and _bridge.use_rust():
        from sqlparse import _sqlparse

        # The extension builds genuine `sql.py` objects directly.
        return tuple(_sqlparse.parse_flat(text))
    return tuple(parsestream(sql, encoding))


def parsestream(
    stream: str | IO[str], encoding: str | None = None
) -> Generator[sql.Statement, None, None]:
    """Parses sql statements from file-like object.

    :param stream: A file-like object.
    :param encoding: The encoding of the stream contents (optional).
    :returns: A generator of :class:`~sqlparse.sql.Statement` instances.
    """
    from sqlparse import _bridge

    text = _bridge.coerce_text(stream, encoding)
    if text is not None and _bridge.use_rust():
        return _parsestream_rust(text)
    stack = engine.FilterStack()
    stack.enable_grouping()
    return stack.run(stream, encoding)


def _parsestream_rust(text: str) -> Generator[sql.Statement, None, None]:
    """Rust-backed statement generator (lazy like `FilterStack.run`)."""
    from sqlparse import _sqlparse

    yield from _sqlparse.parse_flat(text)


def format(sql: str, encoding: str | None = None, **options: Any) -> str:
    """Format *sql* according to *options*.

    Available options are documented in :ref:`formatting`.

    In addition to the formatting options this function accepts the
    keyword "encoding" which determines the encoding of the statement.

    :returns: The formatted SQL statement as string.
    """
    from sqlparse import _bridge

    text = _bridge.coerce_text(sql, encoding)
    if text is not None and _bridge.use_rust():
        from sqlparse import _sqlparse

        validated = formatter.validate_options(dict(options))
        return _sqlparse.format_flat(
            text,
            keyword_case=validated.get('keyword_case'),
            identifier_case=validated.get('identifier_case'),
            truncate_strings=validated.get('truncate_strings'),
            truncate_char=validated.get('truncate_char', '[...]'),
            use_space_around_operators=validated.get('use_space_around_operators', False),
            strip_comments=validated.get('strip_comments', False),
            strip_whitespace=validated.get('strip_whitespace', False),
            reindent=validated.get('reindent', False),
            reindent_aligned=validated.get('reindent_aligned', False),
            indent_width=validated.get('indent_width', 2),
            indent_char=validated.get('indent_char', ' '),
            indent_after_first=validated.get('indent_after_first', False),
            indent_columns=validated.get('indent_columns', False),
            wrap_after=validated.get('wrap_after', 0),
            comma_first=validated.get('comma_first', False),
            compact=validated.get('compact', False),
            right_margin=validated.get('right_margin'),
            output_format=validated.get('output_format'),
        )
    stack = engine.FilterStack()
    options = formatter.validate_options(options)
    stack = formatter.build_filter_stack(stack, options)
    stack.postprocess.append(filters.SerializerUnicode())
    return "".join(stack.run(sql, encoding))


def split(
    sql: str, encoding: str | None = None, strip_semicolon: bool = False
) -> list[str]:
    """Split *sql* into single statements.

    :param sql: A string containing one or more SQL statements.
    :param encoding: The encoding of the statement (optional).
    :param strip_semicolon: If True, remove trailing semicolons
        (default: False).
    :returns: A list of strings.
    """
    from sqlparse import _bridge

    text = _bridge.coerce_text(sql, encoding)
    if text is not None and _bridge.use_rust():
        from sqlparse import _sqlparse

        return _sqlparse.split_flat(text, strip_semicolon)
    stack = engine.FilterStack(strip_semicolon=strip_semicolon)
    return [str(stmt).strip() for stmt in stack.run(sql, encoding)]
