# sqlparse-rust

Rust port of [andialbrecht/sqlparse](https://github.com/andialbrecht/sqlparse)
(SQL lexer / statement splitter / grouping engine / formatter):
`sqlparse-core` crate + `sqlparse` PyO3 binding, BSD-3, upstream-attributed
(see NOTICE).

Status: landed — full upstream suite green (506 passed, 2 xfailed, 1 xpassed
stale), differential fuzz + seam suites green, kill criteria passed.

Measured speedups (release wheel, Ryzen 9 5950X, wall per call, rust vs
pure in the same wheel; `benches/bench_two_track.py`):

| workload | pure | rust | speedup |
|---|---|---|---|
| split-1x (~13 KB) | 27.9 ms | 3.8 ms | 7.3x |
| split-8x (~106 KB) | 228 ms | 32 ms | 7.2x |
| parse-1x | 87.0 ms | 20.3 ms | 4.3x |
| parse-8x | 760 ms | 227 ms | 3.4x |
| format-reindent-1x | 106 ms | 8.8 ms | 12.0x |
| format-reindent-8x | 847 ms | 72 ms | 11.8x |
| format-light-1x | 98 ms | 8.5 ms | 11.5x |
| format-light-8x | 759 ms | 69 ms | 11.0x |
| parse-single | 0.37 ms | 0.083 ms | 4.5x |
| format-single | 0.53 ms | 0.049 ms | 10.7x |

## Layout

- `sqlparse-core/` — hot unit: default-config lexer (hand-written scanner,
  50 patterns + delimited-span pre-scan + generated keyword tables) +
  statement splitter + grouping engine (26 passes) + format engine (all
  filters). No Python dependency; one string crossing, consumed once.
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
