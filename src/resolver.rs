//! The evaluator's cell source — [`formula_lang::CellResolver`] over the
//! engine's value cache, with stub translation for cross-sheet reads.
//!
//! During one formula's evaluation the engine is an immutable lookup
//! table: literals and already-computed cells come straight from the
//! value cache; a read that lands in the formula's stub space is
//! translated to its real `(sheet, row, col)` and served from the cache
//! of that sheet. A stub read whose target sheet does not exist yields
//! `#REF!` — Excel's answer to a broken reference. Coordinates arrive
//! 1-based from the evaluator and are converted to the engine's 0-based
//! keys at the boundary.

use crate::qualified::Prepared;
use crate::CellId;
use formula_lang::{CellResolver, ExcelError, Value};
use std::string::String;

/// Resolves one formula's reads against the whole workbook.
pub(crate) struct EngineResolver<'a> {
    engine: &'a crate::Engine,
    /// The sheet the evaluating formula lives on — the namespace its
    /// unqualified references resolve in.
    owner: &'a str,
    /// The formula's stub table (empty for pure-local formulas).
    prepared: &'a Prepared,
}

impl<'a> EngineResolver<'a> {
    pub(crate) fn new(engine: &'a crate::Engine, owner: &'a str, prepared: &'a Prepared) -> Self {
        Self {
            engine,
            owner,
            prepared,
        }
    }

    /// The 0-based engine key of a local (unqualified) 1-based read.
    fn local_key(&self, col: u32, row: u32) -> CellId {
        (String::from(self.owner), row - 1, col - 1)
    }

    /// Translates a read that landed in the stub space. `None` when the
    /// coordinate is outside every stub rectangle (defensively treated as
    /// a local read — it can only be reached by a reference the rewriter
    /// never produced).
    fn stub_target(&self, col: u32, row: u32) -> Option<(CellId, u64)> {
        let lin = u64::from(row - 1) * GRID_W + u64::from(col - 1);
        let (stub, off) = self.prepared.resolve_stub(lin)?;
        let target_sheet = &stub.target.0;
        let target = (
            String::clone(target_sheet),
            stub.target.1 + u32::try_from(off / GRID_W).unwrap_or(u32::MAX),
            stub.target.2 + u32::try_from(off % GRID_W).unwrap_or(u32::MAX),
        );
        Some((target, off))
    }

    /// One cell read, stub-aware, defaulting to empty.
    fn read(&self, col: u32, row: u32) -> Value {
        if let Some((target, _)) = self.stub_target(col, row) {
            if !self.engine.has_sheet(&target.0) {
                return Value::Error(ExcelError::Ref);
            }
            return self.engine.cached(&target).unwrap_or(Value::Empty);
        }
        self.engine
            .cached(&self.local_key(col, row))
            .unwrap_or(Value::Empty)
    }
}

impl CellResolver for EngineResolver<'_> {
    fn get(&self, col: u32, row: u32) -> Option<Value> {
        // The evaluator only calls `get` for references its AST contains —
        // always ≥ 1, so the subtractions cannot underflow.
        debug_assert!(col >= 1 && row >= 1);
        if col == 0 || row == 0 {
            return None;
        }
        Some(self.read(col, row))
    }

    fn get_range(&self, start: (u32, u32), end: (u32, u32)) -> Vec<Value> {
        // The evaluator normalizes before calling, but the contract says
        // implementations should not rely on it.
        let (c0, r0, c1, r1) = formula_lang::ast::normalize_range(
            &formula_lang::ast::CellRef::new(start.0, start.1),
            &formula_lang::ast::CellRef::new(end.0, end.1),
        );
        let mut out = Vec::with_capacity(((c1 - c0 + 1) * (r1 - r0 + 1)) as usize);
        for row in r0..=r1 {
            for col in c0..=c1 {
                out.push(self.read(col, row));
            }
        }
        out
    }
}

/// Column stride of the linear coordinate space (see [`crate::qualified`]).
const GRID_W: u64 = 16_384;
