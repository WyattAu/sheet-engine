# Security Policy — sheet-engine

## Supported versions

| Version | Supported |
|---------|-----------|
| 0.1.x   | ✅        |

## Reporting a vulnerability

Report privately via [GitHub security advisories] for this repository, or
email **wyatt_au@protonmail.com**. Do **not** open a public issue for
security reports.

You will receive an acknowledgement within **72 hours**. Coordinated
disclosure: we ask for up to 90 days before public disclosure while a
patch ships.

## Scope notes

`sheet-engine` renders metrics derived from in-process counters. Security
considerations for integrators:

- Formulas are parsed with bound resources (`formula-lang` caps nesting
  depth and rejects unbounded references), but hosts should still cap
  *workbook* size before calling `set_cell` in a loop — the engine does
  not impose a cell-count budget.
- XLSX input is untrusted ZIP + XML; the codec (`sheet-xlsx`) rejects
  malformed archives with typed errors. `from_xlsx` on attacker-supplied
  bytes should be treated like any file-format parse: allocate against a
  known budget upstream.
- Formula text round-trips verbatim; systems that re-emit formulas into
  other tools own the escaping there.
- `#![forbid(unsafe_code)]` — no unsafe blocks exist in this crate.

[GitHub security advisories]:
    https://github.com/WyattAu/sheet-engine/security/advisories/new
