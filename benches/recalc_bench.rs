//! Full vs. incremental recalculation — the engine's headline benchmark.
//!
//! Three shapes, each with a large formula population:
//!
//! - `chain` — one formula per row, each reading the row above (a
//!   dependency chain: incremental must re-evaluate the whole chain when
//!   the root changes, and almost nothing when a leaf does);
//! - `grid` — `N` independent formulas over a literal block (the
//!   incremental sweet spot: one edit dirties one formula);
//! - `vlookup` — a lookup formula over a 1000-row table.

#![allow(missing_docs, clippy::unwrap_used, clippy::expect_used)]

use criterion::{criterion_group, criterion_main, Criterion};
use sheet_engine::{Engine, Value};

/// A `rows`-cell chain: A1 literal, A2..A{rows} each `=A{r}+1`.
fn chain_engine(rows: u32) -> Engine {
    let mut e = Engine::new();
    let _ = e.set_cell("Sheet1", 0, 0, "1");
    for r in 1..rows {
        let _ = e.set_cell("Sheet1", r, 0, &format!("=A{r}+1"));
    }
    let _ = e.recalculate();
    e
}

/// A literal row plus one formula per column over it.
fn grid_engine(cols: u32) -> Engine {
    let mut e = Engine::new();
    for c in 0..cols {
        let _ = e.set_cell("Sheet1", 0, c, &(c + 1).to_string());
        let _ = e.set_cell("Sheet1", 1, c, &format!("={}1*2", col_name(c)));
    }
    let _ = e.recalculate();
    e
}

/// 0-based column index → `A1` letters (Excel's base-26 rule).
fn col_name(mut col: u32) -> String {
    let mut letters = Vec::new();
    loop {
        letters.push(b'A' + u8::try_from(col % 26).expect("in range"));
        if col < 26 {
            break;
        }
        col = col / 26 - 1;
    }
    letters.reverse();
    String::from_utf8(letters).expect("ascii")
}

/// A 1000-row two-column lookup table plus one VLOOKUP over it.
fn vlookup_engine() -> Engine {
    let mut e = Engine::new();
    for r in 0..1000u32 {
        let _ = e.set_cell("Sheet1", r, 0, &(r + 1).to_string());
        let _ = e.set_cell("Sheet1", r, 1, &(r * 10).to_string());
    }
    let _ = e.set_cell("Sheet1", 0, 3, "=VLOOKUP(500,A1:B1000,2,FALSE)");
    let _ = e.recalculate();
    e
}

fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("recalc");

    group.bench_function("full/grid-5000", |b| {
        let mut e = grid_engine(5000);
        b.iter(|| {
            let _ = e.recalculate();
            std::hint::black_box(());
        });
    });

    group.bench_function("incremental/grid-5000-one-edit", |b| {
        let mut e = grid_engine(5000);
        b.iter(|| {
            let _ = e.set_cell("Sheet1", 0, 7, "123");
            let _ = e.recalculate_incremental(&[]);
            std::hint::black_box(());
        });
    });

    group.bench_function("full/chain-500", |b| {
        let mut e = chain_engine(500);
        b.iter(|| {
            let _ = e.recalculate();
        });
    });

    group.bench_function("incremental/chain-500-leaf-edit", |b| {
        let mut e = chain_engine(500);
        b.iter(|| {
            let _ = e.set_cell("Sheet1", 499, 0, "5");
            let _ = e.recalculate_incremental(&[]);
        });
    });

    group.bench_function("vlookup-1000", |b| {
        let mut e = vlookup_engine();
        b.iter(|| {
            let _ = e.recalculate();
            std::hint::black_box(e.get_value("Sheet1", 0, 3) == Some(Value::Number(4990.0)));
        });
    });

    group.finish();
}

criterion_group!(benches, bench);
criterion_main!(benches);
