//! Sheet-qualified reference rewriting — how `=Sheet2!A1` evaluates without
//! the evaluator knowing sheets exist.
//!
//! `formula-lang`'s grammar has no `!` (a qualified reference is not even
//! tokenizable), and its [`formula_lang::CellResolver`] resolves bare
//! `(col, row)` pairs in one namespace. Re-implementing the evaluator to
//! add a sheet dimension would abandon every Excel-compatible function, so
//! the engine instead **rewrites** each qualified reference into a *stub*:
//!
//! ```text
//! =Sheet2!A1 + B2      becomes      =A1048577 + B2      (stub shown illustratively)
//!      └ recorded: stub → (Sheet2, A1)
//! ```
//!
//! A stub is a plain in-grid cell reference guaranteed to collide with no
//! real reference *of the same formula*: stub linear coordinates start one
//! past the highest linear coordinate the formula's local references
//! (`B2` above) reach. The mapping is kept per formula in `Prepared`;
//! the engine's resolver translates stub reads back to their real
//! `(sheet, row, col)` targets during evaluation. The original text is
//! never altered in the sheet — rewriting happens only in the evaluation
//! path, so XLSX round-trips carry the formula exactly as written.
//!
//! The scanner is deliberately lenient in the same spirit as
//! `sheet_core::refs`: string literals, error literals (`#REF!` contains a
//! `!`), and function names (`LOG10(`) are skipped; anything unrecognized
//! is copied through verbatim, which lets `formula_lang::parse` produce
//! its typed error for genuinely malformed input. The scanner itself
//! cannot fail.

use formula_lang::ast::{col_name, col_number, CellRef, Expr, MAX_COL, MAX_ROW};
use std::string::{String, ToString};
use std::vec::Vec;

/// Columns in the legal grid — one stride of the linear coordinate space.
const GRID_W: u64 = MAX_COL as u64;

/// Linearizes a 1-based `(col, row)` into `row * 16384 + col` (0-based).
/// The legal grid (`MAX_COL × MAX_ROW`) maps onto `0 .. 2^34`, so any
/// linear coordinate fits comfortably in [`u64`].
fn lin(col: u32, row: u32) -> u64 {
    (u64::from(row) - 1) * GRID_W + u64::from(col) - 1
}

/// Renders a linear coordinate back to `A1` notation (no `$` anchors).
fn render_lin(l: u64) -> String {
    let col = u32::try_from(l % GRID_W).unwrap_or(0) + 1;
    let row = u32::try_from(l / GRID_W).unwrap_or(0) + 1;
    let mut s = col_name(col);
    s.push_str(&row.to_string());
    s
}

/// A cross-sheet target rectangle: sheet name plus normalized 1-based
/// inclusive corners.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CrossRef {
    /// The sheet the reference reads, exactly as written in the qualifier.
    pub sheet: String,
    /// 1-based inclusive row range (`(r, r)` for a single row).
    pub rows: (u32, u32),
    /// 1-based inclusive column range.
    pub cols: (u32, u32),
}

/// One walked span of formula text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Piece {
    /// Copied through unchanged (operators, whitespace, literals, function
    /// names, anything unrecognized).
    Text(String),
    /// A *local* reference or range, kept verbatim; `far` is the highest
    /// linear coordinate it reaches (stub allocation starts above it).
    Local(String, u64),
    /// A sheet-qualified reference or range, to be replaced by a stub.
    Qualified(CrossRef),
}

/// Copies `cs[i..end]` into `buf` — no slicing, bounds tolerated.
fn push_range(cs: &[char], i: usize, end: usize, buf: &mut String) {
    buf.extend(cs.iter().skip(i).take(end.saturating_sub(i)));
}

