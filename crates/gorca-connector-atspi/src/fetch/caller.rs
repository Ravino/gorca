// SPDX-License-Identifier: GPL-3.0-or-later
//! The call connection of one application, with the shadow-mode gate.
//!
//! The connection, the bus name and the shadow flag are private to this module: strategy code
//! (`strategy.rs`, which a fill strategy replaces) can only use the operations below, so it cannot
//! reach `call_method` or the raw connection and bypass the allowlist of [`super::shadow`]. A test
//! reads the strategy sources and fails if they name either.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use gorca_dbus::{DbusError, call_method};
use gorca_rt::Deadline;
use zbus::Connection;
use zbus::zvariant::{OwnedObjectPath, OwnedValue, Value as Zv};

use super::{
    ACCESSIBLE, APPLICATION, CACHE, CACHE_PATH, ENTRY_TEXT_CHARS, ERROR_TEXT_CHARS,
    MAX_SUBTREE_NODES, NULL_PATH, PROPS, RELATION_ERROR_MESSAGE, RELATION_LABELLED_BY, ROOT_PATH,
    TEXT, VALUE, items, shadow,
};

/// `(so)` as a variant: `Some((bus, path))`, `None` for the null object or a wrong shape.
fn ref_of_value(v: &Zv<'_>) -> Option<(String, String)> {
    match v {
        Zv::Value(inner) => ref_of_value(inner),
        // The path is `o` on the wire; some applications send `s`: both are read.
        Zv::Structure(s) => {
            let [Zv::Str(bus), path] = s.fields() else {
                return None;
            };
            let path = match path {
                Zv::ObjectPath(p) => p.as_str(),
                Zv::Str(p) => p.as_str(),
                _ => return None,
            };
            (path != NULL_PATH && !bus.is_empty()).then(|| (bus.to_string(), path.to_owned()))
        }
        _ => None,
    }
}

/// One application on the call connection.
#[derive(Clone, Debug)]
pub struct Caller {
    conn: Connection,
    bus_name: Arc<str>,
    /// Shadow mode (PW-11): `Some` refuses every call outside [`shadow::allowed`] and counts it.
    shadow: Option<Arc<AtomicU64>>,
    /// The coordinate type of `Text` extents and points: `WINDOW`, or `SCREEN` for a toolkit whose
    /// window-relative text extents are offset from its component extents. Set when the toolkit
    /// is identified; shared by the clones of this caller.
    text_coord: Arc<AtomicU32>,
}

impl Caller {
    pub fn new(conn: Connection, bus_name: Arc<str>) -> Self {
        Self {
            conn,
            bus_name,
            shadow: None,
            text_coord: Arc::new(AtomicU32::new(COORD_WINDOW)),
        }
    }

    /// Quirk row `blink-text-extents-frame`: ask `Text` for screen coordinates (`screen`) or
    /// window coordinates (the default).
    pub fn set_text_screen_coords(&self, screen: bool) {
        let c = if screen { COORD_SCREEN } else { COORD_WINDOW };
        self.text_coord.store(c, Ordering::Relaxed);
    }

    fn text_coord(&self) -> u32 {
        self.text_coord.load(Ordering::Relaxed)
    }

    /// Read-only: refuse (and count in `refused`) any call the shadow allowlist does not admit.
    #[must_use]
    pub fn shadowed(mut self, refused: Arc<AtomicU64>) -> Self {
        self.shadow = Some(refused);
        self
    }

    async fn call<B>(
        &self,
        dest: &str,
        deadline: Deadline,
        path: &str,
        iface: &str,
        member: &str,
        body: &B,
    ) -> Result<zbus::Message, DbusError>
    where
        B: serde::Serialize + zbus::zvariant::DynamicType,
    {
        if let Some(refused) = &self.shadow
            && !shadow::allowed(iface, member)
        {
            refused.fetch_add(1, Ordering::Relaxed);
            return Err(DbusError::Zbus(zbus::Error::Failure(format!(
                "shadow mode refuses {iface}.{member}"
            ))));
        }
        call_method(&self.conn, deadline, dest, path, iface, member, body).await
    }

