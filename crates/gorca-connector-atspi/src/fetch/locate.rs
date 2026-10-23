// SPDX-License-Identifier: GPL-3.0-or-later
//! The pointer resolver (ADR-0025 section 2.4): a node, or a character of a text node, to the point a
//! pointer must be put on, described in the toplevel's own AT-SPI coordinate space plus what the
//! pointer sink needs to find the window (pid, title, the toplevel's origin and size, and whether
//! children are reported in device pixels). The core never sees stage coordinates; the sink maps this
//! to the compositor (`gorca-shell-helper`, WM-1..4).
//!
//! Target point: the centre of the character's rectangle for a text offset (the line's, for the end of a
//! line), else the centre of the node's extents, clipped to the toplevel and to every scrolling ancestor
//! (an empty intersection means the object is off screen: "no location"). A node that is not SHOWING has no
//! location, and the application's own hit test (`GetAccessibleAtPoint`), where it has one, must put the point
//! on the node or inside it. The extents are what the application reported; the sink's window guard and
//! occlusion check are the last safety net, so a stale or wrong rectangle is refused, not clicked.

use std::sync::Arc;

use gorca_connector_api::{BackendError, BackendReply};
use gorca_model::{NodeKey, Rect};
use gorca_output_api::{ChildSpace, PointTarget, ToplevelHint};
use gorca_rt::Deadline;

use super::review::sane_rect;
use super::{CallCx, backend_error, resolve};

/// Ancestors walked to find the toplevel (a frame or window); a cycle or a very deep tree stops here.
const MAX_DEPTH: usize = 64;

/// The target point of a rectangle: the centre of what is left of it after clipping to every rectangle of `clips`
/// (the toplevel, when it is in the same space as the object, and the scrolling ancestors), `None` when nothing
/// is left: the object is scrolled or clipped out of view.
pub(crate) fn target_point(r: Rect, clips: &[Rect]) -> Option<(f64, f64)> {
    let mut x0 = f64::from(r.x);
    let mut y0 = f64::from(r.y);
    let mut x1 = x0 + f64::from(r.w);
    let mut y1 = y0 + f64::from(r.h);
    for c in clips {
        x0 = x0.max(f64::from(c.x));
        y0 = y0.max(f64::from(c.y));
        x1 = x1.min(f64::from(c.x) + f64::from(c.w));
        y1 = y1.min(f64::from(c.y) + f64::from(c.h));
    }
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some((f64::midpoint(x0, x1), f64::midpoint(y0, y1)))
}

/// `Locate`: see the module docs.
pub(super) async fn locate(
    cx: &CallCx,
    node: NodeKey,
    offset: Option<u32>,
    dl: Deadline,
) -> Result<BackendReply, BackendError> {
    let n = resolve(cx, node)?;
    let target = locate_path(&cx.caller, &n.path, offset, dl, |bus, path| {
        cx.resolve_ref(bus, path).map(|r| r.path)
    })
    .await?;
    Ok(BackendReply::Target(target))
}

