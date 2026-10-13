//! The compiled keymap and the types the input lane looks chords up with (ADR-0005 4.1, 5.2).
//!
//! `gorca-core` compiles a `ResolvedKeymap` (chord and command names) into this immutable,
//! O(1)-lookup form and publishes it with `ArcSwap`; the input lane only ever loads it.

use std::collections::HashMap;
use std::sync::Arc;

use arc_swap::ArcSwap;

use crate::event::{CmdId, Modifiers};
use crate::grab::GrabSet;

/// Fieldless mirror of the Reader's `Mode`: what the keymap is keyed by.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum ModeKind {
    #[default]
    Focus,
    Browse,
    Review,
    PassThrough,
    InputHelp,
}

/// Lookup context published with every grab set (ADR-0005 5.2).
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub struct InputContext {
    pub mode: ModeKind,
    /// The base mode to return to; equals `mode` unless it is `PassThrough` or `InputHelp`.
    pub resume: ModeKind,
}

impl InputContext {
    pub const fn of(mode: ModeKind) -> Self {
        Self { mode, resume: mode }
    }
}

/// A physical key (K-8). `Sym` is the fallback when no keymap is available.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub enum KeyRef {
    Code(u16),
    Sym(u32),
}

/// Chord identity used for lookup. Lock bits never take part (K-6).
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct ChordKey {
    /// Includes the virtual `GORCA` bit, excludes the locks.
    pub mods: Modifiers,
    pub key: KeyRef,
}

/// What a keymap entry does.
#[allow(clippy::struct_excessive_bools)]
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Bound {
    pub cmd: CmdId,
    /// Implemented: executes and is grabbed. Planned commands are bound but inert (KC-10).
    pub active: bool,
    /// Autorepeat runs the command again (voice settings).
    pub repeatable: bool,
    /// Highest press count the command distinguishes (1 to 3).
    pub max_press: u8,
    /// Runs even in learn mode (`speech.stop`, `input.learn_mode_toggle`).
    pub bypass: bool,
    /// K-3: pressing the chord cancels stale speech.
    pub interrupts: bool,
}

/// Where a table applies (ADR-0005 5.2). App contexts arrive with extensions (M5).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum TableKind {
    Global,
    Focus,
    Browse,
    Review,
}

impl TableKind {
    const fn index(self) -> usize {
        match self {
            TableKind::Global => 0,
            TableKind::Focus => 1,
            TableKind::Browse => 2,
            TableKind::Review => 3,
        }
    }
}

type Table = HashMap<(ChordKey, u8), Option<Bound>>;

/// Immutable chord tables. A `None` entry is a tombstone (KC-3): it blocks fall-through.
#[derive(Clone, Debug, Default)]
pub struct CompiledKeymap {
    tables: [Table; 4],
    /// Keysyms of the gorca modifiers in effect (CapsLock may have been dropped).
    pub modifiers: Vec<u32>,
    /// Commands the lane itself triggers.
    pub stop: Option<CmdId>,
    pub pause: Option<CmdId>,
    pub learn_toggle: Option<CmdId>,
}

impl CompiledKeymap {
    pub fn new(modifiers: Vec<u32>) -> Self {
        Self {
            modifiers,
            ..Self::default()
        }
    }

    pub fn insert(&mut self, table: TableKind, key: ChordKey, presses: u8, b: Option<Bound>) {
        self.tables[table.index()].insert((key, presses), b);
    }

    #[allow(clippy::option_option)] // the outer Option is "found", the inner one a tombstone
    fn find(&self, order: [TableKind; 2], key: ChordKey, presses: u8) -> Option<Option<Bound>> {
        order
            .iter()
            .find_map(|t| self.tables[t.index()].get(&(key, presses)).copied())
    }

    /// `(mode, *)` then `(global, *)`; the first hit wins, a tombstone ends the search.
    pub fn lookup(&self, mode: ModeKind, key: ChordKey, presses: u8) -> Option<Bound> {
        let specific = match mode {
            ModeKind::Browse => TableKind::Browse,
            ModeKind::Review => TableKind::Review,
            ModeKind::Focus | ModeKind::PassThrough | ModeKind::InputHelp => TableKind::Focus,
        };
        self.find([specific, TableKind::Global], key, presses)
            .flatten()
    }

    /// Every chord bound in `mode`, for building grab sets: implemented bindings only (K-1).
    pub fn active_chords(&self, mode: ModeKind) -> Vec<(ChordKey, u8, Bound)> {
        let specific = match mode {
            ModeKind::Browse => TableKind::Browse,
            ModeKind::Review => TableKind::Review,
            ModeKind::Focus | ModeKind::PassThrough | ModeKind::InputHelp => TableKind::Focus,
        };
        let mut out = Vec::new();
        for kind in [specific, TableKind::Global] {
            for ((k, p), b) in &self.tables[kind.index()] {
                // A more specific table's entry (even a tombstone) hides the global one.
                if kind == TableKind::Global
                    && self.tables[specific.index()].contains_key(&(*k, *p))
                {
                    continue;
                }
                if let Some(b) = b
                    && b.active
                {
                    out.push((*k, *p, *b));
                }
            }
        }
        out
    }

