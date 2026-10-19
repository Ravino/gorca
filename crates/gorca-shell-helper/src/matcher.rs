// SPDX-License-Identifier: GPL-3.0-or-later
//! The window matcher of ADR-0025 section 2.4 (WM-1..4): from what a connector knows about a point
//! (the toplevel's AT-SPI origin, size, pid and title; the point in the toplevel's own coordinate
//! space) to what the compositor understands (a Meta window id, the rectangle to count from and a
//! window-relative offset). Pure and table-tested; the helper adds the origin at call time, and its
//! window guard is the last safety net (WM-4): a wrong mapping is refused, not clicked.
//!
//! The rule, verified by a real click per client in the private shell (study section 3, ADR 14):
//! `offset = (p - atspi_origin) / ratio / child_div`, where `origin` is the Meta rect (frame or
//! buffer) whose size equals `atspi_size / ratio`; `ratio` is 1 for a Wayland window and the
//! global XWayland scale (`x-scale`, C-5) for an X11 window; `child_div` is the monitor scale for
//! clients that report children in device pixels (Chromium, Electron), else 1.

use std::collections::HashMap;

use gorca_output_api::{ChildSpace, ToplevelHint};
use zbus::zvariant::OwnedValue;

use crate::wire::{Origin, WireTarget};

/// Tolerance of the size comparison in logical pixels (rounding of fractional scales, WM-2).
pub const SIZE_TOLERANCE: f64 = 2.0;

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct IRect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

/// One row of `GetWindows` (helper v2).
#[derive(Clone, Debug, PartialEq)]
#[allow(clippy::struct_excessive_bools)] // the helper's row has these flags
pub struct WindowRow {
    pub id: u64,
    pub pid: u32,
    pub title: String,
    pub wayland: bool,
    pub frame: IRect,
    pub buffer: IRect,
    pub monitor: i32,
    pub focused: bool,
    pub minimized: bool,
    pub visible: bool,
}

/// One row of `GetMonitors`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct MonitorRow {
    pub index: i32,
    pub scale: f64,
    /// XWayland's global scale (C-5).
    pub x_scale: f64,
}

/// Why no window was chosen: the user hears "No location" for every one of these.
#[derive(Copy, Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum MatchError {
    #[error("no window matches the toplevel")]
    NoCandidate,
    #[error("several windows match the toplevel and nothing tells them apart")]
    Ambiguous,
    #[error("the window's monitor is unknown")]
    NoMonitor,
    #[error("the toplevel has no usable size")]
    BadHint,
}

fn size_matches(r: &IRect, w: f64, h: f64) -> bool {
    (f64::from(r.w) - w).abs() <= SIZE_TOLERANCE && (f64::from(r.h) - h).abs() <= SIZE_TOLERANCE
}

/// Resolve a point in the toplevel's coordinate space `p` to a window and an offset.
///
/// `windows` is `GetWindows` in the helper's order (stacking order, bottom to top), so a later row
/// is above an earlier one (WM-3).
pub fn resolve(
    hint: &ToplevelHint,
    p: (f64, f64),
    windows: &[WindowRow],
    monitors: &[MonitorRow],
) -> Result<WireTarget, MatchError> {
    if hint.atspi_size.0 <= 0 || hint.atspi_size.1 <= 0 || !p.0.is_finite() || !p.1.is_finite() {
        return Err(MatchError::BadHint);
    }
    let x_scale = monitors.first().map_or(1.0, |m| m.x_scale.max(1.0));
    // WM-1: visible windows of the toplevel's process; if the pid is not ours to compare (Flatpak:
    // the AT-SPI pid may be the bus proxy's) every visible window.
    let usable = |w: &&WindowRow| w.visible && !w.minimized;
    let mut cands: Vec<&WindowRow> = windows
        .iter()
        .filter(usable)
        .filter(|w| w.pid == hint.pid)
        .collect();
    if cands.is_empty() {
        cands = windows.iter().filter(usable).collect();
    }
    // WM-2: the size of the frame or the buffer equals the toplevel's size divided by the ratio.
    let mut matched: Vec<(&WindowRow, Origin, f64)> = Vec::new();
    for w in cands {
        let ratio = if w.wayland { 1.0 } else { x_scale };
        let (aw, ah) = (
            f64::from(hint.atspi_size.0) / ratio,
            f64::from(hint.atspi_size.1) / ratio,
        );
        if size_matches(&w.frame, aw, ah) {
            matched.push((w, Origin::Frame, ratio));
        } else if size_matches(&w.buffer, aw, ah) {
            matched.push((w, Origin::Buffer, ratio));
        }
    }
    // WM-3: equal title, then the focused window. Still ambiguous: no guess (two windows of the same size
    // that nothing tells apart are genuinely ambiguous).
    let chosen = match matched.as_slice() {
        [] => return Err(MatchError::NoCandidate),
        [one] => *one,
        many => {
            let titled: Vec<_> = many
                .iter()
                .copied()
                .filter(|(w, _, _)| w.title == *hint.title)
                .collect();
            let pool = if titled.is_empty() {
                many.to_vec()
            } else {
                titled
            };
            if let [one] = pool.as_slice() {
                *one
            } else {
                let focused: Vec<_> = pool.iter().copied().filter(|(w, _, _)| w.focused).collect();
                if let [one] = focused.as_slice() {
                    *one
                } else {
                    return Err(MatchError::Ambiguous);
                }
            }
        }
    };
    let (w, origin, ratio) = chosen;
    let child_div = match hint.child_space {
        ChildSpace::Toplevel => 1.0,
        ChildSpace::DevicePixels => monitors
            .iter()
            .find(|m| m.index == w.monitor)
            .map(|m| m.scale)
            .ok_or(MatchError::NoMonitor)?,
    };
    let x = (p.0 - f64::from(hint.atspi_origin.0)) / ratio / child_div;
    let y = (p.1 - f64::from(hint.atspi_origin.1)) / ratio / child_div;
    Ok(WireTarget {
        window: w.id,
        origin,
        x,
        y,
    })
}

