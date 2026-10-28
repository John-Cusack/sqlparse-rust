# sqlparse-rust

Rust port of [andialbrecht/sqlparse](https://github.com/andialbrecht/sqlparse)
(SQL lexer / statement splitter / grouping engine / formatter):
`sqlparse-core` crate + `sqlparse` PyO3 binding, BSD-3, upstream-attributed
(see NOTICE).

Status: landed — full upstream suite green (506 passed, 2 xfailed, 1 xpassed
stale), differential fuzz + seam suites green, kill criteria passed.

Measured speedups (release wheel, Ryzen 9 5950X; `benches/bench_two_track.py`):
Wall is per-call best-of-3; RSS is subprocess peak (`ru_maxrss`), same
installed wheel both sides (`SQLPARSE_PURE=1` selects pure).

| workload | pure wall | rust wall | speedup | pure peak RSS | rust peak RSS |
|---|---|---|---|---|---|
| split-1x (~13 KB) | 31.0 ms | 4.2 ms | 7.4x | 12.4 MB | 13.7 MB |
| split-8x (~106 KB) | 255 ms | 32 ms | 8.1x | 13.5 MB | 16.3 MB |
| parse-1x | 91.1 ms | 15.9 ms | 5.7x | 20.5 MB | 24.0 MB |
| parse-8x | 839 ms | 157 ms | 5.3x | 31.4 MB | 34.3 MB |
| format-reindent-1x | 105 ms | 9.0 ms | 11.7x | 14.2 MB | 14.2 MB |
| format-reindent-8x | 895 ms | 76 ms | 11.8x | 14.5 MB | 15.6 MB |
| format-light-1x | 106 ms | 8.5 ms | 12.4x | 13.6 MB | 14.0 MB |
| format-light-8x | 784 ms | 69 ms | 11.4x | 13.2 MB | 15.4 MB |
| parse-single | 0.35 ms | 0.082 ms | 4.3x | 13.7 MB | 13.5 MB |
| format-single | 0.47 ms | 0.044 ms | 10.6x | 12.9 MB | 13.0 MB |

Kill criteria: <2x on split+format — passed on every workload (min 4.3x).
Peak RSS runs ~0-20% above pure on bulk workloads (batch token tables
vs the pure generator pipeline); parse-8x improved from +33% to +9% by
constructing the tree directly in the binding, and the lexer drops its
char/offset tables before returning.
- `src/` — PyO3 binding (`tokenize_flat` / `parse_flat` / `format_flat` /
  `split_flat`); no `unsafe`.
- `sqlparse/` — facade: byte-identical upstream tree except `__init__.py`
  (routes the 4 entry points when the default lexer is pristine),
  `lexer.py` (routes `get_tokens`), and `_bridge.py` (routing + assembly).
  Custom `Lexer` config, custom filters, and `SQLPARSE_PURE=1` take the
  exact pure-Python path. `tokens.py` / `keywords.py` stay byte-identical.
- `tests/` — lexer parity + differential fuzz + seam suites (upstream suite
  itself runs verbatim from its sdist against the installed wheel).
- `benches/` — two-track (wall + peak RSS) benches, every figure labeled
  by workload.

## Seams (documented)

- `Lexer.clear` / `set_SQL_REGEX` / `add_keywords` (or any other default
  singleton mutation) → pure path per call.
- Custom `FilterStack` filters, custom `Lexer` subclasses, direct
  `grouping.group` on hand-built trees → pure path (untouched upstream code).
- `SQLPARSE_PURE=1` or missing extension → pure path.
- `RightMarginFilter` raises `NotImplementedError`, like upstream.

## License

BSD-3-Clause — see [LICENSE](LICENSE). Upstream attribution in [NOTICE](NOTICE).
