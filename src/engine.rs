//! The engine itself: cell storage, dependency tracking, calculation.

use crate::error::EngineError;
use crate::qualified::Prepared;
use crate::resolver::EngineResolver;
use crate::workbook;
use crate::CellId;
use formula_lang::ExcelError;
use sheet_core::{Cell, DependencyGraph, Sheet};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::string::String;
use std::vec::Vec;

/// A spreadsheet calculation engine.
///
/// Named sheets of cells ([`sheet_core::Sheet`]), a workbook-level
/// dependency graph (per-sheet [`sheet_core::DependencyGraph`] plus the
/// engine's cross-sheet edges), a cache of computed values, and the dirty
/// set incremental recalculation drains. See the [crate docs](crate) for
/// the calculation model.
pub struct Engine {
    /// Named sheets, sorted by name — the engine's cell storage.
    sheets: BTreeMap<String, Sheet>,
    /// Per-sheet local dependency graphs: a formula's edges to cells of its
    /// own sheet, exactly as `sheet-core` models them.
    graphs: BTreeMap<String, DependencyGraph>,
    /// Cross-sheet edges: a qualified formula → the cells it reads on other
    /// sheets. Local edges never appear here.
    cross_precedents: HashMap<CellId, BTreeSet<CellId>>,
    /// Reverse of `cross_precedents`: a cell → the qualified formulas that
    /// read it.
    cross_dependents: HashMap<CellId, BTreeSet<CellId>>,
    /// Formulas in evaluation-ready form, keyed by their cell.
    formulas: BTreeMap<CellId, Prepared>,
    /// Cells whose formula contains at least one sheet-qualified reference.
    qualified: BTreeSet<CellId>,
    /// Cells whose formula contains a volatile function (`TODAY`, `NOW`,
    /// `OFFSET`) and therefore re-evaluates on every calculation.
    volatile: BTreeSet<CellId>,
    /// Cached values: literals at set time, computed results at
    /// recalculation time. A formula cell with no entry has never been
    /// calculated.
    values: HashMap<CellId, formula_lang::Value>,
    /// Cells edited since the last calculation.
    dirty: HashSet<CellId>,
    /// Cached workbook topological order — the cold path runs only when
    /// edges change (formulas registered or retired), never per
    /// recalculation.
    order_cache: Option<Vec<CellId>>,
}

impl Engine {
    /// An empty workbook with no sheets. Sheets are created on demand by
    /// [`set_cell`](Self::set_cell) and by
    /// [`from_xlsx`](Self::from_xlsx).
    #[must_use]
    pub fn new() -> Self {
        Self {
            sheets: BTreeMap::new(),
            graphs: BTreeMap::new(),
            cross_precedents: HashMap::new(),
            cross_dependents: HashMap::new(),
            formulas: BTreeMap::new(),
            qualified: BTreeSet::new(),
            volatile: BTreeSet::new(),
            values: HashMap::new(),
            dirty: HashSet::new(),
            order_cache: None,
        }
    }
}

impl Default for Engine {
    fn default() -> Self {
        Engine::new()
    }
}

impl Engine {
    /// The names of the sheets currently in the workbook, sorted.
    #[must_use]
    pub fn sheet_names(&self) -> Vec<String> {
        self.sheets.keys().cloned().collect()
    }

    /// `true` when the named sheet exists.
    #[must_use]
    pub fn has_sheet(&self, sheet: &str) -> bool {
        self.sheets.contains_key(sheet)
    }

    /// The cached value of one cell — the computed result for a formula,
    /// the literal for anything else. `None` for a blank cell, an unknown
    /// sheet, or a formula that has never been calculated.
    #[must_use]
    pub fn get_value(&self, sheet: &str, row: u32, col: u32) -> Option<formula_lang::Value> {
        self.values.get(&(sheet.to_string(), row, col)).cloned()
    }

    /// Every volatile cell (`TODAY`, `NOW`, `OFFSET` formulas), in
    /// `(sheet, row, col)` order. These re-evaluate on every calculation,
    /// full or incremental, Excel-style.
    #[must_use]
    pub fn volatile_cells(&self) -> Vec<CellId> {
        self.volatile.iter().cloned().collect()
    }

