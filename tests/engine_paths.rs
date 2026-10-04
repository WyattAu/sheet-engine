#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Engine paths the spec tests don't force: the topological-order cache,
//! volatile cells under incremental recalculation, cycles discovered
//! incrementally, cross-sheet edge retirement, and every error literal.

use formula_lang::ExcelError;
use formula_lang::Value;
use sheet_engine::{Engine, EngineError};

#[test]
fn default_engine_is_an_empty_workbook() {
    let e = Engine::default();
    assert_eq!(e.sheet_names(), Vec::<String>::new());
    assert!(e.volatile_cells().is_empty());
    // Debug renders without panicking (all fields printable).
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "=1+1").expect("set");
    let rendered = format!("{e:?}");
    assert!(rendered.contains("Engine"));
}

#[test]
fn repeated_recalculations_reuse_the_cached_order() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "2").expect("set");
    e.set_cell("S", 0, 1, "=A1*10").expect("set");
    e.recalculate().expect("ok");
    e.recalculate().expect("ok");
    e.recalculate().expect("ok");
    assert_eq!(e.get_value("S", 0, 1), Some(Value::Number(20.0)));
}

#[test]
fn volatile_cells_reevaluate_on_incremental_passes() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "=TODAY()").expect("set");
    e.set_cell("S", 0, 1, "=NOW()+0").expect("set");
    e.set_cell("S", 1, 0, "1").expect("set");
    e.recalculate().expect("ok");
    let first = e.get_value("S", 0, 0);
    assert!(first.is_some());

    // An unrelated edit — volatile cells still refresh (they are never
    // "clean" across calculations).
    e.set_cell("S", 1, 0, "2").expect("set");
    e.recalculate_incremental(&[(String::from("S"), 1, 0)])
        .expect("ok");
    assert!(e.get_value("S", 0, 0).is_some());
    assert!(e.get_value("S", 0, 1).is_some());
    assert_eq!(e.get_value("S", 1, 0), Some(Value::Number(2.0)));
}

#[test]
fn cycles_created_after_a_clean_pass_are_caught_incrementally() {
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "1").expect("set"); // A1
    e.set_cell("S", 0, 1, "=A1+1").expect("set"); // B1
    e.recalculate().expect("clean pass");
    assert_eq!(e.get_value("S", 0, 1), Some(Value::Number(2.0)));

    // B1 now reads itself.
    e.set_cell("S", 0, 1, "=B1+1").expect("set");
    let err = e
        .recalculate_incremental(&[(String::from("S"), 0, 1)])
        .expect_err("circular");
    assert!(matches!(err, EngineError::CircularReference(_)));
    // The cycle cell never computed.
    assert_eq!(e.get_value("S", 0, 1), None);
}

#[test]
fn replacing_a_cross_sheet_formula_retires_its_cross_edges() {
    let mut e = Engine::new();
    e.set_cell("Data", 0, 0, "5").expect("set");
    e.set_cell("S", 0, 0, "=Data!A1*2").expect("set");
    e.set_cell("S", 1, 0, "=Data!A1*3").expect("set");
    e.recalculate().expect("ok");
    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(10.0)));
    assert_eq!(e.get_value("S", 1, 0), Some(Value::Number(15.0)));

    // The first reader goes literal: the second still tracks Data!A1.
    e.set_cell("S", 0, 0, "999").expect("set");
    e.recalculate_incremental(&[]).expect("ok");
    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(999.0)));
    e.set_cell("Data", 0, 0, "100").expect("set");
    e.recalculate_incremental(&[]).expect("ok");
    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(999.0)));
    assert_eq!(e.get_value("S", 1, 0), Some(Value::Number(300.0)));

    // The last reader goes literal: the reverse edge table empties out.
    e.set_cell("S", 1, 0, "1").expect("set");
    e.set_cell("Data", 0, 0, "7").expect("set");
    e.recalculate_incremental(&[]).expect("ok");
    assert_eq!(e.get_value("S", 0, 0), Some(Value::Number(999.0)));
    assert_eq!(e.get_value("S", 1, 0), Some(Value::Number(1.0)));
}

#[test]
fn every_error_literal_is_stored_as_a_value() {
    let mut e = Engine::new();
    let literals = [
        ("#DIV/0!", ExcelError::DivZero),
        ("#N/A", ExcelError::NA),
        ("#NAME?", ExcelError::Name),
        ("#NULL!", ExcelError::Null),
        ("#NUM!", ExcelError::Num),
        ("#REF!", ExcelError::Ref),
        ("#VALUE!", ExcelError::Value),
    ];
    for (i, (lit, expected)) in literals.iter().enumerate() {
        e.set_cell("S", i as u32, 0, lit).expect("set");
        assert_eq!(
            e.get_value("S", i as u32, 0),
            Some(Value::Error(*expected)),
            "literal {lit}"
        );
    }
}

#[test]
fn error_display_and_sources_are_wired() {
    // Formula parse failure chains to formula-lang's error.
    let mut e = Engine::new();
    let err = e.set_cell("S", 0, 0, "=SUM(").expect_err("parse error");
    assert!(err.to_string().starts_with("formula error:"));
    assert!(std::error::Error::source(&err).is_some());

    let dup = EngineError::DuplicateSheet(String::from("X"));
    assert_eq!(dup.to_string(), "duplicate sheet name \"X\"");
    assert!(std::error::Error::source(&dup).is_none());

    let circ =
        EngineError::CircularReference(vec![(String::from("S"), 0, 0), (String::from("S"), 0, 1)]);
    assert_eq!(
        circ.to_string(),
        "circular reference involving S!r0c0, S!r0c1"
    );
    assert!(std::error::Error::source(&circ).is_none());

    let exhausted = EngineError::StubSpaceExhausted;
    assert!(exhausted.to_string().contains("XFD1048576"));

    let xlsx = EngineError::Xlsx(sheet_xlsx::XlsxError::MissingPart {
        part: String::from("xl/workbook.xml"),
    });
    assert!(xlsx.to_string().starts_with("xlsx error:"));
}
