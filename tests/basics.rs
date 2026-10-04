#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Spec tests 1, 5 and the literal/input grammar: literals of every type,
//! volatile tracking, and the `set_cell` input language.

use formula_lang::Value;
use sheet_engine::Engine;

#[test]
fn number_text_and_bool_literals_round_trip() {
    let mut e = Engine::new();
    e.set_cell("Sheet1", 0, 0, "42").expect("set");
    e.set_cell("Sheet1", 1, 0, "hello").expect("set");
    e.set_cell("Sheet1", 2, 0, "TRUE").expect("set");
    e.set_cell("Sheet1", 3, 0, "false").expect("set");
    e.set_cell("Sheet1", 4, 0, "-3.5").expect("set");
    e.set_cell("Sheet1", 5, 0, "1e3").expect("set");

    assert_eq!(e.get_value("Sheet1", 0, 0), Some(Value::Number(42.0)));
    assert_eq!(
        e.get_value("Sheet1", 1, 0),
        Some(Value::Text(String::from("hello")))
    );
    assert_eq!(e.get_value("Sheet1", 2, 0), Some(Value::Boolean(true)));
    assert_eq!(e.get_value("Sheet1", 3, 0), Some(Value::Boolean(false)));
    assert_eq!(e.get_value("Sheet1", 4, 0), Some(Value::Number(-3.5)));
    assert_eq!(e.get_value("Sheet1", 5, 0), Some(Value::Number(1000.0)));
}

#[test]
fn input_grammar_excel_flavored() {
    let mut e = Engine::new();
    // Numeric-looking input becomes numbers…
    e.set_cell("S", 0, 0, "  7  ").expect("set"); // trimmed
    e.set_cell("S", 1, 0, "+8").expect("set");
    e.set_cell("S", 2, 0, ".5").expect("set");
    // …and words that f64 would parse stay text (Excel stores them so).
    e.set_cell("S", 3, 0, "inf").expect("set");
    e.set_cell("S", 4, 0, "NaN").expect("set");
    // Error literals are error values.
    e.set_cell("S", 5, 0, "#DIV/0!").expect("set");
    e.set_cell("S", 6, 0, "#n/a").expect("set");
    // Empty text is empty text, not a formula.
    e.set_cell("S", 7, 0, "").expect("set");

    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(7.0)));
    assert_eq!(e.get_value("S", 1, 0), Some(Value::Number(8.0)));
    assert_eq!(e.get_value("S", 2, 0), Some(Value::Number(0.5)));
    assert_eq!(
        e.get_value("S", 3, 0),
        Some(Value::Text(String::from("inf")))
    );
    assert_eq!(
        e.get_value("S", 4, 0),
        Some(Value::Text(String::from("NaN")))
    );
    assert_eq!(
        e.get_value("S", 5, 0),
        Some(Value::Error(formula_lang::ExcelError::DivZero))
    );
    assert_eq!(
        e.get_value("S", 6, 0),
        Some(Value::Error(formula_lang::ExcelError::NA))
    );
    assert_eq!(e.get_value("S", 7, 0), Some(Value::Text(String::new())));
}

#[test]
fn volatile_functions_are_tracked() {
    let mut e = Engine::new();
    e.set_cell("Sheet1", 0, 0, "=NOW()").expect("set");
    e.set_cell("Sheet1", 1, 0, "=TODAY()+1").expect("set");
    e.set_cell("Sheet1", 2, 0, "=1+1").expect("set");
    e.set_cell("Other", 0, 0, "=OFFSET(A1,1,0)").expect("set");

    let mut volatile = e.volatile_cells();
    volatile.sort();
    assert_eq!(
        volatile,
        vec![
            (String::from("Other"), 0, 0),
            (String::from("Sheet1"), 0, 0),
            (String::from("Sheet1"), 1, 0),
        ]
    );
}

#[test]
fn replacing_a_formula_updates_volatile_tracking() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "=NOW()").expect("set");
    assert_eq!(e.volatile_cells().len(), 1);
    // Overwrite with a pure formula: no longer volatile.
    e.set_cell("S", 0, 0, "=1+2").expect("set");
    assert_eq!(e.volatile_cells(), Vec::<(String, u32, u32)>::new());
    // Overwrite with a literal: still not volatile, and the value is the
    // literal (no recalculation needed).
    e.set_cell("S", 0, 0, "9").expect("set");
    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(9.0)));
}

#[test]
fn formulas_have_no_value_until_recalculated() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "=1+1").expect("set");
    assert_eq!(e.get_value("S", 0, 0), None);
    e.recalculate().expect("recalculates");
    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(2.0)));
}

#[test]
fn sheets_are_created_on_demand_and_listed_sorted() {
    let mut e = Engine::new();
    assert_eq!(e.sheet_names(), Vec::<String>::new());
    e.set_cell("Zeta", 0, 0, "1").expect("set");
    e.set_cell("Alpha", 0, 0, "2").expect("set");
    assert_eq!(
        e.sheet_names(),
        vec![String::from("Alpha"), String::from("Zeta")]
    );
    assert!(e.has_sheet("Alpha"));
    // Reads from a sheet that was never created are simply blank.
    assert_eq!(e.get_value("Nope", 0, 0), None);
}

#[test]
fn malformed_formulas_are_typed_errors_not_panics() {
    let mut e = Engine::new();
    assert!(e.set_cell("S", 0, 0, "=SUM(").is_err());
    assert!(e.set_cell("S", 0, 0, "=Nope!(1)").is_err());
    assert!(e.set_cell("S", 0, 0, "=A1048577").is_err());
    assert!(e.set_cell("S", 0, 0, "=").is_err());
    // …and none of them left anything behind.
    assert_eq!(e.get_value("S", 0, 0), None);
}

#[test]
fn self_reference_is_a_one_cell_cycle() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "=A1+1").expect("set");
    let err = e.recalculate().expect_err("circular");
    let sheet_engine::EngineError::CircularReference(cells) = err else {
        panic!("expected CircularReference");
    };
    assert_eq!(cells, vec![(String::from("S"), 0, 0)]);
    // A cycle cell has no computed value — and no infinite loop happened.
    assert_eq!(e.get_value("S", 0, 0), None);
}
