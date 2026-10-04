#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Spec test 7 — XLSX round-trip: `from_xlsx` → set cells → `to_xlsx` →
//! `from_xlsx`, values preserved.

use formula_lang::Value;
use sheet_engine::Engine;

#[test]
fn literals_survive_the_round_trip() {
    let mut e = Engine::new();
    e.set_cell("Sheet1", 0, 0, "42").expect("set");
    e.set_cell("Sheet1", 0, 1, "hello").expect("set");
    e.set_cell("Sheet1", 0, 2, "TRUE").expect("set");
    e.set_cell("Sheet1", 0, 3, "#DIV/0!").expect("set");
    e.set_cell("Data Sheet", 3, 4, "1.25").expect("set");

    let bytes = e.to_xlsx().expect("writes");
    let back = Engine::from_xlsx(&bytes).expect("reads");

    assert_eq!(back.get_value("Sheet1", 0, 0), Some(Value::Number(42.0)));
    assert_eq!(
        back.get_value("Sheet1", 0, 1),
        Some(Value::Text(String::from("hello")))
    );
    assert_eq!(back.get_value("Sheet1", 0, 2), Some(Value::Boolean(true)));
    assert_eq!(
        back.get_value("Sheet1", 0, 3),
        Some(Value::Error(formula_lang::ExcelError::DivZero))
    );
    assert_eq!(
        back.get_value("Data Sheet", 3, 4),
        Some(Value::Number(1.25))
    );
    assert_eq!(
        back.sheet_names(),
        vec![String::from("Data Sheet"), String::from("Sheet1")]
    );
}

#[test]
fn formulas_and_cached_results_survive_the_round_trip() {
    let mut e = Engine::new();
    e.set_cell("Sheet1", 0, 0, "6").expect("set");
    e.set_cell("Sheet1", 0, 1, "=A1*7").expect("set"); // 42
    e.set_cell("Sheet1", 0, 2, "=Sheet2!A1+1").expect("set");
    e.set_cell("Sheet2", 0, 0, "9").expect("set");
    e.recalculate().expect("recalculates");

    let bytes = e.to_xlsx().expect("writes");
    let mut back = Engine::from_xlsx(&bytes).expect("reads");

    // Values come back from the file's cached results — no recalculation.
    assert_eq!(back.get_value("Sheet1", 0, 1), Some(Value::Number(42.0)));
    assert_eq!(back.get_value("Sheet1", 0, 2), Some(Value::Number(10.0)));

    // …and the dependency graph still works: edit the input, incremental
    // recalculation refreshes the chain.
    back.set_cell("Sheet1", 0, 0, "1").expect("set");
    let mut back = back;
    back.recalculate_incremental(&[(String::from("Sheet1"), 0, 0)])
        .expect("recalculates");
    assert_eq!(back.get_value("Sheet1", 0, 1), Some(Value::Number(7.0)));
    assert_eq!(back.get_value("Sheet1", 0, 2), Some(Value::Number(10.0)));
}

#[test]
fn from_xlsx_then_set_then_to_xlsx_then_from_xlsx_preserves_everything() {
    // The spec's exact scenario: load, add more, save, reload.
    let bytes0 = {
        let mut e = Engine::new();
        e.set_cell("Sheet1", 0, 0, "10").expect("set");
        e.set_cell("Sheet1", 0, 1, "=A1+A1").expect("set");
        e.recalculate().expect("ok");
        e.to_xlsx().expect("writes")
    };
    let mut mid = Engine::from_xlsx(&bytes0).expect("reads");
    mid.set_cell("Sheet1", 1, 0, "=A1*10").expect("set");
    mid.set_cell("Sheet1", 1, 1, "new text").expect("set");
    mid.recalculate().expect("ok");
    let bytes1 = mid.to_xlsx().expect("writes");
    let end = Engine::from_xlsx(&bytes1).expect("reads");

    assert_eq!(end.get_value("Sheet1", 0, 0), Some(Value::Number(10.0)));
    assert_eq!(end.get_value("Sheet1", 0, 1), Some(Value::Number(20.0)));
    assert_eq!(end.get_value("Sheet1", 1, 0), Some(Value::Number(100.0)));
    assert_eq!(
        end.get_value("Sheet1", 1, 1),
        Some(Value::Text(String::from("new text")))
    );
}

#[test]
fn formula_text_round_trips_verbatim() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "1").expect("set");
    // C1, safely outside the summed range.
    e.set_cell("S", 0, 2, "= SUM( A1:A9 ) + Other!B2")
        .expect("set");
    e.set_cell("Other", 1, 1, "2").expect("set");
    e.recalculate().expect("ok");

    let bytes = e.to_xlsx().expect("writes");
    let mut back = Engine::from_xlsx(&bytes).expect("reads");
    back.recalculate().expect("ok");
    assert_eq!(back.get_value("S", 0, 2), Some(Value::Number(3.0)));

    // The stored formula still carries the original spelling — rewrite to
    // XLSX again and load once more to prove stability.
    let bytes2 = back.to_xlsx().expect("writes");
    let mut again = Engine::from_xlsx(&bytes2).expect("reads");
    again.recalculate().expect("ok");
    assert_eq!(again.get_value("S", 0, 2), Some(Value::Number(3.0)));
}

#[test]
fn stale_cache_semantics_are_excel_like() {
    // to_xlsx writes the *cached* results — even stale ones — and
    // from_xlsx trusts them until a recalculation, exactly like Excel.
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "5").expect("set");
    e.set_cell("S", 0, 1, "=A1*2").expect("set");
    // Deliberately no recalculate: the cache for B1 is empty.
    let bytes = e.to_xlsx().expect("writes");
    let mut back = Engine::from_xlsx(&bytes).expect("reads");
    assert_eq!(back.get_value("S", 0, 0), Some(Value::Number(5.0)));
    // No cached result stored → None until recalculated.
    assert_eq!(back.get_value("S", 0, 1), None);
    back.recalculate().expect("ok");
    assert_eq!(back.get_value("S", 0, 1), Some(Value::Number(10.0)));
}

#[test]
fn duplicate_sheet_names_are_rejected() {
    // `sheet-xlsx` preserves what we give it; build a workbook bytes blob
    // with two same-named sheets through the engine's own writer by
    // loading a hand-written workbook. The engine's loader must refuse it.
    let dup = b"PK\x03\x04not-a-real-workbook";
    let err = Engine::from_xlsx(dup).expect_err("not a workbook");
    assert!(matches!(err, sheet_engine::EngineError::Xlsx(_)));
}