    /// Sets one cell from user input.
    ///
    /// Input grammar, Excel-flavored:
    ///
    /// - `=…` — a formula, parsed with `formula-lang` (sheet-qualified
    ///   references supported; see [`crate::qualified`]);
    /// - a number — stored numeric (`"42"`, `"-3.5"`, `"1e3"`);
    /// - `TRUE` / `FALSE`, any case — stored logical;
    /// - an error literal (`"#DIV/0!"` and the other canonical spellings) —
    ///   stored as an error *value*;
    /// - anything else — stored text, verbatim.
    ///
    /// The cell is marked dirty; nothing computes until
    /// [`recalculate`](Self::recalculate) or
    /// [`recalculate_incremental`](Self::recalculate_incremental). A
    /// formula replacing a formula swaps the dependency edges; a literal
    /// replacing a formula removes them. Referencing a sheet that does not
    /// exist is legal at set time (spreadsheets let you type ahead) — such
    /// reads evaluate to `#REF!` until the sheet appears.
    ///
    /// # Errors
    ///
    /// [`EngineError::Formula`] when formula text does not parse;
    /// [`EngineError::Graph`] when its references cannot be modeled (for
    /// example a range wider than `sheet_core::refs::RANGE_CELL_CAP`). A
    /// rejected formula changes nothing *except* dropping the cell's
    /// previous cached value.
    pub fn set_cell(
        &mut self,
        sheet: &str,
        row: u32,
        col: u32,
        input: &str,
    ) -> Result<(), EngineError> {
        let input = input.trim();
        if input.starts_with('=') {
            return self.set_formula(sheet, row, col, input);
        }
        let (cell, value) = literal(input);
        let node = (sheet.to_string(), row, col);
        self.unregister_formula(&node);
        self.sheets
            .entry(node.0.clone())
            .or_default()
            .set(row, col, cell);
        self.values.insert(node.clone(), value);
        self.dirty.insert(node);
        Ok(())
    }

    /// Evaluates every formula in the workbook, in topological order —
    /// each formula after every formula it reads, across sheets. The dirty
    /// set is drained.
    ///
    /// # Errors
    ///
    /// [`EngineError::CircularReference`] when the workbook contains
    /// circular references. Everything *not* on a cycle has still been
    /// evaluated when the error is raised (Excel computes the rest of the
    /// workbook); cells on a cycle keep no value.
    pub fn recalculate(&mut self) -> Result<(), EngineError> {
        let order = self.topo();
        let mut circular = None;
        if order.len() < self.formulas.len() {
            let placed: BTreeSet<&CellId> = order.iter().collect();
            let leftover: Vec<CellId> = self
                .formulas
                .keys()
                .filter(|n| !placed.contains(*n))
                .cloned()
                .collect();
            circular = Some(workbook::circular_members(self, &leftover));
        }
        for node in order {
            self.evaluate_node(&node);
        }
        self.dirty.clear();
        match circular {
            Some(cells) => Err(EngineError::CircularReference(cells)),
            None => Ok(()),
        }
    }

