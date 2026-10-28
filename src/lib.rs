//! `sqlparse._sqlparse`: compiled half of the port.
//!
//! Thin PyO3 translation over [`sqlparse_rust_core`]: the whole SQL string
//! crosses once in, flat results cross once out. All Python-visible
//! behavior (tree classes, validation messages, fallback routing) stays in
//! the `sqlparse/` facade; this file only translates between Python
//! objects and core indices. No `unsafe` in this port.

use pyo3::prelude::*;

#[pymodule]
fn _sqlparse(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add("__version__", "0.6.0")?;
    Ok(())
}
