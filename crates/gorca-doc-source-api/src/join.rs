//! Identity reconciliation port (ADR-0001 section 9.3, IR-2, IR-4).

use gorca_model::{NodeKey, SrcNodeId};
use gorca_rt::mpsc;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum JoinQuality {
    /// Same id from both transports.
    Exact,
    /// Must be confirmed by a `Validate` request or a later event before actions use it (IR-4).
    Heuristic,
}

/// "This AT-SPI node is that source node." Built during the document build or update, never
/// lazily on focus (IR-2).
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct JoinEntry {
    pub atspi: NodeKey,
    pub src: SrcNodeId,
    pub quality: JoinQuality,
}

#[derive(Clone, Debug)]
pub struct JoinPort {
    tx: mpsc::Sender<JoinEntry>,
}

#[derive(Debug)]
pub struct JoinReceiver {
    pub entries: mpsc::Receiver<JoinEntry>,
}

pub fn join_channel(capacity: usize) -> (JoinPort, JoinReceiver) {
    let (tx, rx) = mpsc::channel(capacity);
    (JoinPort { tx }, JoinReceiver { entries: rx })
}

impl JoinPort {
    /// Never blocks. `false` means the entry was dropped; the join will miss and the Reader
    /// falls back to the AT-SPI snapshot (IR-3), counted by M-13.
    pub fn offer(&self, entry: JoinEntry) -> bool {
        self.tx.try_send(entry).is_ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_model::{AppId, ConnectorId, LocalId};

    #[test]
    fn offer_reports_drops() {
        let (port, mut rx) = join_channel(1);
        let e = JoinEntry {
            atspi: NodeKey {
                app: AppId {
                    connector: ConnectorId(0),
                    serial: 0,
                },
                local: LocalId(1),
            },
            src: SrcNodeId(1),
            quality: JoinQuality::Exact,
        };
        assert!(port.offer(e));
        assert!(!port.offer(e));
        assert_eq!(rx.entries.try_recv().unwrap(), e);
    }
}