// ---- decoding of GetWindows / GetMonitors rows ------------------------------------------------

type Row = HashMap<String, OwnedValue>;

fn get_i32(r: &Row, k: &str) -> Option<i32> {
    r.get(k)?.downcast_ref::<i32>().ok()
}

fn get_u32(r: &Row, k: &str) -> Option<u32> {
    r.get(k)?.downcast_ref::<u32>().ok()
}

fn get_u64(r: &Row, k: &str) -> Option<u64> {
    r.get(k)?.downcast_ref::<u64>().ok()
}

fn get_f64(r: &Row, k: &str) -> Option<f64> {
    r.get(k)?.downcast_ref::<f64>().ok()
}

fn get_bool(r: &Row, k: &str) -> Option<bool> {
    r.get(k)?.downcast_ref::<bool>().ok()
}

fn get_str(r: &Row, k: &str) -> Option<String> {
    r.get(k)?.downcast_ref::<&str>().ok().map(str::to_owned)
}

fn rect(r: &Row, prefix: &str) -> Option<IRect> {
    Some(IRect {
        x: get_i32(r, &format!("{prefix}-x"))?,
        y: get_i32(r, &format!("{prefix}-y"))?,
        w: get_i32(r, &format!("{prefix}-w"))?,
        h: get_i32(r, &format!("{prefix}-h"))?,
    })
}

/// Decode the rows of `GetWindows`; a row with a missing field is skipped (the helper is ours, but
/// a hostile or older one must not panic the client).
#[allow(clippy::implicit_hasher)]
pub fn decode_windows(rows: &[Row]) -> Vec<WindowRow> {
    rows.iter()
        .filter_map(|r| {
            Some(WindowRow {
                id: get_u64(r, "id")?,
                pid: get_u32(r, "pid")?,
                title: get_str(r, "title")?,
                wayland: get_str(r, "client-type")? == "wayland",
                frame: rect(r, "frame")?,
                buffer: rect(r, "buffer")?,
                monitor: get_i32(r, "monitor")?,
                focused: get_bool(r, "focused")?,
                minimized: get_bool(r, "minimized")?,
                visible: get_bool(r, "visible")?,
            })
        })
        .collect()
}

