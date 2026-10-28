//! `sqlparse._sqlparse`: compiled half of the port.
//!
//! Thin PyO3 translation over [`sqlparse_rust_core`]: the whole SQL string
//! crosses once in, flat results cross once out. All Python-visible
//! behavior (tree classes, validation messages, fallback routing) stays in
//! the `sqlparse/` facade; this file only translates between Python
//! objects and core indices. No `unsafe` in this port.

use pyo3::prelude::*;
use pyo3::types::{PyList, PyString};
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

/// Per-call cache of `tokens.X` objects and `sql.*` classes, resolved by
/// dotted path exactly like the facade used to.
struct Resolver<'py> {
    py: Python<'py>,
    tokens_mod: Bound<'py, PyModule>,
    ttype_cache: std::collections::HashMap<u16, PyObject>,
    class_cache: std::collections::HashMap<&'static str, PyObject>,
}

impl<'py> Resolver<'py> {
    fn new(py: Python<'py>) -> PyResult<Self> {
        Ok(Resolver {
            py,
            tokens_mod: py.import("sqlparse.tokens")?,
            ttype_cache: std::collections::HashMap::new(),
            class_cache: std::collections::HashMap::new(),
        })
    }

    fn ttype(&mut self, t: tree::TType) -> PyResult<PyObject> {
        let key = t as u16;
        if let Some(o) = self.ttype_cache.get(&key) {
            return Ok(o.clone_ref(self.py));
        }
        let mut obj = self.tokens_mod.getattr("Token")?;
        for part in t.as_str().split('.').skip(1) {
            obj = obj.getattr(part)?;
        }
        let o = obj.unbind();
        self.ttype_cache.insert(key, o.clone_ref(self.py));
        Ok(o)
    }

    fn class(&mut self, sql_mod: &Bound<'py, PyModule>, kind: GroupKindName) -> PyResult<PyObject> {
        if let Some(o) = self.class_cache.get(kind.0) {
            return Ok(o.clone_ref(self.py));
        }
        let o: PyObject = sql_mod.getattr(kind.0)?.unbind();
        self.class_cache.insert(kind.0, o.clone_ref(self.py));
        Ok(o)
    }
}

struct GroupKindName(&'static str);

/// Build a genuine `sql.py` object directly (no intermediate tuples, so
/// peak RSS is the tree itself, like pure Python).
fn build_node<'py>(
    py: Python<'py>,
    res: &mut Resolver<'py>,
    sql_mod: &Bound<'py, PyModule>,
    text: &str,
    node: &tree::Node,
) -> PyResult<PyObject> {
    match node {
        tree::Node::Tok { ttype, start, end } => {
            let cls: PyObject = sql_mod.getattr("Token")?.unbind();
            let tt = res.ttype(*ttype)?;
            let v = PyString::new(py, &text[*start as usize..*end as usize]);
            Ok(cls.call1(py, (tt, v))?)
        }
        tree::Node::Grp { kind, kids, .. } => {
            let mut items = Vec::with_capacity(kids.len());
            for k in kids {
                items.push(build_node(py, res, sql_mod, text, k)?);
            }
            let list = PyList::new(py, items)?;
            let cls = res.class(sql_mod, GroupKindName(kind.as_str()))?;
            Ok(cls.call1(py, (list,))?)
        }
    }
}

/// `[(ttype_path, value)]` for the whole input (default lexer config).
/// The facade wraps this in `iter()` to match `get_tokens` laziness shape.
#[pyfunction]
fn tokenize_flat(text: &str) -> Vec<(String, String)> {
    let toks = lexer::lex(text);
    toks.iter()
        .map(|t| {
            (
                t.ttype.as_str().to_owned(),
                text[t.start as usize..t.end as usize].to_owned(),
            )
        })
        .collect()
}

/// Grouped statements as genuine `sql.py` objects (same classes the pure
/// path builds). Errors surface as `sqlparse.exceptions.SQLParseError`
/// with the exact upstream messages.
#[pyfunction]
fn parse_flat(py: Python<'_>, text: &str) -> PyResult<Vec<PyObject>> {
    let toks = lexer::lex(text);
    let mut splitter = split::Splitter::new();
    let stmts = splitter.split(text, &toks);
    let sql_mod = py.import("sqlparse.sql")?;
    let mut res = Resolver::new(py)?;
    let mut out = Vec::with_capacity(stmts.len());
    for stmt in &stmts {
        let mut kids: Vec<tree::Node> = stmt
            .iter()
            .map(|t| tree::Node::leaf(t.ttype, t))
            .collect();
        group::group_stmt(text, &mut kids)
            .map_err(|e| parse_error(py, e.message()))?;
        let mut items = Vec::with_capacity(kids.len());
        for k in &kids {
            items.push(build_node(py, &mut res, &sql_mod, text, k)?);
        }
        let list = PyList::new(py, items)?;
        let cls = res.class(&sql_mod, GroupKindName("Statement"))?;
        out.push(cls.call1(py, (list,))?);
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
    let toks = lexer::lex(text);
    let mut splitter = split::Splitter::new();
    let stmts = splitter.split(text, &toks);
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
