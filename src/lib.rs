//! `sqlparse._sqlparse`: compiled half of the port.
//!
//! Thin PyO3 translation over [`sqlparse_rust_core`]: the whole SQL string
//! crosses once in, flat results cross once out. All Python-visible
//! behavior (tree classes, validation messages, fallback routing) stays in
//! the `sqlparse/` facade; this file only translates between Python
//! objects and core indices. No `unsafe` in this port.

use pyo3::prelude::*;
use pyo3::types::{PyList, PyString, PyTuple};
use sqlparse_rust_core::{format, group, lexer, split, tree};

/// Raise the facade's own `sqlparse.exceptions.SQLParseError` (kept
/// byte-identical upstream) so `pytest.raises(SQLParseError)` sees the
/// real class, not a generic `Exception`.
fn parse_error(py: Python<'_>, msg: String) -> PyErr {
    let exc = py
        .import("sqlparse.exceptions")
        .and_then(|m| m.getattr("SQLParseError"));
    match exc {
        Ok(cls) => PyErr::from_value(cls.call1((msg,)).unwrap()),
        Err(_) => PyErr::new::<pyo3::exceptions::PyException, _>(msg),
    }
}

fn node_to_py<'py>(
    py: Python<'py>,
    text: &str,
    node: &tree::Node,
) -> PyResult<Bound<'py, PyAny>> {
    match node {
        tree::Node::Tok { ttype, start, end } => {
            let t = PyTuple::new(
                py,
                [
                    PyString::new(py, ttype.as_str()).into_any(),
                    PyString::new(py, &text[*start as usize..*end as usize]).into_any(),
                ],
            )?;
            Ok(t.into_any())
        }
        tree::Node::Grp { kind, kids } => {
            let items: Vec<Bound<'py, PyAny>> =
                kids.iter().map(|k| node_to_py(py, text, k)).collect::<PyResult<_>>()?;
            let t = PyTuple::new(
                py,
                [
                    PyString::new(py, kind.as_str()).into_any(),
                    PyList::new(py, items)?.into_any(),
                ],
            )?;
            Ok(t.into_any())
        }
    }
}

/// `[(ttype_path, value)]` for the whole input (default lexer config).
/// The facade wraps this in `iter()` to match `get_tokens` laziness shape.
#[pyfunction]
fn tokenize_flat(text: &str) -> Vec<(String, String)> {
    let lexed = lexer::lex(text);
    lexed
        .toks
        .iter()
        .map(|t| {
            (
                t.ttype.as_str().to_owned(),
                text[t.start as usize..t.end as usize].to_owned(),
            )
        })
        .collect()
}

/// Grouped statements as nested `(kind_or_ttype, payload)` tuples.
/// Leaf = `(ttype_path, value)`; group = `(kind, [children])`.
/// Errors surface as `sqlparse.exceptions.SQLParseError` with the exact
/// upstream messages (`Maximum grouping depth exceeded (100).`, ...).
#[pyfunction]
fn parse_flat(py: Python<'_>, text: &str) -> PyResult<Vec<PyObject>> {
    let lexed = lexer::lex(text);
    let mut splitter = split::Splitter::new();
    let stmts = splitter.split(text, &lexed.toks);
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in &stmts {
        let mut kids: Vec<tree::Node> = stmt
            .iter()
            .map(|t| tree::Node::leaf(t.ttype, t))
            .collect();
        group::group_stmt(text, &mut kids)
            .map_err(|e| parse_error(py, e.message()))?;
        let items: Vec<Bound<'_, PyAny>> = kids
            .iter()
            .map(|k| node_to_py(py, text, k))
            .collect::<PyResult<_>>()?;
        let stmt_tup = PyTuple::new(
            py,
            [
                PyString::new(py, "Statement").into_any(),
                PyList::new(py, items)?.into_any(),
            ],
        )?;
        out.push(stmt_tup.into_any().unbind());
    }
    Ok(out)
}

#[pymodule]
fn _sqlparse(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", "0.6.0")?;
    m.add_function(wrap_pyfunction!(tokenize_flat, m)?)?;
    m.add_function(wrap_pyfunction!(parse_flat, m)?)?;
    m.add_function(wrap_pyfunction!(format_flat, m)?)?;
    m.add_function(wrap_pyfunction!(split_flat, m)?)?;
    Ok(())
}

fn case_mode(v: Option<&str>) -> u8 {
    match v {
        Some("upper") => 1,
        Some("lower") => 2,
        Some("capitalize") => 3,
        _ => 0,
    }
}

/// Full `format()` path: validated options in, formatted string out.
/// `SQLParseError` on grouping-cap breach (exact messages);
/// bare `NotImplementedError` when `right_margin` is set (like upstream's
/// `RightMarginFilter.process`).
#[pyfunction]
#[pyo3(signature = (text, *, keyword_case=None, identifier_case=None, truncate_strings=None, truncate_char="[...]", use_space_around_operators=false, strip_comments=false, strip_whitespace=false, reindent=false, reindent_aligned=false, indent_width=2, indent_char=" ", indent_after_first=false, indent_columns=false, wrap_after=0, comma_first=false, compact=false, right_margin=None, output_format=None))]
#[allow(clippy::too_many_arguments)]
fn format_flat(
    py: Python<'_>,
    text: &str,
    keyword_case: Option<&str>,
    identifier_case: Option<&str>,
    truncate_strings: Option<usize>,
    truncate_char: &str,
    use_space_around_operators: bool,
    strip_comments: bool,
    strip_whitespace: bool,
    reindent: bool,
    reindent_aligned: bool,
    indent_width: usize,
    indent_char: &str,
    indent_after_first: bool,
    indent_columns: bool,
    wrap_after: usize,
    comma_first: bool,
    compact: bool,
    right_margin: Option<usize>,
    output_format: Option<&str>,
) -> PyResult<String> {
    let opts = format::FormatOptions {
        keyword_case: case_mode(keyword_case),
        identifier_case: case_mode(identifier_case),
        truncate: truncate_strings.map(|w| (w, truncate_char.to_owned())),
        space_ops: use_space_around_operators,
        strip_comments,
        strip_ws: strip_whitespace,
        reindent,
        reindent_aligned,
        indent_width,
        indent_char: indent_char.chars().next().unwrap_or(' '),
        indent_after_first,
        indent_columns,
        wrap_after,
        comma_first,
        compact,
        right_margin,
        output: match output_format {
            Some("python") => 1,
            Some("php") => 2,
            _ => 0,
        },
    };
    match format::format_text(text, &opts) {
        Ok(s) => Ok(s),
        Err(format::FormatError::Group(e)) => Err(parse_error(py, e.message())),
        Err(format::FormatError::NotImplemented) => {
            Err(PyErr::new::<pyo3::exceptions::PyNotImplementedError, _>(()))
        }
    }
}

/// `split()` path: statement strings, stripped, with optional `;` strip.
#[pyfunction]
#[pyo3(signature = (text, strip_semicolon = false))]
fn split_flat(text: &str, strip_semicolon: bool) -> Vec<String> {
    let lexed = lexer::lex(text);
    let mut splitter = split::Splitter::new();
    let stmts = splitter.split(text, &lexed.toks);
    stmts
        .iter()
        .map(|toks| {
            let mut toks = toks.clone();
            if strip_semicolon {
                split::strip_trailing_semicolon(&mut toks, text);
            }
            let s: String = toks
                .iter()
                .map(|t| &text[t.start as usize..t.end as usize])
                .collect();
            s.trim().to_owned()
        })
        .collect()
}