/// The resolver for one object, independent of the connector's path table: `parent_of` turns the
/// `(bus, path)` of an ancestor into a path we may call (refusing other bus names).
pub(crate) async fn locate_path(
    c: &super::caller::Caller,
    start: &Arc<str>,
    offset: Option<u32>,
    dl: Deadline,
    parent_of: impl Fn(&str, &str) -> Option<Arc<str>>,
) -> Result<PointTarget, BackendError> {
    let pid = c.pid().ok_or(BackendError::NotSupported)?;
    // An object that is not SHOWING (hidden, or a list row the toolkit has unmapped) has no place on screen.
    if let Ok(words) = c.state(start, dl).await
        && !has_state(&words, atspi::State::Showing)
    {
        return Err(BackendError::Protocol("not showing".to_owned()));
    }
    let rect = target_rect(c, start, offset, dl).await?;
    // The toplevel: the first ancestor (or the node itself) that is a frame or a window; on the way up the
    // rectangles of the scrolling ancestors (scroll panes, viewports, documents) that clip the object.
    let mut path: Arc<str> = Arc::clone(start);
    let mut top_path: Option<Arc<str>> = None;
    let mut clips: Vec<Rect> = Vec::new();
    for depth in 0..MAX_DEPTH {
        let raw = c.role(&path, dl).await.map_err(|e| backend_error(&e))?;
        if is_toplevel_role(raw) {
            top_path = Some(Arc::clone(&path));
            break;
        }
        if depth > 0
            && is_clipping_role(raw)
            && let Ok((x, y, w, h)) = c.extents_window(&path, dl).await
            && let Some(r) = sane_rect(x, y, w, h)
        {
            clips.push(r);
        }
        let Some((bus, parent)) = c
            .parent_ref(&path, dl)
            .await
            .map_err(|e| backend_error(&e))?
        else {
            break;
        };
        // An ancestor in another process must not aim our calls elsewhere.
        let Some(next) = parent_of(&bus, &parent) else {
            break;
        };
        path = next;
    }
    let top_path = top_path.ok_or(BackendError::NotSupported)?;
    let (tx, ty, tw, th) = c
        .extents_window(&top_path, dl)
        .await
        .map_err(|e| backend_error(&e))?;
    let top_rect = sane_rect(tx, ty, tw, th).ok_or(BackendError::NotSupported)?;
    let title = c
        .name_of(c.bus_name(), &top_path, dl)
        .await
        .map_err(|e| backend_error(&e))?;
    let device = c.children_in_device_pixels();
    // The toplevel is the active window (`ACTIVE`): a tie between identical windows may then be broken by focus.
    let active = c
        .state(&top_path, dl)
        .await
        .is_ok_and(|words| has_state(&words, atspi::State::Active));
    // The toplevel's own rectangle is logical when its children are in device pixels: it cannot clip them
    // (the scrolling ancestors, which are in the children's space, do; the sink's window guard checks the rest).
    if !device {
        clips.push(top_rect);
    }
    let (x, y) = target_point(rect, &clips).ok_or(BackendError::NotSupported)?;
    // Hit test: where the application itself says the point belongs to must be the object or inside it. Not for
    // Chromium/Electron (quirk `blink-hit-test`): their `GetAccessibleAtPoint` answers an object of another branch
    // (measured: an unrelated node of the page for a point on a button), so the document clip and the sink's pick
    // are the checks there.
    if !device && !hit_test_ok(c, &top_path, start, (x, y), dl, &parent_of).await {
        return Err(BackendError::Protocol("hit test disagrees".to_owned()));
    }
    Ok(PointTarget::Window {
        top: ToplevelHint {
            pid,
            title: Arc::from(title.as_str()),
            atspi_origin: (top_rect.x, top_rect.y),
            atspi_size: (
                i32::try_from(top_rect.w).unwrap_or(i32::MAX),
                i32::try_from(top_rect.h).unwrap_or(i32::MAX),
            ),
            child_space: if device {
                ChildSpace::DevicePixels
            } else {
                ChildSpace::Toplevel
            },
            // The registration's own pid (`GetConnectionCredentials`): never a bus proxy's here. A
            // Flatpak application (proxy pid) is not matched until it is verified as such.
            pid_is_proxy: false,
            active,
        },
        x,
        y,
    })
}

/// The rectangle to aim at: the character's for a text offset, the node's own otherwise. A character with no
/// rectangle (the end of a line, an empty line) takes the rectangle of its line, then of the character before it,
/// so the pointer lands on the caret's line and never on the node's centre; only an empty text (offset 0) uses
/// the node's extents, there is no other line.
async fn target_rect(
    c: &super::caller::Caller,
    start: &Arc<str>,
    offset: Option<u32>,
    dl: Deadline,
) -> Result<Rect, BackendError> {
    if let Some(off) = offset {
        let o = i32::try_from(off).unwrap_or(i32::MAX - 1);
        if let Ok((x, y, w, h)) = c.range_extents(start, o, o.saturating_add(1), dl).await
            && let Some(r) = sane_rect(x, y, w, h)
        {
            return Ok(r);
        }
        if let Ok((_, ls, le)) = c.line_at(start, o, dl).await
            && le > ls
            && let Ok((x, y, w, h)) = c.range_extents(start, ls, le, dl).await
            && let Some(r) = sane_rect(x, y, w, h)
        {
            return Ok(r);
        }
        if o > 0
            && let Ok((x, y, w, h)) = c.range_extents(start, o - 1, o, dl).await
            && let Some(r) = sane_rect(x, y, w, h)
        {
            return Ok(r);
        }
        if o > 0 {
            return Err(BackendError::NotSupported);
        }
    }
    let (x, y, w, h) = c
        .extents_window(start, dl)
        .await
        .map_err(|e| backend_error(&e))?;
    sane_rect(x, y, w, h).ok_or(BackendError::NotSupported)
}