    pub async fn role(&self, path: &str, deadline: Deadline) -> Result<u32, DbusError> {
        let m = self
            .call(&self.bus_name, deadline, path, ACCESSIBLE, "GetRole", &())
            .await?;
        m.body().deserialize::<u32>().map_err(DbusError::Zbus)
    }

    pub async fn state(&self, path: &str, deadline: Deadline) -> Result<Vec<u32>, DbusError> {
        let m = self
            .call(&self.bus_name, deadline, path, ACCESSIBLE, "GetState", &())
            .await?;
        m.body().deserialize::<Vec<u32>>().map_err(DbusError::Zbus)
    }

    /// `Accessible.GetAttributes` of `path`: the toolkit's object attributes, `(name, value)`.
    /// An error reply (an object without attributes) is an answer, empty; only a timeout or a
    /// dead connection is an error, so that the node is asked again.
    pub(super) async fn attributes_of(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<Vec<(String, String)>, DbusError> {
        let m = match self
            .call(
                &self.bus_name,
                deadline,
                path,
                ACCESSIBLE,
                "GetAttributes",
                &(),
            )
            .await
        {
            Ok(m) => m,
            Err(DbusError::Zbus(zbus::Error::MethodError(..))) => return Ok(Vec::new()),
            Err(e) => return Err(e),
        };
        let map = m
            .body()
            .deserialize::<std::collections::HashMap<String, String>>()
            .map_err(DbusError::Zbus)?;
        Ok(map.into_iter().collect())
    }

    pub(super) async fn prop(
        &self,
        dest: &str,
        path: &str,
        iface: &str,
        prop: &str,
        deadline: Deadline,
    ) -> Result<OwnedValue, DbusError> {
        let m = self
            .call(dest, deadline, path, PROPS, "Get", &(iface, prop))
            .await?;
        m.body()
            .deserialize::<OwnedValue>()
            .map_err(DbusError::Zbus)
    }

    pub(super) async fn string_prop(
        &self,
        dest: &str,
        path: &str,
        iface: &str,
        prop: &str,
        deadline: Deadline,
    ) -> Result<String, DbusError> {
        let v = self.prop(dest, path, iface, prop, deadline).await?;
        String::try_from(v).map_err(|e| DbusError::Zbus(e.into()))
    }

    pub(super) async fn f64_prop(
        &self,
        path: &str,
        prop: &str,
        deadline: Deadline,
    ) -> Result<f64, DbusError> {
        let v = self
            .prop(&self.bus_name, path, VALUE, prop, deadline)
            .await?;
        f64::try_from(v).map_err(|e| DbusError::Zbus(e.into()))
    }

    /// `Name` of an object, in this application or (for label relations) another peer.
    pub(super) async fn name_of(
        &self,
        dest: &str,
        path: &str,
        deadline: Deadline,
    ) -> Result<String, DbusError> {
        self.string_prop(dest, path, ACCESSIBLE, "Name", deadline)
            .await
    }

    /// The first `LabelledBy` target: `(bus name, path)`.
    pub(super) async fn label_target(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<Option<(String, String)>, DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                ACCESSIBLE,
                "GetRelationSet",
                &(),
            )
            .await?;
        let sets = m
            .body()
            .deserialize::<Vec<(u32, Vec<(String, OwnedObjectPath)>)>>()
            .map_err(DbusError::Zbus)?;
        Ok(sets
            .into_iter()
            .find(|(kind, _)| *kind == RELATION_LABELLED_BY)
            .and_then(|(_, targets)| targets.into_iter().next())
            .map(|(bus, p)| (bus, p.as_str().to_owned())))
    }