/// The end of a double-quoted string literal starting at `i` (the opening
/// quote), with `""` as the escaped quote. An unterminated literal runs to
/// the end of the text (the parser will reject it with a typed error).
fn string_end(cs: &[char], i: usize) -> usize {
    let mut j = i + 1;
    while let Some(&c) = cs.get(j) {
        if c == '"' {
            if matches!(cs.get(j + 1), Some('"')) {
                j += 2;
            } else {
                return j + 1;
            }
        } else {
            j += 1;
        }
    }
    cs.len()
}

/// The end of a single-quoted sheet name starting at `i` (the opening
/// quote), with `''` as the escaped quote. Unterminated runs to the end.
fn quoted_end(cs: &[char], i: usize) -> usize {
    let mut j = i + 1;
    while let Some(&c) = cs.get(j) {
        if c == '\'' {
            if matches!(cs.get(j + 1), Some('\'')) {
                j += 2;
            } else {
                return j + 1;
            }
        } else {
            j += 1;
        }
    }
    cs.len()
}

/// Reads the unescaped sheet name of a single-quoted qualifier. Returns
/// `(name, index_after_closing_quote)`, or `None` when unterminated.
fn quoted_name(cs: &[char], i: usize) -> Option<(String, usize)> {
    let mut j = i + 1;
    let mut name = String::new();
    while let Some(&c) = cs.get(j) {
        if c == '\'' {
            if matches!(cs.get(j + 1), Some('\'')) {
                name.push('\'');
                j += 2;
            } else {
                return Some((name, j + 1));
            }
        } else {
            name.push(c);
            j += 1;
        }
    }
    None
}

/// Length of the error literal starting at `i` (the `#`), longest match
/// first; `1` when no literal matches (the `#` copies through verbatim).
fn error_literal_len(cs: &[char], i: usize) -> usize {
    const CANDIDATES: [&str; 7] = [
        "#DIV/0!", "#VALUE!", "#NAME?", "#NULL!", "#NUM!", "#REF!", "#N/A",
    ];
    let head: String = cs.iter().skip(i).take(CANDIDATES[0].len()).collect();
    let head = head.to_ascii_uppercase();
    CANDIDATES
        .into_iter()
        .find(|lit| head.starts_with(lit))
        .map_or(1, str::len)
}

/// The end of a bare identifier run: `[A-Za-z0-9_.]` — sheet names and
/// function names alike.
fn ident_end(cs: &[char], i: usize) -> usize {
    let mut j = i;
    while matches!(cs.get(j), Some(c) if c.is_ascii_alphanumeric() || *c == '_' || *c == '.') {
        j += 1;
    }
    j
}

/// Parses one `$`-tolerant cell reference at `i`. Returns its end and its
/// 1-based `(col, row)`. A match directly followed by `(` is rejected —
/// that is a function name shaped like a reference (`LOG10(`).
fn try_single_ref(cs: &[char], i: usize) -> Option<(usize, u32, u32)> {
    let mut j = i;
    if matches!(cs.get(j), Some('$')) {
        j += 1;
    }
    let ls = j;
    while matches!(cs.get(j), Some(c) if c.is_ascii_alphabetic()) {
        j += 1;
    }
    if j == ls {
        return None;
    }
    // In-grid columns are at most three letters (`XFD`); longer runs are
    // sheet names or identifiers. (They also overflow `col_number`'s
    // base-26 accumulation, which is not overflow-checked.)
    if j - ls > 3 {
        return None;
    }
    let letters: String = cs.iter().skip(ls).take(j - ls).collect();
    let col = col_number(&letters)?;
    if col == 0 || col > MAX_COL {
        return None;
    }
    if matches!(cs.get(j), Some('$')) {
        j += 1;
    }
    let ds = j;
    while matches!(cs.get(j), Some(c) if c.is_ascii_digit()) {
        j += 1;
    }
    if j == ds {
        return None;
    }
    let digits: String = cs.iter().skip(ds).take(j - ds).collect();
    let row: u32 = digits.parse().ok()?;
    if row == 0 || row > MAX_ROW {
        return None;
    }
    // `A1B2` is two references jammed together — not a reference.
    if matches!(cs.get(j), Some(c) if c.is_ascii_alphanumeric() || *c == '_' || *c == '.') {
        return None;
    }
    // `LOG10(` is a function call, never a reference.
    if matches!(cs.get(j), Some('(')) {
        return None;
    }
    Some((j, col, row))
}

