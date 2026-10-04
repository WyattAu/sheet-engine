//! The engine's error taxonomy — one error per failure domain, plus the
//! circular-reference report.
//!
//! Deliberately exhaustive (no `#[non_exhaustive]`): callers match every
//! variant, and a new variant is a semver-minor event. The three wrapped
//! error types are the three dependencies' own — no information is
//! re-wrapped away.

use crate::CellId;
use core::fmt;
use formula_lang::FormulaError;
use sheet_xlsx::XlsxError;

/// Everything that can fail while driving the calculation engine.
///
/// Parsing, dependency extraction, and XLSX coding delegate to their
/// crates' own errors. The engine adds exactly two of its own: a rejected
/// workbook (duplicate sheet names on load) and the circular-reference
/// report.
#[derive(Debug)]
pub enum EngineError {
    /// The formula text did not tokenize or parse
    /// (`=SUM(`, `=Nope(1)`, `=A1048577`).
    Formula(FormulaError),
    /// The formula's references could not be modeled as dependency edges —
    /// most commonly [`sheet_core::GraphError::RangeTooLarge`] for a range
    /// wider than `sheet_core::refs::RANGE_CELL_CAP` cells.
    Graph(sheet_core::GraphError),
    /// The XLSX codec failed to read or write a workbook.
    Xlsx(XlsxError),
    /// The workbook contains circular references. The payload lists every
    /// cell that sits on a cycle (members of multi-cell cycles, plus
    /// self-references), sorted for determinism. Everything *not* on a
    /// cycle was still evaluated before this error was raised — Excel
    /// semantics: the rest of the workbook computes.
    CircularReference(Vec<CellId>),
    /// `from_xlsx` met two `<sheet>` elements with the same name. The
    /// engine keys sheets by name; it refuses to silently merge them.
    DuplicateSheet(String),
    /// The formula references the very last grid cell *and* a
    /// sheet-qualified reference: no stub coordinate space remains above
    /// its local references for the cross-sheet rewrite. Rare to the
    /// point of pathology — `XFD1048576` plus a cross-sheet read.
    StubSpaceExhausted,
}

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Formula(e) => write!(f, "formula error: {e}"),
            Self::Graph(e) => write!(f, "dependency graph error: {e}"),
            Self::Xlsx(e) => write!(f, "xlsx error: {e}"),
            Self::CircularReference(cells) => {
                write!(f, "circular reference involving ")?;
                for (i, (sheet, row, col)) in cells.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{sheet}!r{row}c{col}")?;
                }
                Ok(())
            }
            Self::DuplicateSheet(name) => write!(f, "duplicate sheet name {name:?}"),
            Self::StubSpaceExhausted => write!(
                f,
                "formula mixes a reference to XFD1048576 with a sheet-qualified \
                 reference; no stub coordinate space remains for the rewrite"
            ),
        }
    }
}

impl std::error::Error for EngineError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Formula(e) => Some(e),
            Self::Graph(e) => Some(e),
            Self::Xlsx(e) => Some(e),
            Self::CircularReference(_) | Self::DuplicateSheet(_) | Self::StubSpaceExhausted => None,
        }
    }
}

impl From<FormulaError> for EngineError {
    fn from(e: FormulaError) -> Self {
        Self::Formula(e)
    }
}

impl From<sheet_core::GraphError> for EngineError {
    fn from(e: sheet_core::GraphError) -> Self {
        Self::Graph(e)
    }
}

impl From<XlsxError> for EngineError {
    fn from(e: XlsxError) -> Self {
        Self::Xlsx(e)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

    use super::EngineError;

    #[test]
    fn graph_errors_display_and_chain() {
        let e = EngineError::Graph(sheet_core::GraphError::EmptyFormula);
        assert!(e.to_string().starts_with("dependency graph error:"));
        let source = std::error::Error::source(&e).expect("chains");
        assert!(source.to_string().contains("formula"));
    }

    #[test]
    fn xlsx_errors_display_and_chain() {
        let e = EngineError::Xlsx(sheet_xlsx::XlsxError::MissingPart {
            part: String::from("p"),
        });
        assert!(e.to_string().starts_with("xlsx error:"));
        assert!(std::error::Error::source(&e).is_some());
    }

    #[test]
    fn from_impls_wrap_without_loss() {
        let f: EngineError = formula_lang::FormulaError::InvalidRange.into();
        assert!(matches!(f, EngineError::Formula(_)));
        let g: EngineError = sheet_core::GraphError::EmptyFormula.into();
        assert!(matches!(g, EngineError::Graph(_)));
    }
}
