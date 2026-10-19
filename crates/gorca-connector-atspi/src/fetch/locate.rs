// SPDX-License-Identifier: GPL-3.0-or-later
//! The pointer resolver (ADR-0025 section 2.4): a node, or a character of a text node, to the point a
//! pointer must be put on, described in the toplevel's own AT-SPI coordinate space plus what the
//! pointer sink needs to find the window (pid, title, the toplevel's origin and size, and whether
//! children are reported in device pixels). The core never sees stage coordinates; the sink maps this
//! to the compositor (`gorca-shell-helper`, WM-1..4).
//!
//! Target point: the centre of the character's rectangle for a text offset, else the centre of the
//! node's extents, kept inside the toplevel (an empty intersection means the object is off screen:
//! "no location"). The extents are what the application reported; the sink's window guard and
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

/// The centre of `r` as a point.
fn centre(r: Rect) -> (f64, f64) {
    (
        f64::from(r.x) + f64::from(r.w) / 2.0,
        f64::from(r.y) + f64::from(r.h) / 2.0,
    )
}

/// Keep `p` inside `top` (the toplevel's extents in the same space); `None` when the rectangle `r`
/// does not intersect it at all (the object is scrolled or clipped out of the window).
pub(crate) fn clip_to_toplevel(r: Rect, top: Rect) -> Option<(f64, f64)> {
    let x0 = f64::from(r.x).max(f64::from(top.x));
    let y0 = f64::from(r.y).max(f64::from(top.y));
    let x1 = (f64::from(r.x) + f64::from(r.w)).min(f64::from(top.x) + f64::from(top.w));
    let y1 = (f64::from(r.y) + f64::from(r.h)).min(f64::from(top.y) + f64::from(top.h));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some((f64::midpoint(x0, x1), f64::midpoint(y0, y1)))
}

/// The target point of a rectangle: clipped to the toplevel when both are in the same space, the
/// plain centre when the children are in device pixels (the toplevel is logical, so the two cannot
/// be compared here; the helper's window guard decides).
pub(crate) fn target_point(r: Rect, top: Rect, device_pixels: bool) -> Option<(f64, f64)> {
    if device_pixels {
        Some(centre(r))
    } else {
        clip_to_toplevel(r, top)
    }
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
    // The character's rectangle for a text offset (the toolkit's coordinate type is already set,
    // quirk `blink-text-extents-frame`), the node's own extents otherwise or when the text gives none.
    let mut rect: Option<Rect> = None;
    if let Some(off) = offset {
        let start_off = i32::try_from(off).unwrap_or(i32::MAX - 1);
        if let Ok((x, y, w, h)) = c
            .range_extents(start, start_off, start_off.saturating_add(1), dl)
            .await
        {
            rect = sane_rect(x, y, w, h);
        }
    }
    if rect.is_none() {
        let (x, y, w, h) = c
            .extents_window(start, dl)
            .await
            .map_err(|e| backend_error(&e))?;
        rect = sane_rect(x, y, w, h);
    }
    let rect = rect.ok_or(BackendError::NotSupported)?;
    // The toplevel: the first ancestor (or the node itself) that is a frame or a window.
    let mut path: Arc<str> = Arc::clone(start);
    let mut top_path: Option<Arc<str>> = None;
    for _ in 0..MAX_DEPTH {
        let raw = c.role(&path, dl).await.map_err(|e| backend_error(&e))?;
        if is_toplevel_role(raw) {
            top_path = Some(Arc::clone(&path));
            break;
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
    let (x, y) = target_point(rect, top_rect, device).ok_or(BackendError::NotSupported)?;
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

/// Is `s` set in the AT-SPI state bit set (two 32-bit words, bit index = `State` value)?
#[allow(clippy::cast_possible_truncation)] // `State` values are below 64
fn has_state(words: &[u32], s: atspi::State) -> bool {
    let bit = s as u32;
    words
        .get((bit / 32) as usize)
        .is_some_and(|w| w & (1 << (bit % 32)) != 0)
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
    fn the_centre_of_a_rectangle() {
        assert_eq!(centre(r(10, 20, 100, 40)), (60.0, 40.0));
    }

    #[test]
    fn a_rectangle_inside_the_toplevel_keeps_its_centre() {
        let p = target_point(r(100, 100, 80, 40), r(0, 0, 500, 400), false).unwrap();
        assert_eq!(p, (140.0, 120.0));
    }

    #[test]
    fn a_partly_visible_rectangle_aims_at_its_visible_part() {
        // 80 px wide, the left 60 px stick out of the window: the visible 20 px are 0..20.
        let p = target_point(r(-60, 10, 80, 20), r(0, 0, 500, 400), false).unwrap();
        assert_eq!(p, (10.0, 20.0));
        let p = target_point(r(450, 380, 100, 100), r(0, 0, 500, 400), false).unwrap();
        assert_eq!(p, (475.0, 390.0));
    }

    #[test]
    fn a_rectangle_outside_the_toplevel_has_no_location() {
        assert_eq!(
            target_point(r(600, 10, 50, 50), r(0, 0, 500, 400), false),
            None
        );
        assert_eq!(
            target_point(r(-100, -100, 50, 50), r(0, 0, 500, 400), false),
            None
        );
        // touching only at an edge is not a visible area
        assert_eq!(
            target_point(r(500, 10, 50, 50), r(0, 0, 500, 400), false),
            None
        );
    }

    #[test]
    fn the_toplevel_origin_may_be_negative_like_an_xwayland_title_bar() {
        // XWayland: toplevel WINDOW origin (0,-38); a node at y 262 is inside.
        let p = target_point(r(230, 250, 20, 24), r(0, -38, 500, 438), false).unwrap();
        assert_eq!(p, (240.0, 262.0));
    }

    #[test]
    fn device_pixel_children_are_not_clipped_against_the_logical_toplevel() {
        // Chromium at 1.25: a child at (375,358) 200x75 device px, toplevel 960x688 logical.
        let p = target_point(r(375, 358, 200, 75), r(0, 0, 960, 688), true).unwrap();
        assert_eq!(p, (475.0, 395.5));
    }

    #[test]
    fn the_active_state_is_read_from_the_state_words() {
        let bit = atspi::State::Active as u32;
        let mut words = vec![0u32, 0u32];
        assert!(!has_state(&words, atspi::State::Active));
        words[(bit / 32) as usize] |= 1 << (bit % 32);
        assert!(has_state(&words, atspi::State::Active));
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
