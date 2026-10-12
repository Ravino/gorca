//! Reference arm (a): zbus `MessageStream` on a dedicated thread, one `push` per mapped event.

use std::collections::HashMap;
use std::os::unix::net::UnixStream as StdUnixStream;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Instant;

use futures_lite::StreamExt;
use gorca_rt::Deadline;
use smallvec::SmallVec;
use tokio::sync::Notify;
use zbus::message::{EndianSig, Type};

use super::{
    AppRoute, DrainConfig, DrainEnd, DrainError, DrainHandle, DrainShared, Router, SignalView,
    map_signal,
};
use crate::{call_method, set_recv_buffer, socket_backlog};

/// Open the drain: connect, authenticate, `Hello`, `AddMatch`, then read on a dedicated thread.
/// Returns once the match rules are installed or the deadline expired.
pub fn spawn_drain(
    cfg: DrainConfig,
    router: Arc<dyn Router>,
) -> Result<DrainHandle, DrainError> {
    let shared = DrainShared::new();
    let stop = Arc::new(Notify::new());
    let (ready_tx, ready_rx) = std::sync::mpsc::channel();
    let wait = cfg.connect_deadline + std::time::Duration::from_secs(1);
    let thread = {
        let (shared, stop) = (Arc::clone(&shared), Arc::clone(&stop));
        std::thread::Builder::new()
            .name("gorca-drain-0".to_owned())
            .spawn(move || run(&cfg, &router, &shared, &stop, &ready_tx))?
    };
    let wake: Box<dyn Fn() + Send + Sync> = {
        let stop = Arc::clone(&stop);
        Box::new(move || stop.notify_one())
    };
    let handle = DrainHandle::new(Arc::clone(&shared), wake, vec![thread]);
    match ready_rx.recv_timeout(wait) {
        Ok(Ok(())) => Ok(handle),
        Ok(Err(e)) => {
            handle.stop();
            Err(e)
        }
        Err(_) => {
            handle.stop();
            Err(DrainError::Deadline)
        }
    }
}

fn unix_path(address: &str) -> Option<&str> {
    let rest = address.strip_prefix("unix:")?;
    rest.split(',').find_map(|kv| kv.strip_prefix("path="))
}

fn run(
    cfg: &DrainConfig,
    router: &Arc<dyn Router>,
    shared: &Arc<DrainShared>,
    stop: &Arc<Notify>,
    ready: &std::sync::mpsc::Sender<Result<(), DrainError>>,
) {
    let rt = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            let _ = ready.send(Err(DrainError::Spawn(e)));
            return;
        }
    };
    rt.block_on(async {
        let setup = tokio::time::timeout(cfg.connect_deadline, connect(cfg)).await;
        let (conn, probe) = match setup {
            Ok(Ok(x)) => x,
            Ok(Err(e)) => {
                let _ = ready.send(Err(e));
                return;
            }
            Err(_) => {
                let _ = ready.send(Err(DrainError::Deadline));
                return;
            }
        };
        let _ = ready.send(Ok(()));
        read_loop(cfg, router, shared, stop, &conn, probe.as_ref()).await;
    });
}

async fn connect(cfg: &DrainConfig) -> Result<(zbus::Connection, Option<StdUnixStream>), DrainError> {
    let err = |e: &dyn std::fmt::Display| DrainError::Connect(e.to_string());
    let (builder, probe) = if let Some(path) = unix_path(&cfg.address) {
        let std_stream = StdUnixStream::connect(path).map_err(|e| err(&e))?;
        if let Some(bytes) = cfg.rcvbuf_bytes {
            let _ = set_recv_buffer(&std_stream, bytes);
        }
        std_stream.set_nonblocking(true).map_err(|e| err(&e))?;
        let probe = std_stream.try_clone().map_err(|e| err(&e))?;
        let tokio_stream = tokio::net::UnixStream::from_std(std_stream).map_err(|e| err(&e))?;
        (
            zbus::connection::Builder::unix_stream(tokio_stream),
            Some(probe),
        )
    } else {
        (
            zbus::connection::Builder::address(cfg.address.as_str()).map_err(|e| err(&e))?,
            None,
        )
    };
    let conn = builder
        .max_queued(crate::MAX_QUEUED)
        .build()
        .await
        .map_err(|e| err(&e))?;
    let deadline = Deadline(Instant::now() + cfg.connect_deadline);
    for rule in &cfg.match_rules {
        call_method(
            &conn,
            deadline,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "AddMatch",
            &rule.as_str(),
        )
        .await
        .map_err(|e| err(&e))?;
    }
    Ok((conn, probe))
}

async fn read_loop(
    cfg: &DrainConfig,
    router: &Arc<dyn Router>,
    shared: &Arc<DrainShared>,
    stop: &Arc<Notify>,
    conn: &zbus::Connection,
    probe: Option<&StdUnixStream>,
) {
    let c = &shared.counters;
    let mut stream = zbus::MessageStream::from(conn);
    let mut routes: HashMap<String, Option<AppRoute>> = HashMap::new();
    let mut mapped = SmallVec::<[gorca_model::NodeEvent; 2]>::new();
    let mut shedding = false;
    let mut tick = 0u32;
    loop {
        let next = tokio::select! {
            biased;
            () = stop.notified() => return,
            m = stream.next() => m,
        };
        let msg = match next {
            Some(Ok(m)) => m,
            Some(Err(e)) => {
                shared.set_end(DrainEnd::Disconnected(e.to_string()));
                return;
            }
            None => {
                shared.set_end(DrainEnd::Disconnected("end of stream".to_owned()));
                return;
            }
        };
        c.messages.fetch_add(1, Ordering::Relaxed);
        c.batches.fetch_add(1, Ordering::Relaxed);
        c.bytes.fetch_add(msg.data().len() as u64, Ordering::Relaxed);
        tick = tick.wrapping_add(1);
        if tick.is_multiple_of(64)
            && let Some(sock) = probe
            && let Ok(backlog) = socket_backlog(sock)
        {
            shared.raise_backlog(backlog);
            if !shedding && backlog >= cfg.shed_watermark {
                shedding = true;
                c.shed_starts.fetch_add(1, Ordering::Relaxed);
            } else if shedding && backlog < cfg.shed_watermark / 2 {
                shedding = false;
            }
        }
        let h = msg.header();
        if h.message_type() != Type::Signal {
            c.ignored.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        let (Some(sender), Some(path), Some(iface), Some(member)) =
            (h.sender(), h.path(), h.interface(), h.member())
        else {
            c.ignored.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        if !routes.contains_key(sender.as_str()) {
            routes.insert(sender.as_str().to_owned(), router.route(sender.as_str()));
        }
        let Some(Some(route)) = routes.get(sender.as_str()) else {
            c.unrouted.fetch_add(1, Ordering::Relaxed);
            continue;
        };
        let body = msg.body();
        let view = SignalView {
            interface: iface.as_str(),
            member: member.as_str(),
            path: path.as_str(),
            body: body.data().bytes(),
            big_endian: h.primary().endian_sig() == EndianSig::Big,
        };
        mapped.clear();
        if map_signal(route.app, &view, &mut mapped).is_err() {
            c.malformed.fetch_add(1, Ordering::Relaxed);
            continue;
        }
        if mapped.is_empty() {
            c.ignored.fetch_add(1, Ordering::Relaxed);
        }
        for ev in mapped.drain(..) {
            route.tx.push(ev);
            c.pushed.fetch_add(1, Ordering::Relaxed);
        }
    }
}