    /// Evaluates only what an edit made stale: the edited cells and their
    /// transitive dependents, plus every volatile cell — in topological
    /// order, so a chain settles in one pass.
    ///
    /// `edited` lists cells that changed outside the engine's bookkeeping
    /// (typically the cells of a [`set_cell`](Self::set_cell) batch);
    /// cells dirtied by earlier `set_cell` calls are included
    /// automatically, so `recalculate_incremental(&[])` is the
    /// "recalculate what is pending" form.
    ///
    /// On a large sheet whose edit touches a small subgraph this is
    /// proportionally cheaper than a full [`recalculate`](Self::recalculate).
    ///
    /// # Errors
    ///
    /// [`EngineError::CircularReference`] when the pending set meets a
    /// cycle — same contract as `recalculate`.
    pub fn recalculate_incremental(
        &mut self,
        edited: &[(String, u32, u32)],
    ) -> Result<(), EngineError> {
        let seeds = self.dirty.iter().cloned().chain(edited.iter().cloned());
        let mut work = workbook::dirty_closure(self, seeds);
        for v in self.volatile.clone() {
            work.insert(v);
        }

        // Incremental evaluation only orders the work set: cells outside
        // it are clean, and their current values read as constants.
        let order = workbook::topological_order_restricted(self, &work);
        let mut circular = None;
        let placed: BTreeSet<&CellId> = order.iter().collect();
        let leftover: Vec<CellId> = work
            .iter()
            // Literal seeds are never "ordered" — only formula cells can
            // be stuck on a cycle.
            .filter(|n| self.formulas.contains_key(*n) && !placed.contains(*n))
            .cloned()
            .collect();
        if !leftover.is_empty() {
            let members = workbook::circular_members(self, &leftover);
            if !members.is_empty() {
                circular = Some(members);
            }
        }

        for node in order {
            self.evaluate_node(&node);
        }
        self.dirty.clear();
        match circular {
            Some(cells) => Err(EngineError::CircularReference(cells)),
            None => Ok(()),
        }
    }

    /// Serializes the workbook to XLSX bytes. Formula cells carry their
    /// cached results, so a `to_xlsx` → `from_xlsx` round-trip preserves
    /// values without a recalculation.
    ///
    /// # Errors
    ///
    /// [`EngineError::Xlsx`] when the codec rejects the workbook.
    pub fn to_xlsx(&self) -> Result<Vec<u8>, EngineError> {
        crate::xlsx_codec::to_xlsx(self)
    }

    /// Loads a workbook from XLSX bytes. Formula cells come back with
    /// their cached results (the file's stored values); a
    /// [`recalculate`](Self::recalculate) refreshes them.
    ///
    /// # Errors
    ///
    /// [`EngineError::Xlsx`] when the bytes are not a readable workbook;
    /// [`EngineError::Formula`] / [`EngineError::Graph`] when a stored
    /// formula cannot be parsed or modeled;
    /// [`EngineError::DuplicateSheet`] for duplicated sheet names.
    pub fn from_xlsx(data: &[u8]) -> Result<Self, EngineError> {
        crate::xlsx_codec::from_xlsx(data)
    }

    // ------------------------------------------------------------------
    // Internals
    // ------------------------------------------------------------------

    /// The sheets map, for the XLSX codec.
    pub(crate) fn sheets_ref(&self) -> &BTreeMap<String, Sheet> {
        &self.sheets
    }

    /// Creates a sheet and its graph when absent (codec load path).
    pub(crate) fn open_sheet(&mut self, sheet: &str) {
        self.sheets.entry(sheet.to_string()).or_default();
        self.graphs.entry(sheet.to_string()).or_default();
    }

    /// Stores a literal cell and its cached value (codec load path).
    pub(crate) fn insert_literal(
        &mut self,
        sheet: &str,
        row: u32,
        col: u32,
        cell: Cell,
        value: formula_lang::Value,
    ) {
        let node = (sheet.to_string(), row, col);
        self.unregister_formula(&node);
        self.sheets
            .entry(node.0.clone())
            .or_default()
            .set(row, col, cell);
        self.values.insert(node, value);
    }

    /// Inserts a cached value directly (codec load path — a formula's
    /// stored result).
    pub(crate) fn insert_cached(&mut self, node: CellId, value: formula_lang::Value) {
        self.values.insert(node, value);
    }

    /// Registers a formula loaded from a file: full bookkeeping, but not
    /// dirty — the file's cached results are authoritative until an edit.
    pub(crate) fn register_loaded_formula(
        &mut self,
        sheet: &str,
        row: u32,
        col: u32,
        text: &str,
    ) -> Result<(), EngineError> {
        self.register_formula(sheet, row, col, text, false)
    }

    /// The workbook's topological order, cached across recalculations.
    /// Invalidated whenever edges change.
    fn topo(&mut self) -> Vec<CellId> {
        if let Some(order) = &self.order_cache {
            return order.clone();
        }
        let order = workbook::topological_order(self);
        self.order_cache = Some(order.clone());
        order
    }