    /// The text of the `aria-errormessage` target of `path` (relation `error-message`), when the
    /// target lives in this application: its `Text` (at most `ERROR_TEXT_CHARS`, never past the
    /// end, which Gecko refuses), else its name. `None` when there is no relation or no text.
    pub(super) async fn error_text_of(&self, path: &str, deadline: Deadline) -> Option<String> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                ACCESSIBLE,
                "GetRelationSet",
                &(),
            )
            .await
            .ok()?;
        let sets = m
            .body()
            .deserialize::<Vec<(u32, Vec<(String, OwnedObjectPath)>)>>()
            .ok()?;
        let (bus, target) = sets
            .into_iter()
            .find(|(kind, _)| *kind == RELATION_ERROR_MESSAGE)
            .and_then(|(_, targets)| targets.into_iter().next())?;
        if bus != *self.bus_name {
            return None;
        }
        let target = target.as_str();
        let count = match self
            .prop(&self.bus_name, target, TEXT, "CharacterCount", deadline)
            .await
        {
            Ok(v) => i32::try_from(v).unwrap_or(0),
            Err(_) => 0,
        };
        let end = count.clamp(0, ERROR_TEXT_CHARS);
        let text = match self
            .call(
                &self.bus_name,
                deadline,
                target,
                TEXT,
                "GetText",
                &(0i32, end),
            )
            .await
        {
            Ok(m) => m
                .body()
                .deserialize::<String>()
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty()),
            Err(_) => None,
        };
        let text = match text {
            Some(t) => t,
            None => self
                .name_of(&self.bus_name, target, deadline)
                .await
                .ok()
                .map(|s| s.trim().to_owned())
                .filter(|s| !s.is_empty())?,
        };
        let text = text.replace(['\u{FFFC}', '\n'], " ");
        let text = text.trim();
        (!text.is_empty()).then(|| text.to_owned())
    }

    pub(super) async fn text_prefix(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<String, DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                TEXT,
                "GetText",
                &(0i32, ENTRY_TEXT_CHARS),
            )
            .await?;
        m.body().deserialize::<String>().map_err(DbusError::Zbus)
    }

    pub fn bus_name(&self) -> &str {
        &self.bus_name
    }

    pub(super) async fn description_of(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<String, DbusError> {
        self.string_prop(&self.bus_name, path, ACCESSIBLE, "Description", deadline)
            .await
    }

    /// The `Parent` property: `Some((bus, path))`, or `None` for the root (null object).
    pub(super) async fn parent_of(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<Option<(String, String)>, DbusError> {
        let v = self
            .prop(&self.bus_name, path, ACCESSIBLE, "Parent", deadline)
            .await?;
        Ok(ref_of_value(&v))
    }

    pub(super) async fn child_count_of(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<i32, DbusError> {
        let v = self
            .prop(&self.bus_name, path, ACCESSIBLE, "ChildCount", deadline)
            .await?;
        i32::try_from(v).map_err(|e| DbusError::Zbus(e.into()))
    }

    /// `GetChildAtIndex`: `None` past the end.
    pub(super) async fn child_at(
        &self,
        path: &str,
        index: u32,
        deadline: Deadline,
    ) -> Result<Option<(String, String)>, DbusError> {
        let i = i32::try_from(index).unwrap_or(i32::MAX);
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                ACCESSIBLE,
                "GetChildAtIndex",
                &(i,),
            )
            .await?;
        let (bus, p) = m
            .body()
            .deserialize::<(String, OwnedObjectPath)>()
            .map_err(DbusError::Zbus)?;
        Ok((p.as_str() != NULL_PATH && !bus.is_empty()).then(|| (bus, p.as_str().to_owned())))
    }

    /// `GetChildren`, at most `MAX_SUBTREE_NODES` refs of a reply of at most 16 MiB.
    pub(super) async fn children_of(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<Vec<(String, String)>, DbusError> {
        self.children_of_capped(path, deadline, items::MAX_DUMP_BYTES)
            .await
    }

    /// [`Self::children_of`] for a reply of at most `max_bytes`: a longer one is refused before
    /// it is decoded (a prefetch must not allocate for a 300 000-row list it only wants a
    /// window of; the caller reads such a parent by index instead).
    pub(super) async fn children_of_capped(
        &self,
        path: &str,
        deadline: Deadline,
        max_bytes: usize,
    ) -> Result<Vec<(String, String)>, DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                ACCESSIBLE,
                "GetChildren",
                &(),
            )
            .await?;
        if m.body().data().len() > max_bytes {
            return Err(DbusError::Zbus(zbus::Error::Failure(
                "GetChildren reply too large".to_owned(),
            )));
        }
        let kids = m
            .body()
            .deserialize::<Vec<(String, OwnedObjectPath)>>()
            .map_err(DbusError::Zbus)?;
        Ok(kids
            .into_iter()
            .take(MAX_SUBTREE_NODES as usize)
            .map(|(b, p)| (b, p.as_str().to_owned()))
            .collect())
    }

    /// `Cache.GetItems`; the reply is decoded by the caller (it can be large).
    pub(super) async fn get_items(&self, deadline: Deadline) -> Result<zbus::Message, DbusError> {
        self.call(&self.bus_name, deadline, CACHE_PATH, CACHE, "GetItems", &())
            .await
    }

    /// Quirk row `blink-wake-call` ([`crate::Toolkit::needs_wake_call`]): one `GetAttributes` and
    /// one `GetRelationSet` on the application root. Both are reads; their only effect is that
    /// Chromium learns an AT uses the tree and builds the web content tree. Best effort: the
    /// replies are ignored. Returns whether either call was answered.
    pub async fn wake(&self, deadline: Deadline) -> bool {
        let (a, r) = tokio::join!(
            self.call(
                &self.bus_name,
                deadline,
                ROOT_PATH,
                ACCESSIBLE,
                "GetAttributes",
                &()
            ),
            self.call(
                &self.bus_name,
                deadline,
                ROOT_PATH,
                ACCESSIBLE,
                "GetRelationSet",
                &()
            )
        );
        a.is_ok() || r.is_ok()
    }

    /// Toolkit and application name of the app root, for `AppInfo` (best effort).
    pub async fn app_info(&self, deadline: Deadline) -> Option<(String, String)> {
        let (toolkit, name) = tokio::join!(
            self.string_prop(
                &self.bus_name,
                ROOT_PATH,
                APPLICATION,
                "ToolkitName",
                deadline
            ),
            self.name_of(&self.bus_name, ROOT_PATH, deadline)
        );
        Some((toolkit.ok()?, name.ok()?))
    }
}