/// Does the application's own hit test agree that `at` is on `start` or inside it? Toolkits answer the direct child
/// of the asked container (GTK3) or the deepest object (Gecko, Blink), so the question is repeated on every answer
/// until the object answers itself or nothing. A toolkit with no hit test (an error, a null answer on the toplevel,
/// an object of another process) agrees: the sink's pick is then the only check.
async fn hit_test_ok(
    c: &super::caller::Caller,
    top: &Arc<str>,
    start: &Arc<str>,
    at: (f64, f64),
    dl: Deadline,
    parent_of: &impl Fn(&str, &str) -> Option<Arc<str>>,
) -> bool {
    #[allow(clippy::cast_possible_truncation)] // coordinates are below COORD_LIMIT
    let (x, y) = (at.0.round() as i32, at.1.round() as i32);
    let mut cur: Arc<str> = Arc::clone(top);
    for _ in 0..MAX_DEPTH {
        let Ok(answer) = c.accessible_at_point(&cur, x, y, dl).await else {
            return true;
        };
        let next = answer.and_then(|(bus, p)| parent_of(&bus, &p));
        match next {
            // nothing below `cur` at the point: `cur` is the deepest object there
            None if *cur == **top => return true,
            None => break,
            Some(n) if n == cur => break,
            Some(n) => cur = n,
        }
    }
    // `cur` must be `start` or inside it ...
    let mut path = Arc::clone(&cur);
    for _ in 0..MAX_DEPTH {
        if path == *start {
            return true;
        }
        match c.parent_ref(&path, dl).await {
            Ok(Some((b, p))) => match parent_of(&b, &p) {
                Some(next) => path = next,
                None => break,
            },
            Ok(None) => break,
            // an object that vanished while asking: no verdict
            Err(_) => return true,
        }
    }
    // ... or an ancestor of it: Blink answers the document for a point on a button and does not descend, so an
    // ancestor is "the toolkit cannot tell". Only an object of another branch is a disagreement.
    let mut path = Arc::clone(start);
    for _ in 0..MAX_DEPTH {
        if path == cur {
            return true;
        }
        match c.parent_ref(&path, dl).await {
            Ok(Some((b, p))) => match parent_of(&b, &p) {
                Some(next) => path = next,
                None => return false,
            },
            Ok(None) => return false,
            Err(_) => return true,
        }
    }
    false
}

/// A role whose rectangle clips its descendants: scroll panes, viewports, documents and inline frames.
fn is_clipping_role(raw: u32) -> bool {
    use atspi::Role;
    matches!(
        Role::try_from(raw),
        Ok(Role::ScrollPane
            | Role::Viewport
            | Role::DocumentFrame
            | Role::DocumentWeb
            | Role::DocumentText
            | Role::DocumentEmail
            | Role::DocumentSpreadsheet
            | Role::DocumentPresentation
            | Role::InternalFrame)
    )
}

/// Resolve one object of the application at `bus_name` without a connector (tools, end-to-end tests):
/// identifies the toolkit like the app server does, then runs the same resolver as `BackendOp::Locate`.
/// Ancestors are followed only inside `bus_name`.
pub async fn locate_object(
    conn: zbus::Connection,
    bus_name: &str,
    path: &str,
    offset: Option<u32>,
    pid: u32,
    dl: Deadline,
) -> Result<PointTarget, BackendError> {
    let bus: Arc<str> = Arc::from(bus_name);
    let c = super::caller::Caller::new(conn, Arc::clone(&bus));
    c.set_pid(Some(pid));
    if let Some((toolkit, app_name)) = c.app_info(dl).await {
        let t = crate::toolkit::Toolkit::classify(&toolkit, &app_name);
        c.set_text_screen_coords(t.text_extents_use_screen_coords());
        c.set_children_in_device_pixels(t.children_in_device_pixels());
    }
    let start: Arc<str> = Arc::from(path);
    locate_path(&c, &start, offset, dl, |b, p| {
        (b == &*bus && !p.is_empty()).then(|| Arc::from(p))
    })
    .await
}

/// Is `s` set in the AT-SPI state bit set (two 32-bit words, least significant first)? `atspi::State` values are
/// the bit masks themselves (`Active` is `1 << 1`).
fn has_state(words: &[u32], s: atspi::State) -> bool {
    crate::decode::state_bits(words) & (s as u64) != 0
}

