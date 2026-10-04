#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! XLSX files crafted with `sheet-xlsx` directly — value encodings the
//! engine's own writer never produces (shared strings, error literals
//! outside `formula-lang`'s table, every cached-result type), proving
//! `from_xlsx` is a *reader* for the format, not just for its own output.

use formula_lang::Value;
use sheet_engine::Engine;
use sheet_xlsx::{SheetData, XlsxCell, XlsxValue, XlsxWorkbook};

#[test]
fn reads_shared_strings_booleans_and_errors_from_crafted_files() {
    let mut shared = sheet_xlsx::SharedStrings::default();
    let hello = shared.insert("hello"); // index 0

    let mut data = SheetData::new("Crafted");
    data.cells
        .insert((0, 0), XlsxCell::new(XlsxValue::SharedString(hello)));
    data.cells
        .insert((1, 0), XlsxCell::new(XlsxValue::Boolean(false)));
    data.cells.insert(
        (2, 0),
        XlsxCell::new(XlsxValue::Error(String::from("#SPILL!"))),
    );
    data.cells.insert(
        (3, 0),
        XlsxCell::new(XlsxValue::Error(String::from("#N/A"))),
    );

    let bytes = write_xlsx_bytes(XlsxWorkbook {
        sheets: vec![data],
        shared_strings: shared,
        styles: Default::default(),
    });

    let e = Engine::from_xlsx(&bytes).expect("reads");
    assert_eq!(
        e.get_value("Crafted", 0, 0),
        Some(Value::Text(String::from("hello")))
    );
    assert_eq!(e.get_value("Crafted", 1, 0), Some(Value::Boolean(false)));
    // `#SPILL!` is not in formula-lang's table: mapped to its nearest
    // canonical neighbor (#VALUE!) as an error value, not text.
    assert_eq!(
        e.get_value("Crafted", 2, 0),
        Some(Value::Error(formula_lang::ExcelError::Value))
    );
    assert_eq!(
        e.get_value("Crafted", 3, 0),
        Some(Value::Error(formula_lang::ExcelError::NA))
    );
}

#[test]
fn reads_formula_cells_with_every_cached_result_type() {
    let mut data = SheetData::new("S");
    data.cells
        .insert((0, 0), XlsxCell::formula("A2+A3", XlsxValue::Number(3.0)));
    data.cells
        .insert((1, 0), XlsxCell::new(XlsxValue::Number(1.0)));
    data.cells
        .insert((2, 0), XlsxCell::new(XlsxValue::Number(2.0)));
    data.cells.insert(
        (0, 1),
        XlsxCell::formula("A2&\"!\"", XlsxValue::FormulaString(String::from("1!"))),
    );
    data.cells.insert(
        (0, 2),
        XlsxCell::formula("A2>A3", XlsxValue::Boolean(false)),
    );
    data.cells.insert(
        (0, 3),
        XlsxCell::formula("A2/A4", XlsxValue::Error(String::from("#DIV/0!"))),
    );

    let bytes = write_xlsx_bytes(XlsxWorkbook {
        sheets: vec![data],
        shared_strings: Default::default(),
        styles: Default::default(),
    });

    let mut e = Engine::from_xlsx(&bytes).expect("reads");
    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(3.0)));
    assert_eq!(
        e.get_value("S", 0, 1),
        Some(Value::Text(String::from("1!")))
    );
    assert_eq!(e.get_value("S", 0, 2), Some(Value::Boolean(false)));
    assert_eq!(
        e.get_value("S", 0, 3),
        Some(Value::Error(formula_lang::ExcelError::DivZero))
    );

    // And the loaded formulas still calculate: refresh the cache.
    e.recalculate().expect("recalculates");
    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(3.0)));
    assert_eq!(
        e.get_value("S", 0, 1),
        Some(Value::Text(String::from("1!")))
    );
}

#[test]
fn writes_every_cached_result_type_and_reads_it_back() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "ab").expect("set");
    e.set_cell("S", 1, 0, "cd").expect("set");
    e.set_cell("S", 0, 1, "=A1&A2").expect("set"); // cached Text
    e.set_cell("S", 1, 1, "=A1<A2").expect("set"); // cached Boolean
    e.set_cell("S", 2, 1, "=1/0").expect("set"); // cached Error
    e.set_cell("S", 3, 1, "=A1&\"x\"").expect("set"); // recalculated below
    e.recalculate().expect("ok");
    e.set_cell("S", 3, 1, "=1+1").expect("set"); // formula with no cache yet

    let bytes = e.to_xlsx().expect("writes");
    let back = Engine::from_xlsx(&bytes).expect("reads");
    assert_eq!(
        back.get_value("S", 0, 1),
        Some(Value::Text(String::from("abcd")))
    );
    assert_eq!(back.get_value("S", 1, 1), Some(Value::Boolean(true)));
    assert_eq!(
        back.get_value("S", 2, 1),
        Some(Value::Error(formula_lang::ExcelError::DivZero))
    );
    // No cached result was stored for the freshly edited formula.
    assert_eq!(back.get_value("S", 3, 1), None);
}

#[test]
fn formula_cells_without_cached_results_load_clean() {
    let mut data = SheetData::new("S");
    // Empty formula string = no cached result, per the reader contract.
    data.cells.insert(
        (0, 0),
        XlsxCell::formula("1+1", XlsxValue::FormulaString(String::new())),
    );

    let bytes = write_xlsx_bytes(XlsxWorkbook {
        sheets: vec![data],
        shared_strings: Default::default(),
        styles: Default::default(),
    });
    let mut e = Engine::from_xlsx(&bytes).expect("reads");
    assert_eq!(e.get_value("S", 0, 0), None);
    e.recalculate().expect("ok");
    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(2.0)));
}

/// Serializes a hand-built workbook without the engine's converter.
fn write_xlsx_bytes(wb: XlsxWorkbook) -> Vec<u8> {
    sheet_xlsx::write_xlsx(&wb).expect("crafted workbook is valid")
}