    pub fn len(&self) -> usize {
        self.tables.iter().map(HashMap::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The grab sets as the key source sees them, published for the input lane.
#[derive(Clone, Debug, Default)]
pub struct GrabView {
    pub committed_gen: u32,
    pub committed: Arc<GrabSet>,
    pub wanted_gen: Option<u32>,
    pub wanted: Option<Arc<GrabSet>>,
}

/// Settings the lane reads on every key (wait-free), swapped by the config task.
#[derive(Clone, Debug)]
pub struct LaneSettings {
    pub multi_press_ms: u32,
    /// Half-width of the uncertainty band around `repeat_delay` (ADR-0005 4.3).
    pub double_tap_guard_ms: u32,
    /// GNOME's keyboard repeat delay, which Mutter's first-click rule compares with.
    pub repeat_delay_ms: u32,
    pub interrupt_on_keys: bool,
    pub stop_speech_on_control: bool,
    pub pause_speech_on_shift: bool,
}

impl Default for LaneSettings {
    fn default() -> Self {
        Self {
            multi_press_ms: 500,
            double_tap_guard_ms: 15,
            repeat_delay_ms: 500,
            interrupt_on_keys: true,
            stop_speech_on_control: true,
            pause_speech_on_shift: true,
        }
    }
}

/// Everything the lane reads from other tasks. All cells are single-writer, wait-free to load.
#[derive(Debug, Default)]
pub struct LaneLinks {
    pub keymap: ArcSwap<CompiledKeymap>,
    pub settings: ArcSwap<LaneSettings>,
    pub view: ArcSwap<GrabView>,
    /// Key intents the Reader has handled: the lane's evidence that the Reader is alive while
    /// learn mode holds the keyboard (LM-3).
    pub reader_ack: std::sync::atomic::AtomicU64,
    /// Bumped by the key source whenever its connection to the keyboard manager starts or ends:
    /// Mutter's key state is unknown across that boundary, so the lane forgets its own (F3).
    pub session: std::sync::atomic::AtomicU64,
}

impl LaneLinks {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ck(code: u16) -> ChordKey {
        ChordKey {
            mods: Modifiers::GORCA,
            key: KeyRef::Code(code),
        }
    }

    #[allow(clippy::unnecessary_wraps)]
    fn b(n: u32) -> Option<Bound> {
        Some(Bound {
            cmd: CmdId(n),
            active: true,
            repeatable: false,
            max_press: 1,
            bypass: false,
            interrupts: true,
        })
    }

    #[test]
    fn specific_table_wins_and_tombstones_block_fall_through() {
        let mut k = CompiledKeymap::default();
        k.insert(TableKind::Global, ck(1), 1, b(1));
        k.insert(TableKind::Global, ck(2), 1, b(2));
        k.insert(TableKind::Browse, ck(1), 1, b(10));
        k.insert(TableKind::Browse, ck(2), 1, None);
        assert_eq!(
            k.lookup(ModeKind::Focus, ck(1), 1).map(|b| b.cmd),
            Some(CmdId(1))
        );
        assert_eq!(
            k.lookup(ModeKind::Browse, ck(1), 1).map(|b| b.cmd),
            Some(CmdId(10))
        );
        assert_eq!(k.lookup(ModeKind::Browse, ck(2), 1), None);
        assert_eq!(
            k.lookup(ModeKind::Review, ck(2), 1).map(|b| b.cmd),
            Some(CmdId(2))
        );
        assert_eq!(
            k.lookup(ModeKind::Focus, ck(1), 2),
            None,
            "press count is part of the key"
        );
    }

    #[test]
    fn active_chords_hide_shadowed_and_inert_entries() {
        let mut k = CompiledKeymap::default();
        k.insert(TableKind::Global, ck(1), 1, b(1));
        k.insert(TableKind::Global, ck(2), 1, b(2));
        k.insert(
            TableKind::Global,
            ck(3),
            1,
            Some(Bound {
                active: false,
                ..b(3).unwrap()
            }),
        );
        k.insert(TableKind::Browse, ck(2), 1, None);
        let focus: Vec<_> = k
            .active_chords(ModeKind::Focus)
            .iter()
            .map(|c| c.2.cmd)
            .collect();
        assert_eq!(focus.len(), 2);
        let browse: Vec<_> = k
            .active_chords(ModeKind::Browse)
            .iter()
            .map(|c| c.2.cmd)
            .collect();
        assert_eq!(browse, vec![CmdId(1)]);
    }
}