/// Parses a reference or `ref:ref` range at `i`. Returns the end and the
/// highest linear coordinate the span reaches.
fn try_ref_range(cs: &[char], i: usize) -> Option<(usize, u64)> {
    let (mut end, col1, row1) = try_single_ref(cs, i)?;
    let mut far = lin(col1, row1);
    if matches!(cs.get(end), Some(':')) {
        if let Some((end2, col2, row2)) = try_single_ref(cs, end + 1) {
            far = far.max(lin(col2, row2));
            end = end2;
        }
    }
    Some((end, far))
}

/// Parses the target after a `!` qualifier: a reference or normalized
/// range. `sheet` is filled by the caller.
fn read_target(cs: &[char], i: usize) -> Option<(CrossRef, usize)> {
    let (after_first, col_a, row_a) = try_single_ref(cs, i)?;
    let (cols, rows, end) = if matches!(cs.get(after_first), Some(':')) {
        match try_single_ref(cs, after_first + 1) {
            Some((after_second, col_b, row_b)) => (
                (col_a.min(col_b), col_a.max(col_b)),
                (row_a.min(row_b), row_a.max(row_b)),
                after_second,
            ),
            None => ((col_a, col_a), (row_a, row_a), after_first),
        }
    } else {
        ((col_a, col_a), (row_a, row_a), after_first)
    };
    Some((
        CrossRef {
            sheet: String::new(),
            rows,
            cols,
        },
        end,
    ))
}

/// Walks formula text once, splitting it into verbatim, local-reference,
/// and sheet-qualified spans. Infallible: unrecognized input copies
/// through, and `formula_lang::parse` judges the result.
pub(crate) fn walk(src: &str) -> Vec<Piece> {
    let cs: Vec<char> = src.chars().collect();
    let mut pieces: Vec<Piece> = Vec::new();
    let mut buf = String::new();
    let mut i = 0;

    while let Some(&c) = cs.get(i) {
        match c {
            '"' => {
                let end = string_end(&cs, i);
                push_range(&cs, i, end, &mut buf);
                i = end;
            }
            '\'' => {
                let mut handled = false;
                if let Some((name, after)) = quoted_name(&cs, i) {
                    if matches!(cs.get(after), Some('!')) {
                        if let Some((mut target, end)) = read_target(&cs, after + 1) {
                            target.sheet = name;
                            pieces.push(Piece::Text(core::mem::take(&mut buf)));
                            pieces.push(Piece::Qualified(target));
                            i = end;
                            handled = true;
                        }
                    }
                }
                if !handled {
                    let end = quoted_end(&cs, i);
                    push_range(&cs, i, end, &mut buf);
                    i = end;
                }
            }
            '#' => {
                let len = error_literal_len(&cs, i);
                push_range(&cs, i, i + len, &mut buf);
                i += len;
            }
            _ if c == '$' || c.is_ascii_alphabetic() => {
                if let Some((end, far)) = try_ref_range(&cs, i) {
                    // A reference-shaped span followed by `!` is an
                    // unquoted sheet name (`A1!B2` — Excel allows it when
                    // the name is not ambiguous; anchors cannot appear in
                    // sheet names and are dropped).
                    if matches!(cs.get(end), Some('!')) {
                        if let Some((mut target, tend)) = read_target(&cs, end + 1) {
                            let mut name: String = cs.iter().skip(i).take(end - i).collect();
                            name.retain(|ch| ch != '$');
                            target.sheet = name;
                            pieces.push(Piece::Text(core::mem::take(&mut buf)));
                            pieces.push(Piece::Qualified(target));
                            i = tend;
                            continue;
                        }
                    }
                    let text: String = cs.iter().skip(i).take(end - i).collect();
                    pieces.push(Piece::Text(core::mem::take(&mut buf)));
                    pieces.push(Piece::Local(text, far));
                    i = end;
                    continue;
                }
                let end = ident_end(&cs, i);
                if end == i {
                    buf.push(c);
                    i += 1;
                    continue;
                }
                let run: String = cs.iter().skip(i).take(end - i).collect();
                if matches!(cs.get(end), Some('!')) {
                    if let Some((mut target, tend)) = read_target(&cs, end + 1) {
                        target.sheet = run;
                        pieces.push(Piece::Text(core::mem::take(&mut buf)));
                        pieces.push(Piece::Qualified(target));
                        i = tend;
                        continue;
                    }
                }
                buf.push_str(&run);
                i = end;
            }
            _ => {
                buf.push(c);
                i += 1;
            }
        }
    }
    pieces.push(Piece::Text(buf));
    pieces
}

