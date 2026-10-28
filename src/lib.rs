//! `sqlparse._sqlparse`: compiled half of the port.
//!
//! Thin PyO3 translation over [`sqlparse_rust_core`]: the whole SQL string
//! crosses once in, flat results cross once out. All Python-visible
//! behavior (tree classes, validation messages, fallback routing) stays in
//! the `sqlparse/` facade; this file only translates between Python
//! objects and core indices. No `unsafe` in this port.

use pyo3::prelude::*;
use sqlparse_rust_core::lexer;

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

#[pymodule]
fn _sqlparse(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", "0.6.0")?;
    m.add_function(wrap_pyfunction!(tokenize_flat, m)?)?;
    Ok(())
}
