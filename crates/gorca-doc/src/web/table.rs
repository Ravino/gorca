// SPDX-License-Identifier: GPL-3.0-or-later
//! Table navigation over the mirror (ADR-0003 4.6, M8): cell lookup, row and column steps and
//! header lookup, all from the tree with no grid copy. A move costs O(depth + cells in one row)
//! plus, for the last row, one pass over the table's row list, so it stays far below the 5 ms
//! gate at 12k cells (PF2).
//!
//! Spans are not modelled: Firefox exposes `colspan`/`rowspan` only through the `TableCell`
//! interface, which the source does not fetch yet (M8 gap TB.span). A spanning cell is one cell
//! in its own row.

use gorca_model::{DocNodeId, Role};

use super::cats::NodeFlags;
use super::doc::WebDoc;

/// How deep row groups (`thead`, `tbody`) may nest below a table.
const GROUP_DEPTH: u8 = 3;
/// Hops from a cursor node up to its cell.
const CELL_HOPS: usize = 16;

/// A move inside a table.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TableMove {
    NextRow,
    PrevRow,
    NextCol,
    PrevCol,
    FirstRow,
    LastRow,
    FirstCol,
    LastCol,
}

impl TableMove {
    /// A move that changes the row (the row header may change) rather than the column.
    pub fn is_vertical(self) -> bool {
        matches!(
            self,
            TableMove::NextRow | TableMove::PrevRow | TableMove::FirstRow | TableMove::LastRow
        )
    }
}

/// Why a move did not happen.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TableEdge {
    /// The cursor is not in a table cell.
    NotInCell,
    /// The move would leave the table.
    Edge,
}

/// A cell with the row and table around it.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct CellRef {
    pub table: DocNodeId,
    pub row: DocNodeId,
    pub cell: DocNodeId,
}

fn visible(doc: &WebDoc, id: DocNodeId) -> bool {
    doc.node(id)
        .is_some_and(|v| !v.flags.contains(NodeFlags::HIDDEN))
}

fn is_cell(doc: &WebDoc, id: DocNodeId) -> bool {
    doc.node(id).is_some_and(|v| {
        matches!(v.role, Role::Cell | Role::ColumnHeader | Role::RowHeader)
            && !v.flags.contains(NodeFlags::HIDDEN)
    })
}

fn is_row(doc: &WebDoc, id: DocNodeId) -> bool {
    doc.node(id)
        .is_some_and(|v| v.role == Role::Row && !v.flags.contains(NodeFlags::HIDDEN))
}

/// A data table (or grid): not a layout table.
fn is_table(doc: &WebDoc, id: DocNodeId) -> bool {
    doc.node(id)
        .is_some_and(|v| v.role == Role::Table && !v.flags.contains(NodeFlags::LAYOUT))
}

/// The cell holding `node` (or `node` itself), with its row and table. `None` outside a data
/// table (a layout table is not navigated).
pub fn cell_of(doc: &WebDoc, node: DocNodeId) -> Option<CellRef> {
    let mut cur = node;
    let mut cell = None;
    for _ in 0..CELL_HOPS {
        if is_cell(doc, cur) {
            cell = Some(cur);
            break;
        }
        cur = doc.parent(cur)?;
    }
    let cell = cell?;
    let row = doc.parent(cell).filter(|r| is_row(doc, *r))?;
    let mut t = doc.parent(row)?;
    for _ in 0..=GROUP_DEPTH {
        if is_table(doc, t) {
            return Some(CellRef {
                table: t,
                row,
                cell,
            });
        }
        t = doc.parent(t)?;
    }
    None
}

fn first_row_in(doc: &WebDoc, n: DocNodeId, depth: u8) -> Option<DocNodeId> {
    if is_row(doc, n) {
        return Some(n);
    }
    let v = doc.node(n)?;
    if depth >= GROUP_DEPTH
        || v.flags.contains(NodeFlags::HIDDEN)
        || matches!(v.role, Role::Cell | Role::ColumnHeader | Role::RowHeader)
    {
        return None;
    }
    doc.children(n)
        .find_map(|c| first_row_in(doc, c, depth + 1))
}

fn last_row_in(doc: &WebDoc, n: DocNodeId, depth: u8) -> Option<DocNodeId> {
    if is_row(doc, n) {
        return Some(n);
    }
    let v = doc.node(n)?;
    if depth >= GROUP_DEPTH
        || v.flags.contains(NodeFlags::HIDDEN)
        || matches!(v.role, Role::Cell | Role::ColumnHeader | Role::RowHeader)
    {
        return None;
    }
    let kids: Vec<DocNodeId> = doc.children(n).collect();
    kids.into_iter()
        .rev()
        .find_map(|c| last_row_in(doc, c, depth + 1))
}