/// One rewritten qualified range: the stub rectangle (in linear
/// coordinates, top-left `start`, shape `rows × cols`) and the real
/// target rectangle's 0-based top-left `(sheet, row, col)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StubRange {
    /// Linear coordinate of the stub rectangle's top-left corner.
    pub start: u64,
    /// Stub rectangle height in rows.
    pub rows: u32,
    /// Stub rectangle width in columns.
    pub cols: u32,
    /// Real target: sheet name plus 0-based top-left corner.
    pub target: (String, u32, u32),
}

/// A formula in its evaluation-ready form: the parsed AST of the rewritten
/// text, the stub translation table, and the volatility verdict.
#[derive(Debug, Clone)]
pub(crate) struct Prepared {
    /// AST of the rewritten formula (stubs substituted for qualifiers).
    pub expr: Expr,
    /// `formula_lang::is_volatile` of the parsed formula.
    pub volatile: bool,
    /// First linear coordinate available for stubs — one past the highest
    /// linear coordinate any local reference of this formula reaches.
    pub stub_base: u64,
    /// The qualified spans, in allocation order (ascending `start`).
    pub stubs: Vec<StubRange>,
}

impl Prepared {
    /// Resolves a linear coordinate against the stub table: `None` when it
    /// is not inside any stub rectangle; otherwise the owning rectangle and
    /// the offset (in cells, row-major) into it.
    pub fn resolve_stub(&self, lin: u64) -> Option<(&StubRange, u64)> {
        let idx = self.stubs.partition_point(|s| s.start <= lin);
        let s = idx.checked_sub(1).and_then(|i| self.stubs.get(i))?;
        let off = lin - s.start;
        // Same geometry the allocator reserved: full strides plus width.
        if off < (u64::from(s.rows) - 1) * GRID_W + u64::from(s.cols) {
            Some((s, off))
        } else {
            None
        }
    }

    /// Reconstructs a local-only formula text whose scanned references are
    /// exactly this formula's *local* references — the text registered in
    /// the per-sheet `sheet_core::DependencyGraph`, whose namespace has no
    /// sheet dimension and must not see stub coordinates.
    ///
    /// `None` when the formula has no local references at all (a purely
    /// cross-sheet formula has no local edges to register).
    #[must_use]
    pub fn local_registration_text(&self) -> Option<String> {
        let mut parts: Vec<String> = Vec::new();
        collect_local(&self.expr, self.stub_base, &mut parts);
        if parts.is_empty() {
            None
        } else {
            Some(format!("={}", parts.join("+")))
        }
    }
}

