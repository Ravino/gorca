//! Table navigation in browse mode (NVDA `Ctrl+Alt+arrows`, M8): move the cursor from cell to
//! cell, speak the cell with the header that changed, say a row or a column. Everything reads one
//! published `WebDoc`; no IPC.

use gorca_i18n::{MsgId as M, t};
use std::time::Instant;

use gorca_doc::web::table::{self, CellRef};
use gorca_doc::web::{TableEdge, TableMove, WebDoc};
use gorca_model::DocNodeId;

use super::cursor;
use super::present::{self, Item};
use crate::commands::CoreCmd;
use crate::reader::{Reader, ReaderEffect, ReaderEffects};

/// A row or column read aloud stops after this many cells.
const SAY_CAP: usize = 256;

/// The table move a command stands for.
#[allow(clippy::wildcard_enum_match_arm)] // the command set is closed; only the table moves matter here
pub fn table_move(c: CoreCmd) -> Option<TableMove> {
    Some(match c {
        CoreCmd::TableNextRow => TableMove::NextRow,
        CoreCmd::TablePrevRow => TableMove::PrevRow,
        CoreCmd::TableNextColumn => TableMove::NextCol,
        CoreCmd::TablePrevColumn => TableMove::PrevCol,
        CoreCmd::TableFirstRow => TableMove::FirstRow,
        CoreCmd::TableLastRow => TableMove::LastRow,
        CoreCmd::TableFirstColumn => TableMove::FirstCol,
        CoreCmd::TableLastColumn => TableMove::LastCol,
        _ => return None,
    })
}

/// The row/column reading commands: `(whole column, say-all)`.
#[allow(clippy::wildcard_enum_match_arm)] // the command set is closed; only four matter here
pub fn table_say(c: CoreCmd) -> Option<bool> {
    match c {
        CoreCmd::TableSayRow | CoreCmd::TableSayAllRow => Some(false),
        CoreCmd::TableSayColumn | CoreCmd::TableSayAllColumn => Some(true),
        _ => None,
    }
}

/// The trimmed text of a node's subtree.
fn text_of(doc: &WebDoc, id: DocNodeId) -> String {
    doc.subtree_text_range(id)
        .map(|(a, b)| {
            doc.text_in(a, b.min(a.saturating_add(present::QUICK_NAV_CAP)))
                .replace(['\n', '\u{FFFC}'], " ")
                .split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

/// A cell's speech: its content, or "blank".
fn cell_items(doc: &WebDoc, cell: DocNodeId, from: Option<u32>) -> Vec<Item> {
    let Some(a) = cursor::content_start(doc, cell) else {
        return vec![Item::Text(t(M::WebBlank).to_owned())];
    };
    let end = doc
        .subtree_text_range(cell)
        .map_or(a, |(_, e)| e)
        .min(a.saturating_add(present::QUICK_NAV_CAP));
    let mut items = if a < end {
        present::describe_from(doc, a, end, from)
    } else {
        Vec::new()
    };
    items.retain(|i| !i.text().trim().is_empty());
    if items.is_empty() {
        items.push(Item::Text(t(M::WebBlank).to_owned()));
    }
    items
}

/// What a move to `to` says: the header that changed, then the cell.
fn move_items(doc: &WebDoc, from: CellRef, to: CellRef, mv: TableMove, at: u32) -> Vec<Item> {
    let mut items = Vec::new();
    let is_header = |c: DocNodeId| {
        doc.node(c).is_some_and(|v| {
            matches!(
                v.role,
                gorca_model::Role::ColumnHeader | gorca_model::Role::RowHeader
            )
        })
    };
    if !is_header(to.cell) {
        let col = table::column_of(doc, to.row, to.cell).unwrap_or(0);
        if mv.is_vertical() {
            let was = table::column_of(doc, from.row, from.cell)
                .and_then(|c| table::row_header(doc, from.row, c));
            if let Some(h) = table::row_header(doc, to.row, col).filter(|h| Some(*h) != was) {
                items.push(Item::Text(text_of(doc, h)));
            }
        } else {
            let was = table::column_of(doc, from.row, from.cell)
                .and_then(|c| table::column_header(doc, from.table, c));
            if let Some(h) = table::column_header(doc, to.table, col).filter(|h| Some(*h) != was) {
                items.push(Item::Text(text_of(doc, h)));
            }
        }
    }
    items.extend(cell_items(doc, to.cell, Some(at)));
    items
}

impl Reader {
    /// A table move from the cursor: cell, header changes, edge.
    pub(super) fn web_table_move(&mut self, mv: TableMove, now: Instant, fx: &mut ReaderEffects) {
        let Some((doc, cur, _)) = self.browse_ctx(fx) else {
            return;
        };
        let Some(b) = self.web.as_ref() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        let last = b.last_off;
        let res = h.read.with(|d| {
            let at = cursor::offset_of(d, cur).unwrap_or_else(|| last.min(d.text_len()));
            let from = table::cell_of(d, cur.node).ok_or(TableEdge::NotInCell)?;
            let to = table::step(d, from, mv)?;
            let off = cursor::content_start(d, to.cell).ok_or(TableEdge::Edge)?;
            let cur = cursor::cursor_at(d, off).ok_or(TableEdge::Edge)?;
            Ok((cur, off, move_items(d, from, to, mv, at)))
        });
        match res {
            Ok((cur, off, items)) => {
                self.web_set_cursor(doc, cur, off, now);
                self.web_speak(&items, fx);
                fx.push(ReaderEffect::ArmTimer(
                    now + super::commands::SCROLL_DEBOUNCE,
                ));
            }
            Err(TableEdge::NotInCell) => self.say(t(M::WebNotInCell), fx),
            Err(TableEdge::Edge) => self.say(t(M::WebTableEdge), fx),
        }
    }

    /// Read the whole row (`column == false`) or column of the cell under the cursor.
    pub(super) fn web_table_say(&mut self, column: bool, fx: &mut ReaderEffects) {
        let Some((doc, cur, _)) = self.browse_ctx(fx) else {
            return;
        };
        let Some(b) = self.web.as_ref() else { return };
        let Some(h) = b.docs.get(doc) else { return };
        let items = h.read.with(|d| {
            let from = table::cell_of(d, cur.node)?;
            let col = table::column_of(d, from.row, from.cell)?;
            let mut out = Vec::new();
            if column {
                let mut row = table::first_row(d, from.table);
                let mut n = 0;
                while let Some(r) = row {
                    if n >= SAY_CAP {
                        break;
                    }
                    if let Some(c) = table::cell_at(d, r, col) {
                        out.push(Item::Text(text_of(d, c)));
                    }
                    row = table::next_row(d, from.table, r);
                    n += 1;
                }
            } else {
                for c in table::cells(d, from.row).into_iter().take(SAY_CAP) {
                    out.push(Item::Text(text_of(d, c)));
                }
            }
            Some(out)
        });
        match items {
            Some(items) => self.web_speak(&items, fx),
            None => self.say(t(M::WebNotInCell), fx),
        }
    }
}
