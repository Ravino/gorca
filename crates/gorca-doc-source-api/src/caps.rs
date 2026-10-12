use bitflags::bitflags;
use gorca_model::TextUnit;

#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SourceKind {
    Atspi,
    /// TBD-S1: Chromium DevTools Accessibility domain over a pipe.
    Cdp,
    /// TBD-S2: Firefox WebExtension plus native-messaging host.
    WebExt,
    /// TBD-S4: composition of two of the above.
    Hybrid,
}

bitflags! {
    /// Actions a source can execute (IR-5: an action goes to the source that declared it).
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct ActionCaps: u8 {
        const FOCUS            = 1 << 0;
        const CLICK            = 1 << 1;
        const SCROLL_INTO_VIEW = 1 << 2;
        const SET_CARET        = 1 << 3;
        const SET_SELECTION    = 1 << 4;
    }
}

bitflags! {
    /// Join keys a source can provide (IR-4). TBD-S3 fills the concrete kinds per browser.
    #[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
    pub struct JoinKeyCaps: u8 {
        /// Same id from both transports (for example a DOM or backend node id in AT-SPI attrs).
        const EXACT     = 1 << 0;
        /// Ancestor path, role, name, index in parent, bounds.
        const HEURISTIC = 1 << 1;
    }
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct SourceCaps {
    /// Can deliver a whole document in one or a few messages.
    pub bulk_snapshot: bool,
    /// Pushes mutations itself (no re-query needed).
    pub push_deltas: bool,
    /// Emits `Settled` after a mutation batch.
    pub settled_markers: bool,
    /// The mirror converts to scalar offsets (TU-2, IR-6).
    pub text_unit: TextUnit,
    pub actions: ActionCaps,
    pub join_keys: JoinKeyCaps,
}
