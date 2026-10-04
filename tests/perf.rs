#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing
)]
//! Spec tests 8 and 11 — performance smoke: VLOOKUP over a 1000-row range
//! under 100 ms, and incremental recalculation matching a full recalc
//! while being faster on large sheets.

use formula_lang::Value;
use sheet_engine::Engine;
use std::time::{Duration, Instant};

/// `cols` literals on row 0, one `=X1*2` formula under each.
fn grid(cols: u32) -> Engine {
    let mut e = Engine::new();
    for c in 0..cols {
        let name = col_name(c);
        e.set_cell("S", 0, c, &(c + 1).to_string()).expect("set");
        e.set_cell("S", 1, c, &format!("={name}1*2")).expect("set");
    }
    e
}

/// 0-based column index → `A1` letters (same base-26 rule as Excel).
fn col_name(mut col: u32) -> String {
    let mut s = Vec::new();
    loop {
        s.push(b'A' + u8::try_from(col % 26).expect("ascii"));
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    s.reverse();
    String::from_utf8(s).expect("ascii")
}

#[test]
fn vlookup_over_1000_rows_completes_well_under_100ms() {
    let mut e = Engine::new();
    for r in 0..1000u32 {
        e.set_cell("S", r, 0, &format!("key{r}")).expect("set");
        e.set_cell("S", r, 1, &(r * 3).to_string()).expect("set");
    }
    e.set_cell("S", 0, 3, "=VLOOKUP(\"key500\",A1:B1000,2,FALSE)")
        .expect("set");
    e.set_cell("S", 1, 3, "=SUM(B1:B1000)").expect("set");

    let start = Instant::now();
    e.recalculate().expect("recalculates");
    let elapsed = start.elapsed();

    assert_eq!(e.get_value("S", 0, 3), Some(Value::Number(1500.0)));
    assert_eq!(e.get_value("S", 1, 3), Some(Value::Number(1_498_500.0)));
    assert!(
        elapsed < Duration::from_millis(100),
        "VLOOKUP sheet took {elapsed:?} (limit 100ms)"
    );
}

#[test]
fn incremental_matches_full_and_beats_it_on_large_sheets() {
    let cols = 5000u32;
    let mut e = grid(cols);
    e.recalculate().expect("recalculates");
    let full_values: Vec<Option<Value>> = (0..cols).map(|c| e.get_value("S", 1, c)).collect();

    // One literal edit — the incremental sweet spot.
    e.set_cell("S", 0, 7, "1000").expect("set");

    let start = Instant::now();
    e.recalculate_incremental(&[(String::from("S"), 0, 7)])
        .expect("recalculates");
    let incremental = start.elapsed();

    // Same results a full recalculation would produce.
    let mut reference = grid(cols);
    reference.set_cell("S", 0, 7, "1000").expect("set");
    let full_start = Instant::now();
    reference.recalculate().expect("recalculates");
    let full = full_start.elapsed();

    for c in 0..cols {
        let expected = if c == 7 {
            Some(Value::Number(2000.0))
        } else {
            full_values[c as usize].clone()
        };
        assert_eq!(e.get_value("S", 1, c), expected, "column {c}");
    }

    assert!(
        incremental < full,
        "incremental ({incremental:?}) must beat full ({full:?}) on {cols} cells"
    );
    // And the win should be decisive, not timer noise: one edit out of
    // 5000 formulas leaves ~99.98% of the work undone.
    assert!(
        full > Duration::from_micros(200),
        "full recalc unexpectedly fast — benchmark shape broken: {full:?}"
    );
}

#[test]
fn editing_a_chain_root_costs_the_chain_but_only_the_chain() {
    // 2000 formulas in one column, each reading the one above: an edit at
    // the root re-evaluates all of them; a full recalc of the same sheet
    // (with an independent second column of 2000) costs twice as much.
    let rows = 2000u32;
    let mut e = Engine::new();
    e.set_cell("S", 0, 0, "1").expect("set");
    e.set_cell("S", 0, 1, "1").expect("set");
    for r in 1..rows {
        e.set_cell("S", r, 0, &format!("=A{r}+1")).expect("set");
        e.set_cell("S", r, 1, &format!("=B{r}+1")).expect("set");
    }
    e.recalculate().expect("ok");

    e.set_cell("S", 0, 0, "100").expect("set");
    let start = Instant::now();
    e.recalculate_incremental(&[]).expect("ok");
    let touched_chain = start.elapsed();
    assert_eq!(
        e.get_value("S", rows - 1, 0),
        Some(Value::Number(100.0 + f64::from(rows) - 1.0))
    );

    // Same edit but against a full recalculation.
    let mut f = Engine::new();
    f.set_cell("S", 0, 0, "100").expect("set");
    f.set_cell("S", 0, 1, "1").expect("set");
    for r in 1..rows {
        f.set_cell("S", r, 0, &format!("=A{r}+1")).expect("set");
        f.set_cell("S", r, 1, &format!("=B{r}+1")).expect("set");
    }
    let full_start = Instant::now();
    f.recalculate().expect("ok");
    let full = full_start.elapsed();

    assert!(
        touched_chain < full,
        "half-dirty incremental ({touched_chain:?}) must beat full ({full:?})"
    );
}
