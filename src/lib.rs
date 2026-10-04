//! Spreadsheet calculation engine — dependency-driven incremental
//! recalculation, Excel-compatible semantics, volatile function tracking.
//!
//! `sheet-engine` is the L2 composition layer of the estate's spreadsheet
//! stack: it turns the parts into a working product.
//!
//! | Crate | Layer | Role in the engine |
//! |---|---|---|
//! | `formula_lang` | L0 | formula tokenizer, Pratt parser, AST, evaluator, Excel coercions |
//! | `sheet_core` | L1 | sparse cell storage (`Sheet`), per-sheet dependency DAG (`DependencyGraph`) |
//! | `sheet_xlsx` | L1 | XLSX codec — workbook, worksheets, shared strings, calc chain |
//!
//! # The workbook dimension
//!
//! `sheet-core` models **one worksheet**: its `DependencyGraph` keys cells
//! by `(row, col)` within a single sheet, and its reference scanner records
//! a qualified reference (`Sheet2!A1`) at its bare coordinates — both
//! documented v0.1 simplifications that defer cross-sheet edges to a
//! workbook-level graph. That workbook-level graph is *this crate*: the
//! engine layers the sheet dimension on top —
//!
//! - **Local edges** (formula reads cells of its own sheet) are owned by a
//!   per-sheet `sheet-core` `DependencyGraph`, exactly as designed.
//! - **Cross-sheet edges** (`=Sheet2!A1`, `='Q4 Data'!B2:F9`) are owned by
//!   the engine and merged with the local edges everywhere a graph question
//!   is asked: topological order, dirty propagation, cycle detection.
//!
//! # Cross-sheet evaluation by stub rewriting
//!
//! `formula-lang`'s grammar has no sheet qualifier (`!` is not a token), so
//! the engine cannot parse `=Sheet2!A1` directly — and its
//! `CellResolver` has no sheet dimension either. Instead of re-implementing
//! the evaluator, the engine **rewrites** each qualified reference to a
//! *stub*: a plain in-grid cell reference that would collide with no real
//! reference in the same formula (stub coordinates start above the highest
//! linear coordinate the formula's local references reach). The rewrite is
//! recorded per formula, and the engine's resolver translates stub reads
//! back to their real `(sheet, row, col)` targets at evaluation time. The
//! original formula text is preserved verbatim in the sheet, so XLSX
//! round-trips are exact.
//!
//! # Calculation model
//!
//! The API is explicit-recalculation, like a file format rather than a UI:
//! [`set_cell`](Engine::set_cell) parses the input, stores it, and marks
//! the cell dirty; [`recalculate`](Engine::recalculate) evaluates every
//! formula in topological order; `recalculate_incremental` evaluates only
//! the dirty subgraph — the edited cells plus their transitive dependents,
//! plus the volatile cells (`TODAY`, `NOW`, `OFFSET`), which re-evaluate on
//! every calculation, Excel-style.
//!
//! Errors are values, not exceptions, exactly as spreadsheets behave:
//! `=1/0` evaluates to `#DIV/0!`, and a formula reading an error cell
//! propagates it (`=A1+1` over an errored `A1` is `#DIV/0!` too). Circular
//! references are the one hard error: they are detected as explicit cycles
//! and reported, never evaluated to a fixed point.
//!
//! # Coordinates
//!
//! The public API is **0-based** `(row, col)` — the same convention as
//! `sheet-core` and `sheet-xlsx`. Cell `A1` is `(0, 0)`; the `A1` notation
//! inside formulas stays 1-based as Excel users write it (`=A1` reads
//! `(0, 0)`).
//!
//! # Example
//!
//! ```
//! use sheet_engine::{Engine, Value};
//!
//! # fn main() -> Result<(), sheet_engine::EngineError> {
//! let mut engine = Engine::new();
//! engine.set_cell("Sheet1", 0, 0, "41")?;        // A1 = 41
//! engine.set_cell("Sheet1", 0, 1, "=A1+1")?;     // B1 = A1 + 1
//! engine.recalculate()?;
//! assert_eq!(engine.get_value("Sheet1", 0, 1), Some(Value::Number(42.0)));
//!
//! // Edit A1; only the dependency chain re-evaluates.
//! engine.set_cell("Sheet1", 0, 0, "100")?;
//! engine.recalculate_incremental(&[("Sheet1".into(), 0, 0)])?;
//! assert_eq!(engine.get_value("Sheet1", 0, 1), Some(Value::Number(101.0)));
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

pub mod error;
pub mod qualified;
pub mod workbook;

mod engine;
mod resolver;
mod xlsx_codec;

pub use engine::Engine;
pub use error::EngineError;
pub use formula_lang::Value;

/// A cell address in the workbook: `(sheet, row, col)` — the sheet's name
/// and 0-based coordinates, the same tuple every engine API speaks.
pub type CellId = (String, u32, u32);
