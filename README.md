# sheet-engine

*Spreadsheet calculation engine: dependency-driven incremental recalculation,
Excel-compatible semantics, volatile function tracking.*

The L2 composition layer of the estate's spreadsheet stack. It turns three
parts into a working product:

| Crate | Layer | Role in the engine |
|---|---|---|
| [`formula-lang`](https://crates.io/crates/formula-lang) | L0 | formula tokenizer, Pratt parser, AST, evaluator, Excel coercions |
| [`sheet-core`](https://crates.io/crates/sheet-core) | L1 | sparse cell storage, per-sheet dependency DAG, reference scanning |
| [`sheet-xlsx`](https://crates.io/crates/sheet-xlsx) | L1 | XLSX codec: workbook, worksheets, shared strings, calc chain |

```rust
use sheet_engine::{Engine, Value};

let mut engine = Engine::new();
engine.set_cell("Sheet1", 0, 0, "41")?;      // A1 = 41
engine.set_cell("Sheet1", 0, 1, "=A1+1")?;   // B1 = A1 + 1
engine.recalculate()?;
assert_eq!(engine.get_value("Sheet1", 0, 1), Some(Value::Number(42.0)));

// Edit A1; only the dependency chain re-evaluates.
engine.set_cell("Sheet1", 0, 0, "100")?;
engine.recalculate_incremental(&[("Sheet1".into(), 0, 0)])?;
assert_eq!(engine.get_value("Sheet1", 0, 1), Some(Value::Number(101.0)));
```

## What the engine adds

`sheet-core` models **one worksheet**, and its reference scanner records a
sheet-qualified reference at its bare coordinates: both documented v0.1
simplifications that defer the workbook dimension to "a workbook-level
graph". That graph is this crate.

**The workbook dimension.** Local edges (a formula reading cells of its own
sheet) are owned by a per-sheet `sheet-core` `DependencyGraph`, exactly as
designed. Cross-sheet edges (`=Sheet2!A1`, `='Q4 Data'!B2:F9`) are owned by
the engine and merged with the local edges everywhere a graph question is
asked: topological evaluation order, dirty propagation, cycle detection;
all workbook-wide, across sheets.

**Cross-sheet evaluation by stub rewriting.** `formula-lang`'s grammar has
no `!`, and its cell resolver has no sheet dimension. Rather than
re-implement the evaluator (and every Excel-compatible function with it),
the engine rewrites each qualified reference to a *stub*: a plain in-grid
reference guaranteed to collide with no real reference in the same formula.
The mapping is kept per formula and translated at read time. The original
text is never altered: XLSX round-trips are byte-faithful.

**Explicit calculation.** Like a file format rather than a UI: `set_cell`
parses, stores, and marks dirty; `recalculate` evaluates every formula in
topological order; `recalculate_incremental` evaluates only the dirty
subgraph (edited cells + transitive dependents + volatile cells), in an
order computed over just that subset. Volatile formulas (`TODAY`, `NOW`,
`OFFSET`) re-evaluate on every calculation, Excel-style.

**Errors are values.** `=1/0` evaluates to `#DIV/0!`; a formula reading an
error cell propagates it; `IFERROR` recovers: all inside the value model.
Circular references are the one hard error: reported as explicit cycles
(exact membership, not "everything downstream"), with the rest of the
workbook computed first.

## Coordinates

The API is 0-based `(row, col)`: same convention as `sheet-core` and
`sheet-xlsx`. Cell `A1` is `(0, 0)`; `A1` notation *inside formulas* stays
1-based as Excel users write it.

## Calculation semantics

| Behavior | Contract |
|---|---|
| Literals | `"42"` / `"-3.5"` / `"1e3"` numeric · `TRUE`/`FALSE` logical · `"#DIV/0!"` error value · everything else text |
| `get_value` before recalculation | `None` for formulas, the literal for anything else |
| Full recalculation | topological order over every formula; cached topological order reused until edges change |
| Incremental recalculation | dirty closure ∪ volatile cells, ordered over just that subset |
| Missing sheet in a reference | legal at set time; evaluates to `#REF!` until the sheet appears |
| Circular references | `EngineError::CircularReference` with exact cycle members; acyclic cells still compute |
| XLSX | `to_xlsx` writes cached results; `from_xlsx` trusts them until a recalculation (Excel behavior) |

## Performance shape

- Full recalculation: O(V + E) topological order (cached across calls with
  unchanged edges) + one evaluation per formula.
- Incremental: O(dirty closure): an edit to one cell of a 5000-formula
  grid re-evaluates one formula, not the sheet. See `benches/recalc_bench.rs`
  and `tests/perf.rs` (VLOOKUP over 1000 rows completes in low
  single-digit milliseconds; the 100 ms contract is a test gate).

## Scope notes

- No UI, no undo, no multi-user story: a calculation core for hosts that
  have one.
- No array-spill semantics (`#SPILL!` literals load and round-trip, but
  dynamic arrays are `formula-lang`'s roadmap, not the engine's).
- Whole-column references (`A:A`) are not modeled; `sheet-core`'s reference
  cap (`RANGE_CELL_CAP`) bounds expanded ranges.

## License

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at
your option.
