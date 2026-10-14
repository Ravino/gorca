//! Find the browser application and the test page's `document web` on a private a11y bus.

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use gorca_dbus::call_method;
use gorca_doc_source_atspi::roles::{raw, state_bit};
use gorca_doc_source_atspi::wire;
use gorca_doc_source_atspi::{AppConn, ConnectOpts};
use gorca_rt::Deadline;

const ACC: &str = "org.a11y.atspi.Accessible";
const PROPS: &str = "org.freedesktop.DBus.Properties";

pub async fn connect_bus(addr: &str) -> Result<zbus::Connection> {
    let b = zbus::connection::Builder::address(addr).context("a11y bus address")?;
    Ok(tokio::time::timeout(Duration::from_secs(5), b.build())
        .await
        .context("connecting to the a11y bus timed out")??)
}

fn dl(ms: u64) -> Deadline {
    Deadline(Instant::now() + Duration::from_millis(ms))
}

/// `(bus name, path)` of every application registered with the registry.
pub async fn list_apps(bus: &zbus::Connection) -> Result<Vec<(String, String)>> {
    let m = call_method(
        bus,
        dl(3000),
        "org.a11y.atspi.Registry",
        "/org/a11y/atspi/accessible/root",
        ACC,
        "GetChildren",
        &(),
    )
    .await?;
    Ok(wire::parse_children(m.body().data().bytes())?)
}

async fn bus_name_prop(bus: &zbus::Connection, dest: &str, path: &str) -> Option<String> {
    let m = call_method(bus, dl(2000), dest, path, PROPS, "Get", &(ACC, "Name"))
        .await
        .ok()?;
    wire::parse_variant_string(m.body().data().bytes())
        .ok()
        .map(str::to_owned)
}

/// Wait for an application whose name contains `needle` to register.
pub async fn wait_for_app(
    bus: &zbus::Connection,
    needle: &str,
    timeout: Duration,
) -> Result<(String, String)> {
    let end = Instant::now() + timeout;
    loop {
        for (dest, path) in list_apps(bus).await.unwrap_or_default() {
            if let Some(n) = bus_name_prop(bus, &dest, &path).await
                && n.to_lowercase().contains(&needle.to_lowercase())
            {
                return Ok((dest, path));
            }
        }
        if Instant::now() > end {
            bail!("no application named like {needle:?} on the bus within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

pub struct DocRef {
    pub path: String,
    pub name: String,
    pub showing: bool,
}

async fn call_p2p(conn: &AppConn, path: &str, member: &str) -> Option<zbus::Message> {
    conn.call(conn.next_lane(), dl(2000), path, ACC, member, &())
        .await
        .ok()
}

/// Breadth-first search for `document web` objects under the app root, limited in depth and size.
pub async fn find_documents(conn: &AppConn, root: &str) -> Vec<DocRef> {
    let mut out = Vec::new();
    let mut queue = std::collections::VecDeque::from([(root.to_owned(), 0u32)]);
    let mut seen = 0u32;
    while let Some((p, depth)) = queue.pop_front() {
        seen += 1;
        if seen > 4000 {
            break;
        }
        let Some(role) = call_p2p(conn, &p, "GetRole")
            .await
            .and_then(|m| wire::parse_u32(m.body().data().bytes()).ok())
        else {
            continue;
        };
        if role == raw::DOCUMENT_WEB {
            let name = conn
                .call(conn.next_lane(), dl(2000), &p, PROPS, "Get", &(ACC, "Name"))
                .await
                .ok()
                .and_then(|m| {
                    wire::parse_variant_string(m.body().data().bytes())
                        .ok()
                        .map(str::to_owned)
                })
                .unwrap_or_default();
            let st = call_p2p(conn, &p, "GetState")
                .await
                .and_then(|m| wire::parse_state(m.body().data().bytes()).ok())
                .unwrap_or(0);
            out.push(DocRef {
                path: p,
                name,
                showing: st & (1u64 << state_bit::SHOWING) != 0,
            });
            continue;
        }
        if depth >= 8 {
            continue;
        }
        if let Some(m) = call_p2p(conn, &p, "GetChildren").await
            && let Ok(kids) = wire::parse_children(m.body().data().bytes())
        {
            for (_, c) in kids {
                queue.push_back((c, depth + 1));
            }
        }
    }
    out
}

/// Wait until a `document web` with a title containing `title` exists.
pub async fn wait_for_document(
    conn: &Arc<AppConn>,
    root: &str,
    title: &str,
    timeout: Duration,
) -> Result<DocRef> {
    let end = Instant::now() + timeout;
    loop {
        let docs = find_documents(conn, root).await;
        if let Some(d) = docs.into_iter().find(|d| d.name.contains(title)) {
            return Ok(d);
        }
        if Instant::now() > end {
            bail!("no document web titled like {title:?} within {timeout:?}");
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
}

pub async fn open_app(
    bus: &zbus::Connection,
    bus_name: &str,
    connections: usize,
) -> Result<Arc<AppConn>> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from);
    let mut opts = ConnectOpts::new(runtime.unwrap_or_default());
    opts.connections = connections;
    Ok(Arc::new(AppConn::connect(bus, bus_name, &opts).await?))
}
