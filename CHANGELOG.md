# Changelog

All notable changes to this project are documented in this file. The format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions
follow [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] — 2026-10-04

### Added

- `Engine`: named sheets of `sheet-core` cells, a workbook-level dependency
  graph (per-sheet `sheet-core` DAG + engine-owned cross-sheet edges), a
  computed-value cache, and a dirty set.
- `set_cell` — the Excel-flavored input grammar: formulas (`=…`), numbers,
  `TRUE`/`FALSE`, error literals, and text.
- `get_value` — cached computed value or literal; `None` for formulas that
  have never been calculated.
- `recalculate` — full topological evaluation over every formula, across
  sheets; the topological order is cached until edges change.
- `recalculate_incremental` — evaluates only the dirty closure plus
  volatile cells, ordered over just that subset.
- `volatile_cells` — the `TODAY`/`NOW`/`OFFSET` formulas, which re-evaluate
  on every calculation.
- Cross-sheet references (`Sheet2!A1`, `='My Sheet'!$B$2:F9`) via per-formula
  stub rewriting — no evaluator re-implementation, byte-faithful formula
  text.
- Circular-reference detection with exact cycle membership (Kosaraju over
  the unorderable remainder), Excel-style: acyclic cells still compute.
- Error values as data: evaluation errors become `Value::Error`, propagate
  through readers, and recover via `IFERROR`/`ISERROR`.
- `to_xlsx` / `from_xlsx` over `sheet-xlsx`, with cached results written
  and trusted (Excel semantics) and verbatim formula text round-trips.

[0.1.0]: https://github.com/WyattAu/sheet-engine/releases/tag/v0.1.0