    /// The sheet's local graph, for the workbook algorithms.
    pub(crate) fn graph_for(&self, sheet: &str) -> Option<&DependencyGraph> {
        self.graphs.get(sheet)
    }

    /// Cross-sheet precedents of a formula cell.
    pub(crate) fn cross_precedents(&self, node: &CellId) -> Option<&BTreeSet<CellId>> {
        self.cross_precedents.get(node)
    }

    /// Cross-sheet dependents of any cell.
    pub(crate) fn cross_dependents(&self, node: &CellId) -> Option<&BTreeSet<CellId>> {
        self.cross_dependents.get(node)
    }

    /// Every formula cell, for the workbook algorithms.
    pub(crate) fn formula_nodes(&self) -> BTreeSet<CellId> {
        self.formulas.keys().cloned().collect()
    }

    /// The cached value of one cell (resolver-facing read).
    pub(crate) fn cached(&self, node: &CellId) -> Option<formula_lang::Value> {
        self.values.get(node).cloned()
    }

    /// Registers formula text at a cell: parse, stub-rewrite, edge
    /// bookkeeping. The cell's previous formula (if any) is replaced.
    fn set_formula(
        &mut self,
        sheet: &str,
        row: u32,
        col: u32,
        text: &str,
    ) -> Result<(), EngineError> {
        self.register_formula(sheet, row, col, text, true)
    }

    /// Registration core. `mark_dirty` is `true` for user edits (the cell
    /// needs recomputation) and `false` for file loads (the file's cached
    /// results are authoritative until something changes).
    fn register_formula(
        &mut self,
        sheet: &str,
        row: u32,
        col: u32,
        text: &str,
        mark_dirty: bool,
    ) -> Result<(), EngineError> {
        // Parse and plan first — nothing mutates until this cannot fail.
        let prepared = crate::qualified::prepare(text)?;
        let registration = if prepared.stubs.is_empty() {
            Some(String::from(text))
        } else {
            prepared.local_registration_text()
        };
        let has_stubs = !prepared.stubs.is_empty();
        let volatile = prepared.volatile;
        // Cross-sheet precedents: every cell of every qualified rectangle
        // — a range edit anywhere inside must reach the reader (local
        // ranges get the same expansion inside `sheet-core`).
        let targets: BTreeSet<CellId> = prepared
            .stubs
            .iter()
            .flat_map(|s| {
                let sheet = s.target.0.clone();
                let base = (s.target.1, s.target.2);
                (0..s.rows).flat_map(move |r| {
                    let sheet = sheet.clone();
                    (0..s.cols).map(move |c| (sheet.clone(), base.0 + r, base.1 + c))
                })
            })
            .collect();

        let node = (sheet.to_string(), row, col);
        // Retire the previous formula's bookkeeping (no-op when the cell
        // was not a formula).
        self.unregister_formula(&node);

        // Local edges: pure-local formulas register their own text;
        // qualified formulas register the reconstructed local-only text so
        // the sheet's graph never sees stub coordinates.
        let graph = self.graphs.entry(node.0.clone()).or_default();
        if let Some(reg) = registration.as_deref() {
            graph.add_formula((row, col), reg)?;
        }

        self.sheets
            .entry(node.0.clone())
            .or_default()
            .set(row, col, Cell::formula(text));
        self.formulas.insert(node.clone(), prepared);
        if has_stubs {
            self.qualified.insert(node.clone());
            for t in &targets {
                self.cross_dependents
                    .entry(t.clone())
                    .or_default()
                    .insert(node.clone());
            }
            self.cross_precedents.insert(node.clone(), targets);
        }
        if volatile {
            self.volatile.insert(node.clone());
        }
        if mark_dirty {
            self.dirty.insert(node);
        }
        self.order_cache = None;
        Ok(())
    }
    /// Removes a cell's formula bookkeeping: parsed form, edge flags,
    /// cached value, and the local graph's edges for the cell. No-op for a
    /// cell that was not a formula.
    fn unregister_formula(&mut self, node: &CellId) {
        if self.formulas.remove(node).is_none() {
            return;
        }
        self.order_cache = None;
        let (sheet, row, col) = (node.0.clone(), node.1, node.2);
        if let Some(graph) = self.graphs.get_mut(&sheet) {
            graph.remove_formula((row, col));
        }
        if let Some(targets) = self.cross_precedents.remove(node) {
            for t in targets {
                if let Some(readers) = self.cross_dependents.get_mut(&t) {
                    readers.remove(node);
                    if readers.is_empty() {
                        self.cross_dependents.remove(&t);
                    }
                }
            }
        }
        self.qualified.remove(node);
        self.volatile.remove(node);
        self.values.remove(node);
    }