// ---- review calls (ADR-0006 15.1): extents, text lines, actions, collection ------------------
const COMPONENT: &str = "org.a11y.atspi.Component";
const ACTION: &str = "org.a11y.atspi.Action";
const SELECTION: &str = "org.a11y.atspi.Selection";
const COLLECTION: &str = "org.a11y.atspi.Collection";
/// `ATSPI_COORD_TYPE_WINDOW`: the only coordinate type gorca ever requests (SF-4). `SCREEN` (0)
/// is meaningless on Wayland and GTK4 answers it with the origin.
const COORD_WINDOW: u32 = 1;
/// `ATSPI_COORD_TYPE_SCREEN`: asked for text extents only where a toolkit's window-relative text
/// extents are not in the frame of `Component.GetExtents` (`Toolkit::text_extents_use_screen_coords`).
const COORD_SCREEN: u32 = 0;
/// A `GetText` or `GetStringAtOffset` reply body larger than this is refused undecoded: the
/// window is at most 16 Ki scalars (64 KiB) and a spoken unit is clipped far below that.
const MAX_TEXT_REPLY_BYTES: usize = 256 * 1024;
/// `ATSPI_TEXT_GRANULARITY_LINE`.
const GRANULARITY_LINE: u32 = 3;

/// One `Collection.GetMatches` rule on the wire: `(aiia{ss}iaiiasib)`.
pub(super) type WireRule = (
    Vec<i32>,
    i32,
    std::collections::HashMap<String, String>,
    i32,
    Vec<i32>,
    i32,
    Vec<String>,
    i32,
    bool,
);

