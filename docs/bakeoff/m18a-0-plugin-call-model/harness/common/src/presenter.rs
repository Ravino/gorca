// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stub presenter and speech sink (SPEC 2.3): compose a `PresIr` from the fused entry through the
//! `gorca-present-ir` constructors, render it to a short string, stamp the instant the text would be
//! handed to the speech client. No D-Bus, no speech server.

use crate::host::Value;
use crate::model::{NodeId, PluginId};
use crate::timing::now_ns;
use gorca_model::{AppId, ConnectorId, LocalId, NodeKey};
use gorca_present_ir::{Content, Field, FieldKind, Origin, PresIr, Reason, RowId};
use smallvec::SmallVec;

/// The fused winner for one (node, group).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fused {
    pub group: u16,
    pub plugin: PluginId,
    pub value: Value,
}

/// Fused entry of one node: observed name/role plus the winning plugin facts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FusedEntry {
    pub node: NodeId,
    pub role: u16,
    pub name: String,
    pub sem_rev: u32,
    pub winners: Vec<Fused>,
}

pub fn compose(e: &FusedEntry, reason: Reason) -> PresIr {
    let mut fields: SmallVec<[Field; 6]> = SmallVec::new();
    fields.push(Field::new(
        FieldKind::NAME,
        Content::Text(e.name.as_str().into()),
        Origin::default(),
    ));
    fields.push(Field::new(
        FieldKind::ROLE,
        Content::Text(format!("role{}", e.role).as_str().into()),
        Origin::default(),
    ));
    for w in &e.winners {
        let txt = match &w.value {
            Value::Text(t) => t.to_string(),
            Value::Number(n) => n.to_string(),
            Value::Code(c) => c.to_string(),
            Value::Bits(b) => format!("{b:x}"),
            Value::Absent | Value::Other => String::new(),
        };
        let origin = Origin {
            row: RowId(w.group),
            group: Origin::NO_GROUP,
            via: w.plugin as u32 + 1,
        };
        fields.push(Field::new(
            FieldKind(FieldKind::EXT_BASE + w.group),
            Content::Text(txt.as_str().into()),
            origin,
        ));
    }
    let key = NodeKey {
        app: AppId {
            connector: ConnectorId(1),
            serial: 1,
        },
        local: LocalId(e.node as u64),
    };
    PresIr::new(reason, key, e.sem_rev, fields)
}

/// Render to the string the speech client would get (text fields in order).
pub fn render(ir: &PresIr, out: &mut String) {
    out.clear();
    for f in &ir.fields {
        if let Content::Text(t) = &f.content {
            if !t.is_empty() {
                if !out.is_empty() {
                    out.push_str(", ");
                }
                out.push_str(t);
            }
        }
    }
}

/// Stamps the instant text is handed to the (absent) speech client.
#[derive(Default)]
pub struct SpeechSink {
    pub last_ns: u64,
    pub last_len: usize,
    pub utterances: u64,
}

impl SpeechSink {
    #[inline]
    pub fn speak(&mut self, text: &str) -> u64 {
        self.last_ns = now_ns();
        self.last_len = text.len();
        self.utterances += 1;
        self.last_ns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compose_and_render() {
        let e = FusedEntry {
            node: 5,
            role: 3,
            name: "row five".into(),
            sem_rev: 1,
            winners: vec![Fused {
                group: 100,
                plugin: 0,
                value: Value::Text("alice".into()),
            }],
        };
        let ir = compose(&e, Reason::Focus);
        assert_eq!(ir.fields.len(), 3);
        let mut s = String::new();
        render(&ir, &mut s);
        assert_eq!(s, "row five, role3, alice");
        let mut sink = SpeechSink::default();
        assert!(sink.speak(&s) > 0);
    }
}
