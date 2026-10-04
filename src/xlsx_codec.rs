//! XLSX serialization — the engine's bytes in and out, over `sheet-xlsx`.
//!
//! Sheets serialize in name order; cells in row-major order. Formula cells
//! carry their cached results (the value side of `SpreadsheetML`), so a
//! `to_xlsx` → `from_xlsx` round-trip preserves values without
//! recalculating, and `from_xlsx` → `to_xlsx` preserves the file's stored
//! results even when they are stale. Formula text round-trips verbatim
//! (the engine stores it with its leading `=`; the file format stores it
//! without).

use crate::error::EngineError;
use crate::CellId;
use formula_lang::{ExcelError, Value};
use sheet_core::{Cell, CellValue};
use sheet_xlsx::{
    read_xlsx, write_xlsx, SharedStrings, SheetData, XlsxCell, XlsxValue, XlsxWorkbook,
};
use std::string::{String, ToString};
use std::vec::Vec;

/// Serializes the workbook. See [`Engine::to_xlsx`](crate::Engine::to_xlsx).
///
/// # Errors
///
/// [`EngineError::Xlsx`] when the codec rejects the workbook.
pub(crate) fn to_xlsx(engine: &crate::Engine) -> Result<Vec<u8>, EngineError> {
    let mut workbook = XlsxWorkbook {
        sheets: Vec::new(),
        shared_strings: SharedStrings::default(),
        styles: sheet_xlsx::Styles::default(),
    };
    for (name, sheet) in engine.sheets_ref() {
        let mut data = SheetData::new(name.clone());
        for ((row, col), cell) in sheet.iter() {
            let xcell = match &cell.value {
                CellValue::Number(n) => XlsxCell::new(XlsxValue::Number(*n)),
                CellValue::Text(t) => XlsxCell::new(XlsxValue::InlineString(t.clone())),
                CellValue::Boolean(b) => XlsxCell::new(XlsxValue::Boolean(*b)),
                CellValue::Error(e) => XlsxCell::new(XlsxValue::Error(e.to_string())),
                CellValue::Blank => XlsxCell::new(XlsxValue::InlineString(String::new())),
                CellValue::Formula(text) => {
                    let node: CellId = (name.clone(), *row, *col);
                    let cached = cached_as_xlsx(engine.cached(&node));
                    let body = text.strip_prefix('=').unwrap_or(text);
                    XlsxCell::formula(body, cached)
                }
            };
            data.cells.insert((*row, *col), xcell);
        }
        workbook.sheets.push(data);
    }
    Ok(write_xlsx(&workbook)?)
}

/// Loads a workbook. See [`Engine::from_xlsx`](crate::Engine::from_xlsx).
///
/// Loaded formulas are registered but *not* marked dirty — their cached
/// results come from the file, so an edit to one cell after loading
/// recalculates only that cell's subgraph.
///
/// # Errors
///
/// As documented on [`Engine::from_xlsx`](crate::Engine::from_xlsx).
pub(crate) fn from_xlsx(data: &[u8]) -> Result<crate::Engine, EngineError> {
    let workbook = read_xlsx(data)?;
    let mut engine = crate::Engine::new();
    for data in &workbook.sheets {
        if engine.has_sheet(&data.name) {
            return Err(EngineError::DuplicateSheet(data.name.clone()));
        }
        // The sheet and its graph must exist before formula registration.
        engine.open_sheet(&data.name);
        for ((row, col), xcell) in &data.cells {
            if let Some(body) = &xcell.formula {
                let text = format!("={body}");
                engine.register_loaded_formula(&data.name, *row, *col, &text)?;
                if let Some(v) = cached_from_xlsx(&xcell.value, &workbook.shared_strings) {
                    engine.insert_cached((data.name.clone(), *row, *col), v);
                }
            } else {
                let (cell, value) = literal_from_xlsx(&xcell.value, &workbook.shared_strings);
                engine.insert_literal(&data.name, *row, *col, cell, value);
            }
        }
    }
    Ok(engine)
}

/// Maps a cached engine value to the file's cached-result encoding.
fn cached_as_xlsx(cached: Option<Value>) -> XlsxValue {
    match cached {
        Some(Value::Number(n)) => XlsxValue::Number(n),
        Some(Value::Text(t)) => XlsxValue::FormulaString(t),
        Some(Value::Boolean(b)) => XlsxValue::Boolean(b),
        Some(Value::Error(e)) => XlsxValue::Error(String::from(e.literal())),
        Some(Value::Empty) | None => XlsxValue::FormulaString(String::new()),
    }
}

/// Maps the file's cached-result encoding back to an engine value.
/// `None` when no result is stored (an empty formula string, per the
/// reader's contract).
fn cached_from_xlsx(value: &XlsxValue, shared: &SharedStrings) -> Option<Value> {
    match value {
        XlsxValue::Number(n) => Some(Value::Number(*n)),
        XlsxValue::Boolean(b) => Some(Value::Boolean(*b)),
        XlsxValue::Error(lit) => error_from_xlsx(lit).map(Value::Error),
        XlsxValue::FormulaString(s) => {
            if s.is_empty() {
                None
            } else {
                Some(Value::Text(s.clone()))
            }
        }
        XlsxValue::SharedString(i) => shared.get(*i).map(|t| Value::Text(String::from(t))),
        XlsxValue::InlineString(s) => Some(Value::Text(s.clone())),
    }
}

/// Converts a value-only cell to its stored cell and cached value.
fn literal_from_xlsx(value: &XlsxValue, shared: &SharedStrings) -> (Cell, Value) {
    match value {
        XlsxValue::Number(n) => (Cell::number(*n), Value::Number(*n)),
        XlsxValue::Boolean(b) => (Cell::boolean(*b), Value::Boolean(*b)),
        XlsxValue::Error(lit) => match error_from_xlsx(lit) {
            Some(e) => (Cell::error(sheet_core_error(e)), Value::Error(e)),
            None => (Cell::text(lit.clone()), Value::Text(lit.clone())),
        },
        // Inline and formula strings both carry the text in the cell.
        XlsxValue::InlineString(s) | XlsxValue::FormulaString(s) => {
            (Cell::text(s.clone()), Value::Text(s.clone()))
        }
        XlsxValue::SharedString(i) => {
            let text = shared.get(*i).map(String::from).unwrap_or_default();
            (Cell::text(text.clone()), Value::Text(text))
        }
    }
}

/// Parses an error literal with `formula-lang`, falling back to
/// `sheet-core`'s table (two more spellings, mapped onto the nearest
/// canonical value).
fn error_from_xlsx(lit: &str) -> Option<ExcelError> {
    if let Some(e) = ExcelError::from_literal(lit) {
        return Some(e);
    }
    match lit.parse::<sheet_core::ExcelError>() {
        // `#SPILL!` is a blocked dynamic array — `#VALUE!` is its nearest
        // canonical neighbor; every other extra spelling maps to `#NAME?`.
        Ok(sheet_core::ExcelError::Spill) => Some(ExcelError::Value),
        Ok(_) => Some(ExcelError::Name),
        Err(()) => None,
    }
}

/// Maps a `formula-lang` error to `sheet-core`'s (a superset) for cell
/// storage.
fn sheet_core_error(e: ExcelError) -> sheet_core::ExcelError {
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
