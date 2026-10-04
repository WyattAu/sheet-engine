//! Workbook-level dependency algorithms — the sheet dimension `sheet-core`'s
//! single-sheet graph leaves to this crate.
//!
//! Three questions, all answered over the *union* of per-sheet local edges
//! (owned by each sheet's `sheet_core::DependencyGraph`) and the engine's
//! cross-sheet edges:
//!
//! - `topological_order` — a total, deterministic evaluation order over
//!   every formula cell in the workbook (Kahn's algorithm with a sorted
//!   frontier; ties break on `(sheet, row, col)`).
//! - `dirty_closure` — the transitive dependents of an edit set, the
//!   minimal recompute set for incremental recalculation.
//! - `circular_members` — the exact cycle membership among the cells
//!   Kahn's algorithm left unordered (Kosaraju's algorithm; a cell counts
//!   when it sits on a multi-cell cycle or references itself).
//!
//! All functions are pure walks over engine state: no mutation, no
//! recursion (explicit stacks — formula chains are unbounded), and
//! deterministic output for deterministic input.

use crate::CellId;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::vec::Vec;

/// The workbook-level precedents of `node`: every formula-modeling cell it
/// reads, local (through the sheet's graph) and cross-sheet (through the
/// engine's qualified-edge table) together.
fn precedents(engine: &crate::Engine, node: &CellId) -> Vec<CellId> {
    let (sheet, row, col) = node;
    let mut out: Vec<CellId> = engine
        .graph_for(sheet)
        .map(|g| {
            g.precedents((*row, *col))
                .into_iter()
                .map(|(r, c)| (sheet.clone(), r, c))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(xs) = engine.cross_precedents(node) {
        out.extend(xs.iter().cloned());
    }
    out
}

/// The workbook-level dependents of `node`: formula cells that must
/// recompute when `node` changes.
pub(crate) fn dependents(engine: &crate::Engine, node: &CellId) -> Vec<CellId> {
    let (sheet, row, col) = node;
    let mut out: Vec<CellId> = engine
        .graph_for(sheet)
        .map(|g| {
            g.dependents((*row, *col))
                .into_iter()
                .map(|(r, c)| (sheet.clone(), r, c))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if let Some(xs) = engine.cross_dependents(node) {
        out.extend(xs.iter().cloned());
    }
    out
}

/// Every formula cell of the workbook, in `(sheet, row, col)` order.
pub(crate) fn formula_cells(engine: &crate::Engine) -> BTreeSet<CellId> {
    engine.formula_nodes()
}

/// Deterministic topological order over every formula cell: each formula
/// appears after every formula it reads. Cells on — or downstream of — a
/// cycle are omitted; see [`circular_members`].
///
/// The engine caches this (edges change only when formulas are registered
/// or retired); the cache lives in [`crate::Engine`] and this function is
/// the cold path.
pub(crate) fn topological_order(engine: &crate::Engine) -> Vec<CellId> {
    let nodes = formula_cells(engine);
    let mut indegree: BTreeMap<&CellId, usize> = nodes.iter().map(|n| (n, 0)).collect();
    // precedent → dependents, restricted to formula cells.
    let mut adj: BTreeMap<&CellId, Vec<&CellId>> = BTreeMap::new();
    for n in &nodes {
        for p in precedents(engine, n) {
            // Only edges into modeled cells count; literals and blanks are
            // constants, always "already evaluated".
            if let Some(pn) = nodes.get(&p) {
                if let Some(deg) = indegree.get_mut(n) {
                    *deg += 1;
                }
                adj.entry(pn).or_default().push(n);
            }
        }
    }
    kahn(indegree, &adj)
}

/// Topological order over a *subset* of formula cells — the work set of an
/// incremental recalculation. Edges to cells outside the set are ignored:
/// those values are clean and current, so they read as constants. Same
/// determinism contract as [`topological_order`].
pub(crate) fn topological_order_restricted(
    engine: &crate::Engine,
    work: &BTreeSet<CellId>,
) -> Vec<CellId> {
    let mut indegree: BTreeMap<&CellId, usize> = work.iter().map(|n| (n, 0)).collect();
    let mut adj: BTreeMap<&CellId, Vec<&CellId>> = BTreeMap::new();
    for n in work {
        for p in precedents(engine, n) {
            if let Some(pn) = work.get(&p) {
                if let Some(deg) = indegree.get_mut(n) {
                    *deg += 1;
                }
                adj.entry(pn).or_default().push(n);
            }
        }
    }
    kahn(indegree, &adj)
}

/// Kahn's algorithm over prepared adjacency; the ready set is sorted, so
/// pops — and therefore the total order — are deterministic in
/// `(sheet, row, col)`.
fn kahn(
    mut indegree: BTreeMap<&CellId, usize>,
    adj: &BTreeMap<&CellId, Vec<&CellId>>,
) -> Vec<CellId> {
    let mut frontier: BTreeSet<&CellId> = indegree
        .iter()
        .filter(|(_, &d)| d == 0)
        .map(|(&n, _)| n)
        .collect();
    let mut order: Vec<CellId> = Vec::with_capacity(indegree.len());
    while let Some(n) = frontier.pop_first() {
        order.push(n.clone());
        if let Some(deps) = adj.get(n) {
            for &d in deps {
                if let Some(deg) = indegree.get_mut(d) {
                    *deg -= 1;
                    if *deg == 0 {
                        frontier.insert(d);
                    }
                }
            }
        }
    }
    order
}

/// The transitive dependents of `seeds` (the seeds included). This is the
/// minimal recompute set: a cell appears because an edit flows to it.
pub(crate) fn dirty_closure(
    engine: &crate::Engine,
    seeds: impl IntoIterator<Item = CellId>,
) -> BTreeSet<CellId> {
    let mut seen: BTreeSet<CellId> = seeds.into_iter().collect();
    let mut queue: VecDeque<CellId> = seen.iter().cloned().collect();
    while let Some(n) = queue.pop_front() {
        for d in dependents(engine, &n) {
            if seen.insert(d.clone()) {
                queue.push_back(d);
            }
        }
    }
    seen
}

/// The exact circular-reference membership among `leftover` — the cells
/// topological ordering could not place. Kosaraju's algorithm over the
/// subgraph the leftover induces: a cell is reported when it belongs to a
/// multi-cell strongly-connected component or references itself. Output is
/// sorted for determinism.
pub(crate) fn circular_members(engine: &crate::Engine, leftover: &[CellId]) -> Vec<CellId> {
    if leftover.is_empty() {
        return Vec::new();
    }
    let set: BTreeSet<&CellId> = leftover.iter().collect();
    // Forward and reversed adjacency, restricted to the leftover.
    let mut forward: BTreeMap<&CellId, Vec<&CellId>> = BTreeMap::new();
    let mut reverse: BTreeMap<&CellId, Vec<&CellId>> = BTreeMap::new();
    for n in leftover {
        for p in precedents(engine, n) {
            if let Some(pn) = set.get(&p) {
                forward.entry(n).or_default().push(*pn);
                reverse.entry(*pn).or_default().push(n);
            }
        }
    }

    // Pass 1: finish-time order via an explicit-stack DFS.
    let mut finished: Vec<&CellId> = Vec::with_capacity(leftover.len());
    let mut state: BTreeMap<&CellId, u8> = BTreeMap::new(); // 0/absent = unvisited, 1 = on stack, 2 = done
    for start in leftover {
        if state.get(start).copied().unwrap_or(0) != 0 {
            continue;
        }
        let mut stack: Vec<(&CellId, usize)> = vec![(start, 0)];
        state.insert(start, 1);
        while let Some(top) = stack.last_mut() {
            let (n, idx) = *top;
            let next = forward.get(n).and_then(|ns| ns.get(idx)).copied();
            if let Some(c) = next {
                top.1 += 1;
                if state.get(c).copied().unwrap_or(0) == 0 {
                    state.insert(c, 1);
                    stack.push((c, 0));
                }
            } else {
                state.insert(n, 2);
                finished.push(n);
                stack.pop();
            }
        }
    }

    // Pass 2: reversed-graph DFS in reverse finish order → SCCs.
    let mut assigned: BTreeMap<&CellId, usize> = BTreeMap::new();
    let mut components: Vec<Vec<&CellId>> = Vec::new();
    for &n in finished.iter().rev() {
        if assigned.contains_key(n) {
            continue;
        }
        let id = components.len();
        let mut members: Vec<&CellId> = Vec::new();
        let mut queue: VecDeque<&CellId> = VecDeque::from([n]);
        assigned.insert(n, id);
        while let Some(c) = queue.pop_front() {
            members.push(c);
            for d in reverse.get(c).into_iter().flatten() {
                if !assigned.contains_key(d) {
                    assigned.insert(d, id);
                    queue.push_back(*d);
                }
            }
        }
        components.push(members);
    }

    // A component is a cycle when it has several members or a self-edge.
    let mut cycle_cells: BTreeSet<CellId> = BTreeSet::new();
    for members in &components {
        let single = members.first();
        let self_edge = members.len() == 1
            && single.is_some_and(|n| forward.get(n).is_some_and(|ns| ns.contains(n)));
        if members.len() > 1 || self_edge {
            cycle_cells.extend(members.iter().map(|n| (*n).clone()));
        }
    }
    cycle_cells.into_iter().collect()
}

/// Renders a cell as `Sheet!A1` (1-based) for error messages and tests.
#[cfg(test)]
pub(crate) fn a1(node: &CellId) -> String {
    let (sheet, row, col) = node;
    format!(
        "{sheet}!{}{}",
        formula_lang::ast::col_name(col + 1),
        row + 1
    )
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::indexing_slicing
    )]

    use super::*;
    use crate::Engine;

    fn n(sheet: &str, row: u32, col: u32) -> CellId {
        (sheet.to_string(), row, col)
    }

    #[test]
    fn topo_orders_a_cross_sheet_chain() {
        let mut e = Engine::new();
        e.set_cell("S1", 0, 0, "1").unwrap();
        e.set_cell("S2", 0, 0, "=S1!A1+1").unwrap();
        e.set_cell("S1", 0, 1, "=S2!A1+1").unwrap();
        let order = topological_order(&e);
        let a1s: Vec<String> = order.iter().map(a1).collect();
        assert_eq!(a1s, vec!["S2!A1", "S1!B1"]);
    }

    #[test]
    fn closure_flows_across_sheets() {
        let mut e = Engine::new();
        e.set_cell("S1", 0, 0, "1").unwrap();
        e.set_cell("S2", 0, 0, "=S1!A1+1").unwrap();
        e.set_cell("S2", 0, 1, "=S2!A1+1").unwrap();
        let dirty = dirty_closure(&e, [n("S1", 0, 0)]);
        let a1s: Vec<String> = dirty.iter().map(a1).collect();
        assert_eq!(a1s, vec!["S1!A1", "S2!A1", "S2!B1"]);
    }

    #[test]
    fn cycles_are_reported_exactly_not_downstream() {
        // A1 ↔ B1, C1 = A1 + 1 (downstream of the cycle, not in it).
        let mut e = Engine::new();
        e.set_cell("S", 0, 0, "=B1+1").unwrap();
        e.set_cell("S", 0, 1, "=A1+1").unwrap();
        e.set_cell("S", 0, 2, "=A1+1").unwrap();
        let order = topological_order(&e);
        assert_eq!(order, Vec::<CellId>::new());
        let left = formula_cells(&e)
            .into_iter()
            .filter(|n| !order.contains(n))
            .collect::<Vec<_>>();
        let cyc = circular_members(&e, &left);
        let a1s: Vec<String> = cyc.iter().map(a1).collect();
        assert_eq!(a1s, vec!["S!A1", "S!B1"]);
    }

    #[test]
    fn cross_sheet_cycles_are_found() {
        let mut e = Engine::new();
        e.set_cell("S1", 0, 0, "=S2!A1+1").unwrap();
        e.set_cell("S2", 0, 0, "=S1!A1+1").unwrap();
        let order = topological_order(&e);
        assert_eq!(order, Vec::<CellId>::new());
        let left: Vec<CellId> = formula_cells(&e).into_iter().collect();
        let cyc = circular_members(&e, &left);
        assert_eq!(cyc.len(), 2);
    }

    #[test]
    fn a1_notation_is_one_based() {
        assert_eq!(a1(&n("S", 0, 0)), "S!A1");
        assert_eq!(a1(&n("S", 1, 27)), "S!AB2");
    }
}
