#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Spec tests 2, 3, 10 — formulas, dependency chains, error propagation —
//! plus the incremental contract.

use formula_lang::Value;
use sheet_engine::{Engine, EngineError};

#[test]
fn formula_over_cells_computes() {
    let mut e = Engine::new();
    e.set_cell("Sheet1", 0, 0, "2").expect("set"); // A1
    e.set_cell("Sheet1", 0, 1, "40").expect("set"); // B1
    e.set_cell("Sheet1", 0, 2, "=A1+B1").expect("set"); // C1
    e.recalculate().expect("recalculates");
    assert_eq!(e.get_value("Sheet1", 0, 2), Some(Value::Number(42.0)));
}

#[test]
fn editing_a_literal_updates_the_dependency_chain_incrementally() {
    let mut e = Engine::new();
    e.set_cell("Sheet1", 0, 0, "1").expect("set"); // A1 = 1
    e.set_cell("Sheet1", 1, 0, "=A1+1").expect("set"); // A2 = 2
    e.set_cell("Sheet1", 2, 0, "=A2*10").expect("set"); // A3 = 20
    e.recalculate().expect("recalculates");

    // Edit the root; only the chain re-evaluates.
    e.set_cell("Sheet1", 0, 0, "5").expect("set");
    e.recalculate_incremental(&[(String::from("Sheet1"), 0, 0)])
        .expect("recalculates");
    assert_eq!(e.get_value("Sheet1", 1, 0), Some(Value::Number(6.0)));
    assert_eq!(e.get_value("Sheet1", 2, 0), Some(Value::Number(60.0)));
}

#[test]
fn incremental_with_no_edits_drains_pending_set_cell_work() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "3").expect("set");
    e.set_cell("S", 0, 1, "=A1*2").expect("set");
    // No `edited` argument — the dirty set from set_cell drives it.
    e.recalculate_incremental(&[]).expect("recalculates");
    assert_eq!(e.get_value("S", 0, 1), Some(Value::Number(6.0)));
}

#[test]
fn errors_propagate_through_arithmetic() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "=1/0").expect("set"); // A1 = #DIV/0!
    e.set_cell("S", 0, 1, "=A1+1").expect("set"); // B1 reads the error
    e.set_cell("S", 0, 2, "=IFERROR(A1,99)").expect("set"); // C1 recovers
    e.recalculate().expect("recalculates (errors are values)");

    assert_eq!(
        e.get_value("S", 0, 0),
        Some(Value::Error(formula_lang::ExcelError::DivZero))
    );
    assert_eq!(
        e.get_value("S", 0, 1),
        Some(Value::Error(formula_lang::ExcelError::DivZero))
    );
    assert_eq!(e.get_value("S", 0, 2), Some(Value::Number(99.0)));
}

#[test]
fn stored_error_literals_propagate_too() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "#REF!").expect("set");
    e.set_cell("S", 0, 1, "=A1+1").expect("set");
    e.recalculate().expect("recalculates");
    assert_eq!(
        e.get_value("S", 0, 1),
        Some(Value::Error(formula_lang::ExcelError::Ref))
    );
}

#[test]
fn circular_references_are_detected_not_looped() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "=B1").expect("set"); // A1 = B1
    e.set_cell("S", 0, 1, "=A1").expect("set"); // B1 = A1
    let err = e.recalculate().expect_err("circular");
    let EngineError::CircularReference(cells) = err else {
        panic!("expected CircularReference, got {err:?}");
    };
    let mut cells = cells;
    cells.sort();
    assert_eq!(
        cells,
        vec![(String::from("S"), 0, 0), (String::from("S"), 0, 1)]
    );
}

