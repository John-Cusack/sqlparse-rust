# sqlparse-rust

Rust port of [andialbrecht/sqlparse](https://github.com/andialbrecht/sqlparse)
(SQL lexer / statement splitter / grouping engine / formatter):
`sqlparse-core` crate + `sqlparse` PyO3 binding, BSD-3, upstream-attributed
(see NOTICE).

Status: probe phase — headroom re-probe with a frozen `WORKLOAD.md` and the
`sqlparser`-crate gap audit land in the rustsmith evidence PR before any
port code. Kill criteria: <2x on representative split+format workloads, or
per-token FFI toll dominating, stops the lane as `no_gain` with the numbers.

## Layout (planned)

- `sqlparse-core/` — hot unit: whole-string lexer + statement splitter +
  grouping engine + format filters (one string crossing, consumed once).
- `src/` — PyO3 binding exposing `parse` / `format` / `split` / `tokenize`.
- `sqlparse/` — thin facade preserving the public API; user filters, custom
  `Lexer.add_keywords` vocabularies, and non-kernel paths stay Python behind
  a documented seam; pure-Python fallback importable without the extension.
- `tests/` — upstream suite mirror + equivalence corpus; `benches/` —
  two-track (wall + peak RSS) benches, every figure labeled by workload.

## License

BSD-3-Clause — see [LICENSE](LICENSE). Upstream attribution in [NOTICE](NOTICE).