/// AST walk collecting the rendered local references (linear coordinate
/// strictly below `stub_base`). Ranges register as ranges — the graph's
/// scanner re-expands them, subject to its own cell cap.
fn collect_local(expr: &Expr, stub_base: u64, parts: &mut Vec<String>) {
    match expr {
        Expr::CellRef { col, row, .. } => {
            if lin(*col, *row) < stub_base {
                parts.push(format!("{}{}", col_name(*col), row));
            }
        }
        Expr::Range { start, end } => {
            let CellRef {
                col: c0, row: r0, ..
            } = *start;
            let CellRef {
                col: c1, row: r1, ..
            } = *end;
            if lin(c0, r0) < stub_base && lin(c1, r1) < stub_base {
                parts.push(format!("{}{}:{}{}", col_name(c0), r0, col_name(c1), r1));
            }
        }
        Expr::Unary { expr, .. } => collect_local(expr, stub_base, parts),
        Expr::Binary { left, right, .. } => {
            collect_local(left, stub_base, parts);
            collect_local(right, stub_base, parts);
        }
        Expr::Function { args, .. } => {
            for arg in args {
                collect_local(arg, stub_base, parts);
            }
        }
        _ => {}
    }
}

/// Parses `text` (a formula with leading `=`) into its evaluation-ready
/// form: walked, qualified references replaced by allocated stubs, parsed.
///
/// # Errors
///
/// [`EngineError::Formula`] when the rewritten text does not parse — the
/// same error the original text would produce, since rewriting only
/// substitutes references for references. [`EngineError::StubSpaceExhausted`]
/// when the formula's local references reach the last grid cell and no
/// stub coordinate remains for a qualified rewrite.
pub(crate) fn prepare(text: &str) -> Result<Prepared, crate::EngineError> {
    let pieces = walk(text);
    let local_max = pieces.iter().fold(0u64, |m, p| match p {
        Piece::Local(_, far) => m.max(*far),
        _ => m,
    });

    let mut rewritten = String::new();
    let mut next = local_max + 1;
    let mut stubs: Vec<StubRange> = Vec::new();
    for p in &pieces {
        match p {
            Piece::Text(t) | Piece::Local(t, _) => rewritten.push_str(t),
            Piece::Qualified(t) => {
                let rows = t.rows.1 - t.rows.0 + 1;
                let cols = t.cols.1 - t.cols.0 + 1;
                let start = next;
                // The rectangle occupies `(rows-1)` full strides plus its
                // own width — the same geometry `Prepared::resolve_stub`
                // translates back.
                let span = (u64::from(rows) - 1) * GRID_W + u64::from(cols);
                let end_lin = start + span - 1;
                rewritten.push_str(&render_lin(start));
                if rows > 1 || cols > 1 {
                    rewritten.push(':');
                    rewritten.push_str(&render_lin(end_lin));
                }
                stubs.push(StubRange {
                    start,
                    rows,
                    cols,
                    target: (t.sheet.clone(), t.rows.0 - 1, t.cols.0 - 1),
                });
                next += span;
            }
        }
    }

    if next > MAX_LIN + 1 {
        return Err(crate::EngineError::StubSpaceExhausted);
    }
    let expr = formula_lang::parse(&rewritten).map_err(crate::EngineError::Formula)?;
    let volatile = formula_lang::is_volatile(&expr);
    Ok(Prepared {
        expr,
        volatile,
        stub_base: local_max + 1,
        stubs,
    })
}