#[test]
fn cells_downstream_of_a_cycle_are_not_themselves_circular() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "=B1+1").expect("set"); // A1 ↔ B1
    e.set_cell("S", 0, 1, "=A1+1").expect("set");
    e.set_cell("S", 0, 2, "=D1+1").expect("set"); // C1 reads D1 — fine
    e.set_cell("S", 0, 3, "7").expect("set"); // D1
    let err = e.recalculate().expect_err("circular");
    let EngineError::CircularReference(cells) = err else {
        panic!("expected CircularReference");
    };
    let mut cells = cells;
    cells.sort();
    assert_eq!(
        cells,
        vec![(String::from("S"), 0, 0), (String::from("S"), 0, 1)]
    );
    // Everything off the cycle still computed.
    assert_eq!(e.get_value("S", 0, 2), Some(Value::Number(8.0)));
}

#[test]
fn a_500_cell_chain_settles_in_topological_order() {
    let rows = 500u32;
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "1").expect("set");
    for r in 1..rows {
        e.set_cell("S", r, 0, &format!("=A{r}+1")).expect("set");
    }
    e.recalculate().expect("recalculates");
    // The whole chain evaluated, in order: the tail saw every predecessor.
    assert_eq!(
        e.get_value("S", rows - 1, 0),
        Some(Value::Number(f64::from(rows)))
    );
    assert_eq!(e.get_value("S", 249, 0), Some(Value::Number(250.0)));
}

#[test]
fn overwriting_a_formula_swaps_its_edges() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "1").expect("set"); // A1
    e.set_cell("S", 1, 0, "2").expect("set"); // A2
    e.set_cell("S", 0, 1, "=A1*100").expect("set"); // B1 reads A1
    e.recalculate().expect("ok");
    assert_eq!(e.get_value("S", 0, 1), Some(Value::Number(100.0)));

    // B1 now reads A2 — A1's edits no longer reach it.
    e.set_cell("S", 0, 1, "=A2*100").expect("set");
    e.set_cell("S", 0, 0, "5").expect("set");
    e.recalculate_incremental(&[(String::from("S"), 0, 0)])
        .expect("ok");
    assert_eq!(e.get_value("S", 0, 1), Some(Value::Number(200.0)));

    e.set_cell("S", 1, 0, "3").expect("set");
    e.recalculate_incremental(&[(String::from("S"), 1, 0)])
        .expect("ok");
    assert_eq!(e.get_value("S", 0, 1), Some(Value::Number(300.0)));
}

#[test]
fn replacing_a_formula_with_a_literal_detaches_readers_correctly() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "=1/0").expect("set"); // A1 errors
    e.set_cell("S", 0, 1, "=A1+1").expect("set"); // B1 propagates
    e.recalculate().expect("ok");
    assert!(e.get_value("S", 0, 1).is_some_and(|v| v.is_error()));

    // A1 becomes the literal 5 — B1 now reads 6.
    e.set_cell("S", 0, 0, "5").expect("set");
    e.recalculate_incremental(&[]).expect("ok");
    assert_eq!(e.get_value("S", 0, 1), Some(Value::Number(6.0)));
}

#[test]
fn vlookup_over_a_function_grid() {
    let mut e = Engine::new();
    for r in 0..10u32 {
        e.set_cell("S", r, 0, &(r * 10).to_string()).expect("set");
        e.set_cell("S", r, 1, &format!("val{r}")).expect("set");
    }
    e.set_cell("S", 0, 3, "=VLOOKUP(30,A1:B10,2)").expect("set");
    e.set_cell("S", 1, 3, "=VLOOKUP(35,A1:B10,2,FALSE)")
        .expect("set"); // exact miss
    e.set_cell("S", 2, 3, "=SUM(A1:A10)").expect("set");
    e.recalculate().expect("recalculates");
    assert_eq!(
        e.get_value("S", 0, 3),
        Some(Value::Text(String::from("val3")))
    );
    assert_eq!(
        e.get_value("S", 1, 3),
        Some(Value::Error(formula_lang::ExcelError::NA))
    );
    assert_eq!(e.get_value("S", 2, 3), Some(Value::Number(450.0)));
}