impl Caller {
    /// `Component.GetExtents(WINDOW)`.
    pub(super) async fn extents_window(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<(i32, i32, i32, i32), DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                COMPONENT,
                "GetExtents",
                &(COORD_WINDOW,),
            )
            .await?;
        m.body()
            .deserialize::<(i32, i32, i32, i32)>()
            .map_err(DbusError::Zbus)
    }

    pub(super) async fn grab_focus(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<bool, DbusError> {
        let m = self
            .call(&self.bus_name, deadline, path, COMPONENT, "GrabFocus", &())
            .await?;
        m.body().deserialize::<bool>().map_err(DbusError::Zbus)
    }

    pub(crate) async fn text_char_count(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<i32, DbusError> {
        let v = self
            .prop(&self.bus_name, path, TEXT, "CharacterCount", deadline)
            .await?;
        i32::try_from(v).map_err(|e| DbusError::Zbus(e.into()))
    }

    pub(crate) async fn text_caret(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<i32, DbusError> {
        let v = self
            .prop(&self.bus_name, path, TEXT, "CaretOffset", deadline)
            .await?;
        i32::try_from(v).map_err(|e| DbusError::Zbus(e.into()))
    }

    /// The reply of a text call is refused before it is decoded when its body is larger than
    /// any text the Reader would use (a hostile or minified multi-megabyte line): decoding it
    /// would copy it only to throw most of it away.
    fn text_reply_fits(m: &zbus::Message) -> Result<(), DbusError> {
        if m.body().data().len() > MAX_TEXT_REPLY_BYTES {
            return Err(DbusError::Zbus(zbus::Error::Failure(
                "text reply too large".to_owned(),
            )));
        }
        Ok(())
    }

    pub(crate) async fn text_range(
        &self,
        path: &str,
        start: i32,
        end: i32,
        deadline: Deadline,
    ) -> Result<String, DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                TEXT,
                "GetText",
                &(start, end),
            )
            .await?;
        Self::text_reply_fits(&m)?;
        m.body().deserialize::<String>().map_err(DbusError::Zbus)
    }

    /// `Text.GetStringAtOffset(offset, LINE)`: `(text, start, end)`.
    pub(super) async fn line_at(
        &self,
        path: &str,
        offset: i32,
        deadline: Deadline,
    ) -> Result<(String, i32, i32), DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                TEXT,
                "GetStringAtOffset",
                &(offset, GRANULARITY_LINE),
            )
            .await?;
        m.body()
            .deserialize::<(String, i32, i32)>()
            .map_err(DbusError::Zbus)
    }

    /// `Text.GetStringAtOffset(offset, granularity)`: `(text, start, end)`.
    pub(crate) async fn string_at(
        &self,
        path: &str,
        offset: i32,
        granularity: u32,
        deadline: Deadline,
    ) -> Result<(String, i32, i32), DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                TEXT,
                "GetStringAtOffset",
                &(offset, granularity),
            )
            .await?;
        Self::text_reply_fits(&m)?;
        m.body()
            .deserialize::<(String, i32, i32)>()
            .map_err(DbusError::Zbus)
    }

    /// `Text.GetNSelections()`.
    pub(crate) async fn n_selections(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<i32, DbusError> {
        let m = self
            .call(&self.bus_name, deadline, path, TEXT, "GetNSelections", &())
            .await?;
        m.body().deserialize::<i32>().map_err(DbusError::Zbus)
    }

    /// `Text.GetSelection(i)`: `(start, end)`.
    pub(crate) async fn selection_range(
        &self,
        path: &str,
        index: i32,
        deadline: Deadline,
    ) -> Result<(i32, i32), DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                TEXT,
                "GetSelection",
                &(index,),
            )
            .await?;
        m.body()
            .deserialize::<(i32, i32)>()
            .map_err(DbusError::Zbus)
    }

    /// `Text.GetAttributeRun(offset, include_defaults = true)`: the attributes of the run that
    /// holds `offset` with the defaults filled in, and the run `(start, end)`.
    pub(crate) async fn attribute_run(
        &self,
        path: &str,
        offset: i32,
        deadline: Deadline,
    ) -> Result<(std::collections::HashMap<String, String>, i32, i32), DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                TEXT,
                "GetAttributeRun",
                &(offset, true),
            )
            .await?;
        m.body()
            .deserialize::<(std::collections::HashMap<String, String>, i32, i32)>()
            .map_err(DbusError::Zbus)
    }

    /// `Text.GetRangeExtents(start, end, WINDOW)`.
    pub(super) async fn range_extents(
        &self,
        path: &str,
        start: i32,
        end: i32,
        deadline: Deadline,
    ) -> Result<(i32, i32, i32, i32), DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                TEXT,
                "GetRangeExtents",
                &(start, end, self.text_coord()),
            )
            .await?;
        m.body()
            .deserialize::<(i32, i32, i32, i32)>()
            .map_err(DbusError::Zbus)
    }

    /// `Text.GetOffsetAtPoint(x, y, WINDOW)`.
    pub(super) async fn offset_at_point(
        &self,
        path: &str,
        x: i32,
        y: i32,
        deadline: Deadline,
    ) -> Result<i32, DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                TEXT,
                "GetOffsetAtPoint",
                &(x, y, self.text_coord()),
            )
            .await?;
        m.body().deserialize::<i32>().map_err(DbusError::Zbus)
    }

    pub(super) async fn set_caret(
        &self,
        path: &str,
        offset: i32,
        deadline: Deadline,
    ) -> Result<bool, DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                TEXT,
                "SetCaretOffset",
                &(offset,),
            )
            .await?;
        m.body().deserialize::<bool>().map_err(DbusError::Zbus)
    }

    pub(super) async fn n_actions(&self, path: &str, deadline: Deadline) -> Result<i32, DbusError> {
        let v = self
            .prop(&self.bus_name, path, ACTION, "NActions", deadline)
            .await?;
        i32::try_from(v).map_err(|e| DbusError::Zbus(e.into()))
    }

    pub(super) async fn do_action(
        &self,
        path: &str,
        index: i32,
        deadline: Deadline,
    ) -> Result<bool, DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                ACTION,
                "DoAction",
                &(index,),
            )
            .await?;
        m.body().deserialize::<bool>().map_err(DbusError::Zbus)
    }

    pub(super) async fn index_in_parent(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<i32, DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                ACCESSIBLE,
                "GetIndexInParent",
                &(),
            )
            .await?;
        m.body().deserialize::<i32>().map_err(DbusError::Zbus)
    }

    pub(super) async fn select_child(
        &self,
        parent_path: &str,
        index: i32,
        deadline: Deadline,
    ) -> Result<bool, DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                parent_path,
                SELECTION,
                "SelectChild",
                &(index,),
            )
            .await?;
        m.body().deserialize::<bool>().map_err(DbusError::Zbus)
    }

    /// `Collection.GetMatches(rule, SORT_ORDER_CANONICAL, count, traverse = true)`.
    pub(super) async fn get_matches(
        &self,
        path: &str,
        rule: WireRule,
        count: i32,
        deadline: Deadline,
    ) -> Result<Vec<(String, String)>, DbusError> {
        let m = self
            .call(
                &self.bus_name,
                deadline,
                path,
                COLLECTION,
                "GetMatches",
                &(rule, 0u32, count, true),
            )
            .await?;
        if m.body().data().len() > items::MAX_DUMP_BYTES {
            return Err(DbusError::Zbus(zbus::Error::Failure(
                "GetMatches reply too large".to_owned(),
            )));
        }
        let kids = m
            .body()
            .deserialize::<Vec<(String, OwnedObjectPath)>>()
            .map_err(DbusError::Zbus)?;
        Ok(kids
            .into_iter()
            .map(|(b, p)| (b, p.as_str().to_owned()))
            .collect())
    }

    /// The `Parent` of an object as `(bus, path)`.
    pub(super) async fn parent_ref(
        &self,
        path: &str,
        deadline: Deadline,
    ) -> Result<Option<(String, String)>, DbusError> {
        self.parent_of(path, deadline).await
    }
}