/// The first row of the table.
pub fn first_row(doc: &WebDoc, table: DocNodeId) -> Option<DocNodeId> {
    doc.children(table).find_map(|c| first_row_in(doc, c, 1))
}

/// The last row of the table.
pub fn last_row(doc: &WebDoc, table: DocNodeId) -> Option<DocNodeId> {
    let kids: Vec<DocNodeId> = doc.children(table).collect();
    kids.into_iter().rev().find_map(|c| last_row_in(doc, c, 1))
}

/// The row after `row` in document order, through row groups.
pub fn next_row(doc: &WebDoc, table: DocNodeId, row: DocNodeId) -> Option<DocNodeId> {
    let mut n = row;
    loop {
        let mut s = doc.next_sibling(n);
        while let Some(x) = s {
            if let Some(r) = first_row_in(doc, x, 1) {
                return Some(r);
            }
            s = doc.next_sibling(x);
        }
        n = doc.parent(n)?;
        if n == table {
            return None;
        }
    }
}

/// The row before `row` in document order, through row groups.
pub fn prev_row(doc: &WebDoc, table: DocNodeId, row: DocNodeId) -> Option<DocNodeId> {
    let mut n = row;
    loop {
        let parent = doc.parent(n)?;
        let kids: Vec<DocNodeId> = doc.children(parent).collect();
        let at = kids.iter().position(|k| *k == n)?;
        for x in kids[..at].iter().rev() {
            if let Some(r) = last_row_in(doc, *x, 1) {
                return Some(r);
            }
        }
        if parent == table {
            return None;
        }
        n = parent;
    }
}

/// The visible cells of a row, in order.
pub fn cells(doc: &WebDoc, row: DocNodeId) -> Vec<DocNodeId> {
    doc.children(row).filter(|c| is_cell(doc, *c)).collect()
}

/// Zero-based column of `cell` in its row.
pub fn column_of(doc: &WebDoc, row: DocNodeId, cell: DocNodeId) -> Option<usize> {
    doc.children(row)
        .filter(|c| is_cell(doc, *c))
        .position(|c| c == cell)
}

/// The cell at `col` in `row`; a row shorter than the column has none.
pub fn cell_at(doc: &WebDoc, row: DocNodeId, col: usize) -> Option<DocNodeId> {
    doc.children(row).filter(|c| is_cell(doc, *c)).nth(col)
}

/// Where a move from `from` lands.
pub fn step(doc: &WebDoc, from: CellRef, mv: TableMove) -> Result<CellRef, TableEdge> {
    let col = column_of(doc, from.row, from.cell).ok_or(TableEdge::NotInCell)?;
    let at_row = |row: DocNodeId, col: usize| {
        cell_at(doc, row, col)
            .map(|cell| CellRef {
                table: from.table,
                row,
                cell,
            })
            .ok_or(TableEdge::Edge)
    };
    match mv {
        TableMove::NextCol => at_row(from.row, col + 1),
        TableMove::PrevCol => at_row(from.row, col.checked_sub(1).ok_or(TableEdge::Edge)?),
        TableMove::FirstCol => at_row(from.row, 0),
        TableMove::LastCol => {
            let n = cells(doc, from.row).len();
            at_row(from.row, n.checked_sub(1).ok_or(TableEdge::Edge)?)
        }
        TableMove::NextRow => at_row(
            next_row(doc, from.table, from.row).ok_or(TableEdge::Edge)?,
            col,
        ),
        TableMove::PrevRow => at_row(
            prev_row(doc, from.table, from.row).ok_or(TableEdge::Edge)?,
            col,
        ),
        TableMove::FirstRow => at_row(first_row(doc, from.table).ok_or(TableEdge::Edge)?, col),
        TableMove::LastRow => at_row(last_row(doc, from.table).ok_or(TableEdge::Edge)?, col),
    }
}

/// The column header of column `col`: the header cell of the table's first row (and the next
/// rows while they are all header cells at that column).
pub fn column_header(doc: &WebDoc, table: DocNodeId, col: usize) -> Option<DocNodeId> {
    let mut row = first_row(doc, table)?;
    let mut found = None;
    for _ in 0..4 {
        match cell_at(doc, row, col) {
            Some(c) if doc.node(c).is_some_and(|v| v.role == Role::ColumnHeader) => {
                found = Some(c);
            }
            _ => break,
        }
        match next_row(doc, table, row) {
            Some(r) => row = r,
            None => break,
        }
    }
    found
}

/// The row header of `row`: the first row header cell to the left of (or at) column `col`, or
/// the first cell of the row when that is a row header.
pub fn row_header(doc: &WebDoc, row: DocNodeId, col: usize) -> Option<DocNodeId> {
    doc.children(row)
        .filter(|c| is_cell(doc, *c))
        .take(col + 1)
        .find(|c| doc.node(*c).is_some_and(|v| v.role == Role::RowHeader))
}