#[allow(clippy::implicit_hasher)]
pub fn decode_monitors(rows: &[Row]) -> Vec<MonitorRow> {
    rows.iter()
        .filter_map(|r| {
            Some(MonitorRow {
                index: get_i32(r, "index")?,
                scale: get_f64(r, "scale")?,
                // `x-scale` is helper v2; absent means 1.
                x_scale: get_f64(r, "x-scale").unwrap_or(1.0),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> IRect {
        IRect { x, y, w, h }
    }

    fn win(id: u64, pid: u32, title: &str, frame: IRect, buffer: IRect) -> WindowRow {
        WindowRow {
            id,
            pid,
            title: title.to_owned(),
            wayland: true,
            frame,
            buffer,
            monitor: 0,
            focused: false,
            minimized: false,
            visible: true,
        }
    }

    fn hint(
        pid: u32,
        title: &str,
        origin: (i32, i32),
        size: (i32, i32),
        cs: ChildSpace,
    ) -> ToplevelHint {
        ToplevelHint {
            pid,
            title: Arc::from(title),
            atspi_origin: origin,
            atspi_size: size,
            child_space: cs,
        }
    }

    fn mons(scale: f64, x_scale: f64) -> Vec<MonitorRow> {
        vec![MonitorRow {
            index: 0,
            scale,
            x_scale,
        }]
    }

    fn near(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-6
    }

    #[test]
    fn gtk4_frame_plus_window_coordinates() {
        // GTK4: the AT-SPI toplevel is the GtkWindow = the Meta frame; WINDOW is relative to it.
        let ws = [win(
            7,
            100,
            "T",
            rect(300, 200, 500, 400),
            rect(286, 188, 528, 429),
        )];
        let t = resolve(
            &hint(100, "T", (0, 0), (500, 400), ChildSpace::Toplevel),
            (46.0, 314.0),
            &ws,
            &mons(1.0, 1.0),
        )
        .unwrap();
        assert_eq!((t.window, t.origin), (7, Origin::Frame));
        assert!(near(t.x, 46.0) && near(t.y, 314.0));
    }

    #[test]
    fn gtk3_buffer_origin_when_the_toplevel_includes_the_shadow() {
        // GTK3: the AT-SPI toplevel is 552x489 = the buffer (CSD shadow); frame is 500x437.
        let ws = [win(
            8,
            100,
            "T",
            rect(300, 200, 500, 437),
            rect(274, 177, 552, 489),
        )];
        let t = resolve(
            &hint(100, "T", (0, 0), (552, 489), ChildSpace::Toplevel),
            (26.0, 60.0),
            &ws,
            &mons(1.0, 1.0),
        )
        .unwrap();
        assert_eq!(t.origin, Origin::Buffer);
        assert!(near(t.x, 26.0) && near(t.y, 60.0));
    }

    #[test]
    fn chromium_children_are_device_pixels_divided_by_the_monitor_scale() {
        let ws = [win(
            9,
            5,
            "page",
            rect(0, 32, 960, 688),
            rect(0, 32, 960, 688),
        )];
        let t = resolve(
            &hint(5, "page", (0, 0), (960, 688), ChildSpace::DevicePixels),
            (375.0, 358.0),
            &ws,
            &mons(1.25, 2.0),
        )
        .unwrap();
        assert!(near(t.x, 300.0) && near(t.y, 286.4), "{t:?}");
    }

    #[test]
    fn xwayland_uses_the_global_ratio_and_the_toplevel_origin_with_the_title_bar() {
        // GTK3 under XWayland: AT-SPI 500x438 at WINDOW origin (0,-38); frame 250x219; x-scale 2.
        let mut w = win(
            10,
            7,
            "X",
            rect(355, 267, 250, 219),
            rect(324, 239, 312, 281),
        );
        w.wayland = false;
        let t = resolve(
            &hint(7, "X", (0, -38), (500, 438), ChildSpace::Toplevel),
            (240.0, 262.0),
            &[w],
            &mons(1.25, 2.0),
        )
        .unwrap();
        // The client saw X (240, 262) for a click at frame (120,150): the AT-SPI point is (240, 262 - 38).
        assert_eq!(t.origin, Origin::Frame);
        assert!(near(t.x, 120.0) && near(t.y, 150.0), "{t:?}");
    }

    #[test]
    fn the_ratio_is_global_even_on_a_one_x_monitor() {
        let mut w = win(
            11,
            7,
            "X",
            rect(1110, 100, 250, 219),
            rect(1079, 72, 312, 281),
        );
        w.wayland = false;
        w.monitor = 1;
        let ms = vec![
            MonitorRow {
                index: 0,
                scale: 2.0,
                x_scale: 2.0,
            },
            MonitorRow {
                index: 1,
                scale: 1.0,
                x_scale: 2.0,
            },
        ];
        let t = resolve(
            &hint(7, "X", (0, -38), (500, 438), ChildSpace::Toplevel),
            (240.0, 262.0),
            &[w],
            &ms,
        )
        .unwrap();
        assert!(near(t.x, 120.0));
    }

    #[test]
    fn only_visible_windows_of_the_process_are_candidates_and_a_missing_pid_falls_back_to_all() {
        let mut hidden = win(1, 100, "T", rect(0, 0, 500, 400), rect(0, 0, 500, 400));
        hidden.minimized = true;
        let other = win(2, 999, "T", rect(0, 0, 500, 400), rect(0, 0, 500, 400));
        let h = hint(100, "T", (0, 0), (500, 400), ChildSpace::Toplevel);
        // pid 100 has only a minimized window: falls back to all visible rows (Flatpak case).
        assert_eq!(
            resolve(
                &h,
                (1.0, 1.0),
                &[hidden.clone(), other.clone()],
                &mons(1.0, 1.0)
            )
            .unwrap()
            .window,
            2
        );
        // but with a visible window of the pid, the other process is never considered
        let mine = win(3, 100, "T", rect(0, 0, 500, 400), rect(0, 0, 500, 400));
        assert_eq!(
            resolve(&h, (1.0, 1.0), &[other, mine], &mons(1.0, 1.0))
                .unwrap()
                .window,
            3
        );
        assert_eq!(
            resolve(&h, (1.0, 1.0), &[hidden], &mons(1.0, 1.0)),
            Err(MatchError::NoCandidate)
        );
    }

    #[test]
    fn sizes_match_within_two_pixels_and_not_beyond() {
        let ws = [win(1, 1, "T", rect(0, 0, 499, 402), rect(0, 0, 600, 500))];
        let h = |w| hint(1, "T", (0, 0), (w, 400), ChildSpace::Toplevel);
        assert!(resolve(&h(500), (1.0, 1.0), &ws, &mons(1.0, 1.0)).is_ok());
        assert_eq!(
            resolve(&h(504), (1.0, 1.0), &ws, &mons(1.0, 1.0)),
            Err(MatchError::NoCandidate)
        );
    }

    #[test]
    fn ties_are_broken_by_title_then_focus_and_otherwise_refused() {
        let a = win(1, 1, "A", rect(0, 0, 500, 400), rect(0, 0, 500, 400));
        let mut b = win(2, 1, "B", rect(10, 10, 500, 400), rect(10, 10, 500, 400));
        let h = |t: &str| hint(1, t, (0, 0), (500, 400), ChildSpace::Toplevel);
        assert_eq!(
            resolve(
                &h("B"),
                (1.0, 1.0),
                &[a.clone(), b.clone()],
                &mons(1.0, 1.0)
            )
            .unwrap()
            .window,
            2
        );
        assert_eq!(
            resolve(
                &h("A"),
                (1.0, 1.0),
                &[a.clone(), b.clone()],
                &mons(1.0, 1.0)
            )
            .unwrap()
            .window,
            1
        );
        // equal titles: the focused one
        let mut a2 = a.clone();
        a2.title = "same".to_owned();
        b.title = "same".to_owned();
        b.focused = true;
        assert_eq!(
            resolve(
                &h("same"),
                (1.0, 1.0),
                &[a2.clone(), b.clone()],
                &mons(1.0, 1.0)
            )
            .unwrap()
            .window,
            2
        );
        // nothing tells them apart: no guess
        b.focused = false;
        assert_eq!(
            resolve(&h("same"), (1.0, 1.0), &[a2, b], &mons(1.0, 1.0)),
            Err(MatchError::Ambiguous)
        );
    }

    #[test]
    fn bad_hints_are_refused() {
        let ws = [win(1, 1, "T", rect(0, 0, 500, 400), rect(0, 0, 500, 400))];
        assert_eq!(
            resolve(
                &hint(1, "T", (0, 0), (0, 400), ChildSpace::Toplevel),
                (1.0, 1.0),
                &ws,
                &mons(1.0, 1.0)
            ),
            Err(MatchError::BadHint)
        );
        assert_eq!(
            resolve(
                &hint(1, "T", (0, 0), (500, 400), ChildSpace::Toplevel),
                (f64::NAN, 1.0),
                &ws,
                &mons(1.0, 1.0)
            ),
            Err(MatchError::BadHint)
        );
    }

    #[test]
    fn device_pixels_need_the_windows_monitor() {
        let mut w = win(1, 1, "T", rect(0, 0, 500, 400), rect(0, 0, 500, 400));
        w.monitor = 5;
        assert_eq!(
            resolve(
                &hint(1, "T", (0, 0), (500, 400), ChildSpace::DevicePixels),
                (1.0, 1.0),
                &[w],
                &mons(1.0, 1.0)
            ),
            Err(MatchError::NoMonitor)
        );
    }

    #[test]
    fn decoding_skips_rows_with_missing_fields() {
        let ov = |v: zbus::zvariant::Value<'_>| OwnedValue::try_from(v).unwrap();
        let mut r: Row = HashMap::new();
        r.insert("id".into(), ov(7_u64.into()));
        assert!(decode_windows(&[r]).is_empty());
        let mut m: Row = HashMap::new();
        m.insert("index".into(), ov(0_i32.into()));
        m.insert("scale".into(), ov(1.5_f64.into()));
        let d = decode_monitors(&[m]);
        assert_eq!((d[0].scale, d[0].x_scale), (1.5, 1.0));
    }
}
