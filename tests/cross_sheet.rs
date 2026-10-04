#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Spec test 6 — cross-sheet references, resolved through the engine's
//! workbook-level graph and stub rewriting.

use formula_lang::Value;
use sheet_engine::{Engine, EngineError};

#[test]
fn cross_sheet_scalar_reference_resolves() {
    let mut e = Engine::new();
    e.set_cell("Sheet1", 0, 0, "=Sheet2!A1+1").expect("set");
    e.set_cell("Sheet2", 0, 0, "41").expect("set");
    e.recalculate().expect("recalculates");
    assert_eq!(e.get_value("Sheet1", 0, 0), Some(Value::Number(42.0)));
}

#[test]
fn cross_sheet_chain_propagates_an_edit() {
    let mut e = Engine::new();
    e.set_cell("Data", 0, 0, "10").expect("set");
    e.set_cell("Report", 0, 0, "=Data!A1*2").expect("set");
    e.set_cell("Summary", 0, 0, "=Report!A1+1").expect("set");
    e.recalculate().expect("recalculates");
    assert_eq!(e.get_value("Summary", 0, 0), Some(Value::Number(21.0)));

    // One edit; the whole cross-sheet chain follows.
    e.set_cell("Data", 0, 0, "100").expect("set");
    e.recalculate_incremental(&[(String::from("Data"), 0, 0)])
        .expect("recalculates");
    assert_eq!(e.get_value("Report", 0, 0), Some(Value::Number(200.0)));
    assert_eq!(e.get_value("Summary", 0, 0), Some(Value::Number(201.0)));
}

#[test]
fn quoted_sheet_names_and_anchors() {
    let mut e = Engine::new();
    e.set_cell("Q4 Data", 1, 1, "7").expect("set"); // B2
    e.set_cell("S", 0, 0, "='Q4 Data'!$B$2*3").expect("set");
    e.recalculate().expect("recalculates");
    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(21.0)));
}

#[test]
fn cross_sheet_ranges_and_lookups() {
    let mut e = Engine::new();
    for r in 0..5u32 {
        e.set_cell("Data", r, 0, &(r + 1).to_string()).expect("set");
    }
    e.set_cell("S", 0, 0, "=SUM(Data!A1:A5)").expect("set");
    e.set_cell("S", 1, 0, "=VLOOKUP(3,Data!A1:B5,1)")
        .expect("set");
    e.recalculate().expect("recalculates");
    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(15.0)));
    assert_eq!(e.get_value("S", 1, 0), Some(Value::Number(3.0)));

    // Edit under the range; the summary follows.
    e.set_cell("Data", 2, 0, "30").expect("set");
    e.recalculate_incremental(&[]).expect("recalculates");
    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(42.0)));
}

#[test]
fn mixed_local_and_cross_sheet_formulas_register_both_edges() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "1").expect("set"); // A1
    e.set_cell("S", 1, 0, "=Sheet2!A1+A1+1").expect("set"); // A2: local + cross
    e.set_cell("Sheet2", 0, 0, "10").expect("set");
    e.recalculate().expect("recalculates");
    assert_eq!(e.get_value("S", 1, 0), Some(Value::Number(12.0)));

    // Either input edit reaches A2.
    e.set_cell("S", 0, 0, "2").expect("set");
    e.recalculate_incremental(&[]).expect("ok");
    assert_eq!(e.get_value("S", 1, 0), Some(Value::Number(13.0)));
    e.set_cell("Sheet2", 0, 0, "20").expect("set");
    e.recalculate_incremental(&[]).expect("ok");
    assert_eq!(e.get_value("S", 1, 0), Some(Value::Number(23.0)));
}

#[test]
fn reading_a_missing_sheet_is_a_ref_error() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "=Nowhere!A1+1").expect("set");
    e.recalculate().expect("recalculates");
    assert_eq!(
        e.get_value("S", 0, 0),
        Some(Value::Error(formula_lang::ExcelError::Ref))
    );

    // …until the sheet appears: set_cell auto-creates it, the cell is
    // re-evaluated, and the read lands.
    e.set_cell("Nowhere", 0, 0, "9").expect("set");
    e.recalculate_incremental(&[]).expect("ok");
    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(10.0)));
}

#[test]
fn cross_sheet_cycles_are_detected() {
    let mut e = Engine::new();
    e.set_cell("S1", 0, 0, "=S2!A1+1").expect("set");
    e.set_cell("S2", 0, 0, "=S1!A1+1").expect("set");
    let err = e.recalculate().expect_err("circular");
    let EngineError::CircularReference(cells) = err else {
        panic!("expected CircularReference");
    };
    assert_eq!(cells.len(), 2);
}

#[test]
fn stub_coordinates_never_collide_with_high_local_refs() {
    let mut e = Engine::new();
    // A local reference near the very top of the grid, and a cross-sheet
    // read whose stub allocation must land above it.
    e.set_cell("S", 1_048_574, 16_383, "5").expect("set"); // XFD1048575
    e.set_cell("S", 0, 0, "=Other!A1+XFD1048575").expect("set");
    e.set_cell("Other", 0, 0, "1").expect("set");
    e.recalculate().expect("recalculates");
    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(6.0)));
}