/// A frame, window or dialog: what the compositor shows as a window. Dialogs and menus of GTK3 are
/// their own AT-SPI windows.
fn is_toplevel_role(raw: u32) -> bool {
    use atspi::Role;
    matches!(
        Role::try_from(raw),
        Ok(Role::Frame | Role::Window | Role::Dialog | Role::FileChooser | Role::Alert)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect { x, y, w, h }
    }

    #[test]
    fn a_rectangle_inside_the_clips_keeps_its_centre() {
        let p = target_point(r(100, 100, 80, 40), &[r(0, 0, 500, 400)]).unwrap();
        assert_eq!(p, (140.0, 120.0));
        assert_eq!(target_point(r(10, 20, 100, 40), &[]), Some((60.0, 40.0)));
    }

    #[test]
    fn a_partly_visible_rectangle_aims_at_its_visible_part() {
        let top = [r(0, 0, 500, 400)];
        let p = target_point(r(-60, 10, 80, 20), &top).unwrap();
        assert_eq!(p, (10.0, 20.0));
        let p = target_point(r(450, 380, 100, 100), &top).unwrap();
        assert_eq!(p, (475.0, 390.0));
    }

    #[test]
    fn a_rectangle_outside_the_toplevel_has_no_location() {
        let top = [r(0, 0, 500, 400)];
        assert_eq!(target_point(r(600, 10, 50, 50), &top), None);
        assert_eq!(target_point(r(-100, -100, 50, 50), &top), None);
        // touching only at an edge is not a visible area
        assert_eq!(target_point(r(500, 10, 50, 50), &top), None);
    }

    #[test]
    fn the_toplevel_origin_may_be_negative_like_an_xwayland_title_bar() {
        let p = target_point(r(230, 250, 20, 24), &[r(0, -38, 500, 438)]).unwrap();
        assert_eq!(p, (240.0, 262.0));
    }

    #[test]
    fn a_row_scrolled_out_of_its_scroll_pane_has_no_location_though_inside_the_window() {
        // window 0..600, a scroll pane 100..300 tall; the row at y 400 is inside the window, not the pane
        let clips = [r(0, 100, 500, 200), r(0, 0, 500, 600)];
        assert_eq!(target_point(r(10, 400, 100, 20), &clips), None);
        // a row half out of the pane aims at the visible half
        let p = target_point(r(10, 290, 100, 20), &clips).unwrap();
        assert_eq!(p, (60.0, 295.0));
    }

    #[test]
    fn device_pixel_children_are_clipped_by_their_own_scrolling_ancestor_only() {
        // Chromium at 1.25: the document is 1200x760 device px; the logical toplevel (960x688) is not a clip
        let doc = [r(0, 100, 1200, 760)];
        let p = target_point(r(375, 358, 200, 75), &doc).unwrap();
        assert_eq!(p, (475.0, 395.5));
        // below the document's visible rectangle: not visible, although inside the device-pixel window
        assert_eq!(target_point(r(375, 900, 200, 75), &doc), None);
    }

    #[test]
    fn scrolling_ancestors_are_documents_panes_and_viewports() {
        assert!(is_clipping_role(atspi::Role::ScrollPane as u32));
        assert!(is_clipping_role(atspi::Role::DocumentWeb as u32));
        assert!(!is_clipping_role(atspi::Role::Button as u32));
        assert!(!is_clipping_role(atspi::Role::Frame as u32));
    }

    #[test]
    fn states_are_read_from_the_state_words_as_masks() {
        let mut words = vec![0u32, 0u32];
        assert!(!has_state(&words, atspi::State::Active));
        words[0] |= 1 << 1; // ATSPI_STATE_ACTIVE
        assert!(has_state(&words, atspi::State::Active));
        assert!(!has_state(&words, atspi::State::Showing));
        words[0] |= 1 << 25; // ATSPI_STATE_SHOWING
        assert!(has_state(&words, atspi::State::Showing));
        assert!(!has_state(&[], atspi::State::Active));
    }

    #[test]
    fn only_frames_windows_and_dialogs_are_toplevels() {
        assert!(is_toplevel_role(atspi::Role::Frame as u32));
        assert!(is_toplevel_role(atspi::Role::Dialog as u32));
        assert!(!is_toplevel_role(atspi::Role::Button as u32));
        assert!(!is_toplevel_role(atspi::Role::Application as u32));
    }
}