    /// Evaluates one formula cell and caches the result. Errors are
    /// values: `#DIV/0!` from arithmetic lands in the cache as
    /// `Value::Error` exactly like a referenced error cell does.
    fn evaluate_node(&mut self, node: &CellId) {
        let Some(prepared) = self.formulas.get(node) else {
            return;
        };
        let resolver = EngineResolver::new(self, &node.0, prepared);
        let evaluated = formula_lang::evaluate(&prepared.expr, &resolver);
        let value = match evaluated {
            Ok(v) => v,
            // Evaluation-produced worksheet errors carry the literal.
            Err(formula_lang::FormulaError::Eval(e)) => formula_lang::Value::Error(e),
            // Parse-validated failures (unknown function, recursion limit,
            // invalid range) cannot reach evaluation; `#VALUE!` is the
            // spreadsheet-honest answer if one ever does.
            Err(_) => formula_lang::Value::Error(ExcelError::Value),
        };
        self.values.insert(node.clone(), value);
    }
}

impl core::fmt::Debug for Engine {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Engine")
            .field("sheets", &self.sheets)
            .field("graphs", &self.graphs)
            .field("formulas", &self.formulas.keys().collect::<Vec<_>>())
            .field("qualified", &self.qualified)
            .field("volatile", &self.volatile)
            .field("values", &self.values)
            .field("dirty", &self.dirty)
            .finish_non_exhaustive()
    }
}

/// Parses a non-formula literal into its stored cell and cached value.
/// Total: anything not a number, boolean, or error literal is text.
fn literal(input: &str) -> (Cell, formula_lang::Value) {
    if let Some(v) = ExcelError::from_literal(input) {
        return (Cell::error(sheet_error(v)), formula_lang::Value::Error(v));
    }
    match input.to_ascii_uppercase().as_str() {
        "TRUE" => return (Cell::boolean(true), formula_lang::Value::Boolean(true)),
        "FALSE" => return (Cell::boolean(false), formula_lang::Value::Boolean(false)),
        _ => {}
    }
    if let Some(n) = numeric(input) {
        return (Cell::number(n), formula_lang::Value::Number(n));
    }
    (
        Cell::text(String::from(input)),
        formula_lang::Value::Text(String::from(input)),
    )
}

/// Excel-flavored numeric literal: a finite float whose first character
/// could begin a number. `f64::from_str` also accepts `"inf"`/`"NaN"` —
/// spreadsheets store those as text.
fn numeric(input: &str) -> Option<f64> {
    let first = input.chars().next()?;
    if !(first.is_ascii_digit() || first == '+' || first == '-' || first == '.') {
        return None;
    }
    input.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// Maps `formula-lang`'s error values onto `sheet-core`'s (which has two
/// more for array semantics; the engine's literal cells use the nearest
/// canonical spelling).
fn sheet_error(e: ExcelError) -> sheet_core::ExcelError {
    match e {
        ExcelError::DivZero => sheet_core::ExcelError::Div0,
        ExcelError::NA => sheet_core::ExcelError::Na,
        ExcelError::Name => sheet_core::ExcelError::Name,
        ExcelError::Null => sheet_core::ExcelError::Null,
        ExcelError::Num => sheet_core::ExcelError::Num,
        ExcelError::Ref => sheet_core::ExcelError::Ref,
        ExcelError::Value => sheet_core::ExcelError::Value,
    }
}