/// Does the node exist and is it visible (a stale `DocNodeId` after a re-render).
pub fn alive(doc: &WebDoc, id: DocNodeId) -> bool {
    doc.is_attached(id) && visible(doc, id)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::web::{DocOp, NodeSpec, SubtreeSpec};

    fn cell(role: Role, t: &str) -> SubtreeSpec {
        SubtreeSpec::with_children(
            NodeSpec::new(role).flags(NodeFlags::CELL),
            vec![SubtreeSpec::leaf(NodeSpec::new(Role::StaticText).text(t))],
        )
    }

    fn row(cells: Vec<SubtreeSpec>) -> SubtreeSpec {
        SubtreeSpec::with_children(NodeSpec::new(Role::Row).flags(NodeFlags::BLOCK), cells)
    }

    // table: thead[ A B ], tbody[ r1 1 2 ], tbody[ r2 3 4 ]
    fn page() -> (WebDoc, DocNodeId) {
        let mut d = WebDoc::new(3);
        let group = |rows| SubtreeSpec::with_children(NodeSpec::new(Role::Unknown), rows);
        let table = SubtreeSpec::with_children(
            NodeSpec::new(Role::Table),
            vec![
                group(vec![row(vec![
                    cell(Role::ColumnHeader, "Name"),
                    cell(Role::ColumnHeader, "A"),
                    cell(Role::ColumnHeader, "B"),
                ])]),
                group(vec![row(vec![
                    cell(Role::RowHeader, "r1"),
                    cell(Role::Cell, "1"),
                    cell(Role::Cell, "2"),
                ])]),
                group(vec![row(vec![
                    cell(Role::RowHeader, "r2"),
                    cell(Role::Cell, "3"),
                    cell(Role::Cell, "4"),
                ])]),
            ],
        );
        let root = SubtreeSpec::with_children(NodeSpec::new(Role::DocumentWeb), vec![table]);
        assert!(
            d.apply(&DocOp::InsertSubtree {
                parent: None,
                before: None,
                subtree: Arc::new(root),
            })
            .is_ok()
        );
        let t = d.first_of(super::super::Cat::TABLE).unwrap_or_else(|| {
            // the synthetic tables here carry no flags: find it by role
            d.preorder()
                .find(|n| d.node(*n).is_some_and(|v| v.role == Role::Table))
                .unwrap()
        });
        (d, t)
    }

    fn text_of(d: &WebDoc, n: DocNodeId) -> String {
        let (a, b) = d.subtree_text_range(n).unwrap();
        d.text_in(a, b).trim().to_owned()
    }

    fn cell_with(d: &WebDoc, t: &str) -> DocNodeId {
        d.preorder()
            .find(|n| is_cell(d, *n) && text_of(d, *n) == t)
            .unwrap()
    }

    #[test]
    fn moves_cross_row_groups_and_stop_at_the_edges() {
        let (d, _) = page();
        let one = cell_of(&d, cell_with(&d, "1")).unwrap();
        let right = step(&d, one, TableMove::NextCol).unwrap();
        assert_eq!(text_of(&d, right.cell), "2");
        assert_eq!(step(&d, right, TableMove::NextCol), Err(TableEdge::Edge));
        let down = step(&d, right, TableMove::NextRow).unwrap();
        assert_eq!(text_of(&d, down.cell), "4");
        assert_eq!(step(&d, down, TableMove::NextRow), Err(TableEdge::Edge));
        let up = step(&d, down, TableMove::PrevRow).unwrap();
        assert_eq!(text_of(&d, up.cell), "2");
        let top = step(&d, down, TableMove::FirstRow).unwrap();
        assert_eq!(text_of(&d, top.cell), "B");
        assert_eq!(
            text_of(&d, step(&d, down, TableMove::LastCol).unwrap().cell),
            "4"
        );
        assert_eq!(
            text_of(&d, step(&d, down, TableMove::FirstCol).unwrap().cell),
            "r2"
        );
        assert_eq!(step(&d, top, TableMove::PrevRow), Err(TableEdge::Edge));
    }

    #[test]
    fn headers_come_from_the_header_row_and_the_row_header_cell() {
        let (d, t) = page();
        let three = cell_of(&d, cell_with(&d, "3")).unwrap();
        let col = column_of(&d, three.row, three.cell).unwrap();
        assert_eq!(col, 1);
        assert_eq!(text_of(&d, column_header(&d, t, col).unwrap()), "A");
        assert_eq!(text_of(&d, row_header(&d, three.row, col).unwrap()), "r2");
        // A text node inside a cell finds its cell.
        let txt = d.first_child(three.cell).unwrap();
        assert_eq!(cell_of(&d, txt).unwrap().cell, three.cell);
    }

    #[test]
    fn outside_a_table_there_is_no_cell() {
        let (d, _) = page();
        let root = d.root().unwrap();
        assert!(cell_of(&d, root).is_none());
    }
}
