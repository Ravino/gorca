//! The off-screen frame wake (`EngineQuirks::wake_offscreen_frames`).
//!
//! Blink serializes a cross-process iframe only while it intersects the viewport. The wake
//! scrolls each empty, not showing frame into view once, waits for its tree, and puts the page
//! back where it was.
//!
//! The page position is never guessed: a reference node that is in view is measured before the
//! first scroll (`GetExtents`, screen coordinates), and the end is `ScrollToPoint` of that very
//! node to the measured position, so the original scroll offset comes back exactly. The task
//! checks the reference between steps: when it moved by anything but our own scroll (the user
//! scrolled, the page shifted), it stops and restores nothing, because the viewport is no longer
//! ours to move. It is cancelled with its document (`shutdown`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_rt::{CancellationToken, Deadline};

use crate::p2p::AppConn;
use crate::roles;
use crate::wire;

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const COMPONENT: &str = "org.a11y.atspi.Component";
/// ATSPI_COORD_TYPE_SCREEN.
const SCREEN: u32 = 0;
/// ATSPI_SCROLL_TOP_LEFT.
const SCROLL_TOP_LEFT: u32 = 6;
/// Polls of one frame and the gap between them.
pub(crate) const POLLS: u32 = 12;
pub(crate) const POLL: Duration = Duration::from_millis(60);
/// Candidates examined for the reference node.
pub(crate) const REFS_MAX: usize = 16;

/// What the build found: frames to wake and nodes (document order, top of the page first) one
/// of which is the position reference.
#[derive(Debug, Default)]
pub(crate) struct WakePlan {
    pub frames: Vec<String>,
    pub refs: Vec<String>,
}

type Pos = (i32, i32);

async fn call_state(app: &AppConn, path: &str, timeout: Duration) -> Option<u64> {
    let m = app
        .call(
            app.next_lane(),
            Deadline(Instant::now() + timeout),
            path,
            ACCESSIBLE,
            "GetState",
            &(),
        )
        .await
        .ok()?;
    wire::parse_state(m.body().data().bytes()).ok()
}

/// Top-left corner of `path` in screen coordinates, `None` when the call fails or the node
/// has no area.
async fn position(app: &AppConn, path: &str, timeout: Duration) -> Option<Pos> {
    let m = app
        .call(
            app.next_lane(),
            Deadline(Instant::now() + timeout),
            path,
            COMPONENT,
            "GetExtents",
            &(SCREEN,),
        )
        .await
        .ok()?;
    let (x, y, w, h): (i32, i32, i32, i32) = m.body().deserialize().ok()?;
    (w > 0 && h > 0).then_some((x, y))
}

async fn find_reference(
    app: &AppConn,
    refs: &[String],
    timeout: Duration,
) -> Option<(String, Pos)> {
    for r in refs {
        let showing = call_state(app, r, timeout)
            .await
            .is_some_and(|st| st & (1 << roles::state_bit::SHOWING) != 0);
        if !showing {
            continue;
        }
        if let Some(p) = position(app, r, timeout).await {
            return Some((r.clone(), p));
        }
    }
    None
}

/// Run the wake until it ends or `shutdown` fires.
pub(crate) async fn run(
    app: Arc<AppConn>,
    plan: WakePlan,
    timeout: Duration,
    shutdown: CancellationToken,
) {
    tokio::select! {
        () = shutdown.cancelled() => {}
        () = wake(&app, plan, timeout) => {}
    }
}

async fn wake(app: &AppConn, plan: WakePlan, timeout: Duration) {
    let Some((reference, origin)) = find_reference(app, &plan.refs, timeout).await else {
        // Nothing in view to measure the scroll position against: leave the frames asleep
        // rather than scroll the page without a way back.
        tracing::debug!(
            frames = plan.frames.len(),
            "no reference node, frames not woken"
        );
        return;
    };
    let (mut woke, mut scrolled) = (0usize, 0usize);
    let mut still_ours = true;
    'frames: for f in &plan.frames {
        // Only a frame that is not in view sleeps.
        let showing = call_state(app, f, timeout)
            .await
            .is_some_and(|st| st & (1 << roles::state_bit::SHOWING) != 0);
        if showing {
            continue;
        }
        // The viewport is where the previous step left it, or somebody moved it.
        if scrolled > 0 && !still_ours {
            break;
        }
        if app
            .call(
                app.next_lane(),
                Deadline(Instant::now() + timeout),
                f,
                COMPONENT,
                "ScrollTo",
                &(SCROLL_TOP_LEFT,),
            )
            .await
            .is_err()
        {
            continue;
        }
        scrolled += 1;
        let mut after_scroll: Option<Pos> = None;
        // The frame's renderer needs a frame or two to serialize its tree.
        for i in 0..POLLS {
            tokio::time::sleep(POLL).await;
            if i == 0 {
                after_scroll = position(app, &reference, timeout).await;
            }
            let kids = app
                .call(
                    app.next_lane(),
                    Deadline(Instant::now() + timeout),
                    f,
                    ACCESSIBLE,
                    "GetChildren",
                    &(),
                )
                .await
                .ok()
                .and_then(|m| wire::parse_children(m.body().data().bytes()).ok())
                .map_or(0, |k| k.len());
            if kids > 0 {
                woke += 1;
                break;
            }
        }
        // The reference must still sit where our scroll put it.
        let now = position(app, &reference, timeout).await;
        if after_scroll.is_none() || now != after_scroll {
            still_ours = false;
            break 'frames;
        }
    }
    if scrolled > 0 && still_ours {
        for _ in 0..2 {
            let _ = app
                .call(
                    app.next_lane(),
                    Deadline(Instant::now() + timeout),
                    &reference,
                    COMPONENT,
                    "ScrollToPoint",
                    &(SCREEN, origin.0, origin.1),
                )
                .await;
            if position(app, &reference, timeout).await == Some(origin) {
                break;
            }
        }
    } else if scrolled > 0 {
        tracing::debug!("the viewport moved during the frame wake: not restored");
    }
    tracing::debug!(
        frames = plan.frames.len(),
        woke,
        "offscreen frames scrolled into view"
    );
}