/// Highest linear coordinate of the legal grid (`XFD1048576`).
const MAX_LIN: u64 = GRID_W * (MAX_ROW as u64) - 1;

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;
    use std::vec;

    fn qualified_of(text: &str) -> Vec<CrossRef> {
        walk(text)
            .into_iter()
            .filter_map(|p| match p {
                Piece::Qualified(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    fn locals_of(text: &str) -> Vec<(String, u64)> {
        walk(text)
            .into_iter()
            .filter_map(|p| match p {
                Piece::Local(t, far) => Some((t, far)),
                _ => None,
            })
            .collect()
    }

    fn plain(text: &str) -> String {
        walk(text)
            .into_iter()
            .filter_map(|p| match p {
                Piece::Text(t) | Piece::Local(t, _) => Some(t),
                Piece::Qualified(_) => None,
            })
            .collect()
    }

    #[test]
    fn plain_formulas_pass_through_unchanged() {
        assert_eq!(plain("=A1+1"), "=A1+1");
        assert_eq!(plain("= SUM( A1 : B2 ) + $C$3"), "= SUM( A1 : B2 ) + $C$3");
        assert_eq!(
            plain("=IF(A1>=2,\"yes\",\"no\")"),
            "=IF(A1>=2,\"yes\",\"no\")"
        );
        // String literals never scanned — A1 inside quotes stays text.
        assert_eq!(plain("=\"sum in A1?\"&A1"), "=\"sum in A1?\"&A1");
        assert_eq!(plain("=\"say \"\"hi\"\"\""), "=\"say \"\"hi\"\"\"");
    }

    #[test]
    fn error_literals_are_not_qualifiers() {
        // `#REF!` and `#DIV/0!` contain `!` — they must copy verbatim.
        assert_eq!(plain("=IFERROR(A1,#REF!)"), "=IFERROR(A1,#REF!)");
        assert_eq!(plain("=#DIV/0!+1"), "=#DIV/0!+1");
        assert_eq!(plain("=ISERROR(#N/A)"), "=ISERROR(#N/A)");
    }

    #[test]
    fn function_names_that_look_like_references_are_not_refs() {
        // LOG10 is letters+digits and col_number("LOG") is in-grid — the
        // `(` lookahead is what keeps it a function name.
        assert_eq!(locals_of("=LOG10(A1)"), vec![("A1".to_string(), 0)]);
        assert_eq!(plain("=LOG10(100)"), "=LOG10(100)");
        assert_eq!(qualified_of("=LOG10(100)"), Vec::<CrossRef>::new());
    }

    #[test]
    fn local_refs_record_their_far_corner() {
        assert_eq!(locals_of("=A1"), vec![("A1".to_string(), 0)]);
        // B2 → (row 1 * 16384) + col 1.
        assert_eq!(locals_of("=B2+1"), vec![("B2".to_string(), 16_385)]);
        // Range: far corner is C3.
        let far = lin(3, 3);
        assert_eq!(locals_of("=SUM(A1:C3)"), vec![("A1:C3".to_string(), far)]);
        assert_eq!(locals_of("=$A$1"), vec![("$A$1".to_string(), 0)]);
        // Jammed identifiers are not references.
        assert_eq!(locals_of("=A1B2"), Vec::new());
        // Out-of-grid coordinates are not references (the parser will
        // reject the text later, verbatim).
        assert_eq!(locals_of("=ZZZ16385"), Vec::new());
    }

    #[test]
    fn qualified_refs_are_found_in_all_spellings() {
        assert_eq!(
            qualified_of("=Sheet2!A1+1"),
            vec![CrossRef {
                sheet: "Sheet2".to_string(),
                rows: (1, 1),
                cols: (1, 1)
            }]
        );
        assert_eq!(qualified_of("=sheet_two!B2:C3")[0].sheet, "sheet_two");
        assert_eq!(qualified_of("=x.y_z9!A1")[0].sheet, "x.y_z9");
        // Quoted names, with '' escapes.
        assert_eq!(
            qualified_of("='My Sheet'!$B$2")[0].sheet,
            "My Sheet".to_string()
        );
        assert_eq!(
            qualified_of("='It''s here'!A1")[0].sheet,
            "It's here".to_string()
        );
        // A reference-shaped sheet name followed by `!`.
        assert_eq!(qualified_of("=A1!B2")[0].sheet, "A1".to_string());
        // A range target.
        let t = &qualified_of("='Q4 Data'!B2:F9")[0];
        assert_eq!(t.rows, (2, 9));
        assert_eq!(t.cols, (2, 6));
    }

    #[test]
    fn malformed_qualifiers_copy_through_for_the_parser_to_reject() {
        // Unterminated quote, dangling `!`, qualifier without a target.
        assert_eq!(plain("='Oops"), "='Oops");
        assert_eq!(plain("=Sheet2!"), "=Sheet2!");
        assert_eq!(plain("=Sheet2!1A"), "=Sheet2!1A");
    }

    #[test]
    fn prepare_rewrites_qualified_refs_to_in_grid_stubs() {
        let p = prepare("=Sheet2!A1+B2").expect("parses");
        // B2 is the highest local → linear 16385; stub starts at 16386.
        assert_eq!(p.stub_base, 16_386);
        assert_eq!(p.stubs.len(), 1);
        let s = &p.stubs[0];
        assert_eq!(s.start, 16_386);
        assert_eq!(s.target, ("Sheet2".to_string(), 0, 0));
        // The rewritten text parses and its only non-local ref is the stub.
        assert!(p.local_registration_text().is_some_and(|t| t == "=B2"));

        let q = prepare("='My Sheet'!B2:F9").expect("parses");
        assert_eq!(q.stubs.len(), 1);
        let s = &q.stubs[0];
        assert_eq!(s.rows, 8);
        assert_eq!(s.cols, 5);
        assert_eq!(s.target, ("My Sheet".to_string(), 1, 1));
        // Purely cross-sheet: no local registration text.
        assert_eq!(q.local_registration_text(), None);
    }

    #[test]
    fn stub_allocation_avoids_every_local_coordinate() {
        // A formula reaching XFD1048575: the stub lands directly above it,
        // and renders back inside the legal grid.
        let top = lin(16_384, 1_048_575);
        let p = prepare("=XFD1048575+Sheet2!A1").expect("parses");
        assert_eq!(p.stub_base, top + 1);
        assert_eq!(p.stubs[0].start, top + 1);
        let s = render_lin(p.stubs[0].start);
        assert!(formula_lang::parse(&format!("={s}")).is_ok());
    }

    #[test]
    fn the_last_grid_cell_leaves_no_stub_room() {
        let err = prepare("=XFD1048576+Sheet2!A1").expect_err("no stub space");
        assert!(matches!(err, crate::EngineError::StubSpaceExhausted));
        // The same formula without the qualifier is fine.
        assert!(prepare("=XFD1048576+A1").is_ok());
    }

    #[test]
    fn resolve_stub_translates_cells_and_ranges() {
        let p = prepare("=SUM('Data'!A1:B3)+C4").expect("parses");
        assert_eq!(p.stubs.len(), 1);
        let start = p.stubs[0].start;
        // Top-left → (Data, 0, 0), offset 0.
        let (s, off) = p.resolve_stub(start).expect("in range");
        assert_eq!(s.target.0, "Data");
        assert_eq!(off, 0);
        // Second cell of the first row → linear offset 1 (column step).
        let (_, off) = p.resolve_stub(start + 1).expect("in range");
        assert_eq!(off, 1);
        // First cell of the second row → linear offset one stride down.
        let (_, off) = p.resolve_stub(start + GRID_W).expect("in range");
        assert_eq!(off, GRID_W);
        // Outside the reserved span → None (below the rect, and one past
        // its end).
        assert!(p.resolve_stub(start - 1).is_none());
        let span = (GRID_W) * 2 + 2; // (rows-1) strides + width
        assert!(p.resolve_stub(start + span).is_none());
        // A phantom coordinate inside a stride but past the rect's width
        // is translated (it is unreachable by real references — the AST
        // only contains the rect corners — but the geometry is total).
        let (s, _) = p.resolve_stub(start + 6).expect("inside the stride");
        assert_eq!(s.target.0, "Data");
        assert!(p.resolve_stub(p.stub_base - 1).is_none());
        // Below the base, resolve never consults stubs.
        assert!(p.resolve_stub(0).is_none());
    }

    #[test]
    fn mixed_formulas_register_only_their_local_edges() {
        let p = prepare("=Sheet2!A1+B2+SUM(C1:C9)").expect("parses");
        let text = p.local_registration_text().expect("has locals");
        // Local refs only, ranges kept as ranges for the graph to expand.
        assert!(text.contains("B2"));
        assert!(text.contains("C1:C9"));
        assert!(!text.contains('!'));
    }
}
