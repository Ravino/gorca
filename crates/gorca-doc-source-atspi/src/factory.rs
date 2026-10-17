//! The AT-SPI [`DocSourceFactory`]: everything the core must not know about a browser's AT-SPI
//! surface (browse-mode design section 6, ADR-0003 AM-10).
//!
//! * `is_web` / `needs_warm`: by the toolkit and application name the connector reported (Gecko
//!   serves AT-SPI from a lazily populated parent-process cache, so it needs a warm-up);
//! * `warm`: activate the cache domains once per application ([`crate::warm`]);
//! * `locate`: walk `Accessible.Parent` from a focused node to the top-level `DOCUMENT_WEB`;
//! * `create`: an [`AtspiDocSource`] over the application's peer-to-peer connections.
//!
//! The factory never reads the environment (the runtime directory comes from the caller) and
//! never blocks: every call carries a deadline.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use gorca_doc_source_api::{DocSource, DocSourceFactory, SourceError, WarmOutcome, WarmReport};
use gorca_model::{AppId, AppInfo, NodeKey, TextUnit, WebEngine};
use gorca_rt::{BoxFuture, Deadline, mpsc};

use crate::p2p::{AppConn, ConnectOpts};
use crate::roles::raw;
use crate::source::{AtspiDocSource, SourceConfig};
use crate::warm::warm_domains;
use crate::wire::{self, AttrInterner};

const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
const ROOT: &str = "/org/a11y/atspi/accessible/root";
/// Hops of the parent walk (a deeply nested page plus a few frames).
const MAX_HOPS: usize = 96;
/// Calls and depth of the search for any document of an application.
const FIND_CALLS: usize = 64;
const FIND_DEPTH: usize = 6;
const STEP: Duration = Duration::from_millis(400);

/// How the factory turns the core's keys into bus names and object paths. Implemented by the
/// daemon over the connector's tables; the core never sees paths.
pub trait PathResolver: Send + Sync + 'static {
    /// The unique bus name of the application.
    fn bus_of(&self, app: AppId) -> Option<Arc<str>>;
    /// The object path of a node the connector has seen.
    fn path_of(&self, key: NodeKey) -> Option<Arc<str>>;
    /// The a11y bus address, once the connector resolved it.
    fn a11y_address(&self) -> Option<String>;
}

struct AppEntry {
    conn: Arc<AppConn>,
    interner: Arc<AttrInterner>,
}

/// Creates AT-SPI document sources. Cloning shares the connections.
#[derive(Clone)]
pub struct AtspiDocSourceFactory {
    inner: Arc<Inner>,
}

struct Inner {
    resolver: Arc<dyn PathResolver>,
    runtime_dir: PathBuf,
    bus: tokio::sync::OnceCell<zbus::Connection>,
    apps: tokio::sync::Mutex<HashMap<AppId, Arc<AppEntry>>>,
    /// Paths of document roots the factory located (the connector may not know them).
    roots: Mutex<HashMap<NodeKey, String>>,
}

impl std::fmt::Debug for AtspiDocSourceFactory {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AtspiDocSourceFactory")
            .finish_non_exhaustive()
    }
}

fn dl(d: Duration) -> Deadline {
    Deadline(Instant::now() + d)
}

fn is_gecko(info: &AppInfo) -> bool {
    info.web_engine == WebEngine::Gecko
}

fn is_blink(info: &AppInfo) -> bool {
    info.web_engine == WebEngine::Blink
}

/// `Properties.Get(Accessible, "Parent")`: `(bus, path)` of the parent reference.
async fn parent_of(conn: &AppConn, path: &str) -> Option<(String, String)> {
    let m = conn
        .call(
            conn.next_lane(),
            dl(STEP),
            path,
            PROPERTIES,
            "Get",
            &(ACCESSIBLE, "Parent"),
        )
        .await
        .ok()?;
    wire::parse_variant_ref(m.body().data().bytes()).ok()
}

async fn role_of(conn: &AppConn, path: &str) -> Option<u32> {
    let m = conn
        .call(conn.next_lane(), dl(STEP), path, ACCESSIBLE, "GetRole", &())
        .await
        .ok()?;
    wire::parse_u32(m.body().data().bytes()).ok()
}

/// Walk up from `path`; the topmost `DOCUMENT_WEB` on the way is the top-level document.
pub async fn top_document(conn: &AppConn, path: &str) -> Option<String> {
    let mut cur = path.to_owned();
    let mut top: Option<String> = None;
    for _ in 0..MAX_HOPS {
        let role = role_of(conn, &cur).await?;
        if role == raw::DOCUMENT_WEB {
            top = Some(cur.clone());
        }
        if role == raw::APPLICATION {
            break;
        }
        let Some((_, parent)) = parent_of(conn, &cur).await else {
            break;
        };
        if parent.is_empty() || parent.ends_with("/null") || parent == cur {
            break;
        }
        cur = parent;
    }
    top
}

/// Breadth-first search from the application root for any web document.
pub async fn any_document(conn: &AppConn) -> Option<String> {
    let mut queue: std::collections::VecDeque<(String, usize)> = std::collections::VecDeque::new();
    queue.push_back((ROOT.to_owned(), 0));
    let mut calls = 0usize;
    while let Some((p, depth)) = queue.pop_front() {
        if calls >= FIND_CALLS {
            return None;
        }
        calls += 1;
        if role_of(conn, &p).await == Some(raw::DOCUMENT_WEB) {
            return Some(p);
        }
        if depth >= FIND_DEPTH {
            continue;
        }
        calls += 1;
        let Ok(m) = conn
            .call(
                conn.next_lane(),
                dl(STEP),
                &p,
                ACCESSIBLE,
                "GetChildren",
                &(),
            )
            .await
        else {
            continue;
        };
        if let Ok(kids) = wire::parse_children(m.body().data().bytes()) {
            for (_, kid) in kids {
                queue.push_back((kid, depth + 1));
            }
        }
    }
    None
}

impl AtspiDocSourceFactory {
    /// `runtime_dir` is the directory application sockets must live under (the caller resolves
    /// it; library crates never read the environment).
    pub fn new(resolver: Arc<dyn PathResolver>, runtime_dir: PathBuf) -> Self {
        Self {
            inner: Arc::new(Inner {
                resolver,
                runtime_dir,
                bus: tokio::sync::OnceCell::new(),
                apps: tokio::sync::Mutex::new(HashMap::new()),
                roots: Mutex::new(HashMap::new()),
            }),
        }
    }

    /// Forget an application (it went away): its connections close.
    pub async fn forget(&self, app: AppId) {
        self.inner.apps.lock().await.remove(&app);
        self.inner
            .roots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .retain(|k, _| k.app != app);
    }
}

impl Inner {
    async fn bus(&self) -> Result<&zbus::Connection, SourceError> {
        self.bus
            .get_or_try_init(|| async {
                let addr = self
                    .resolver
                    .a11y_address()
                    .ok_or_else(|| SourceError::Protocol("a11y bus address unknown".into()))?;
                let builder = zbus::connection::Builder::address(addr.as_str())
                    .map_err(|e| SourceError::Protocol(e.to_string()))?;
                gorca_dbus::with_deadline(dl(Duration::from_secs(2)), builder.build())
                    .await
                    .map_err(|e| SourceError::Protocol(e.to_string()))
            })
            .await
    }

    async fn entry(&self, app: AppId) -> Result<Arc<AppEntry>, SourceError> {
        if let Some(e) = self.apps.lock().await.get(&app) {
            return Ok(Arc::clone(e));
        }
        let name = self.resolver.bus_of(app).ok_or(SourceError::Detached)?;
        let bus = self.bus().await?;
        let conn = AppConn::connect(bus, &name, &ConnectOpts::new(self.runtime_dir.clone()))
            .await
            .map_err(|e| SourceError::Protocol(e.to_string()))?;
        let entry = Arc::new(AppEntry {
            conn: Arc::new(conn),
            interner: Arc::new(AttrInterner::default()),
        });
        self.apps.lock().await.insert(app, Arc::clone(&entry));
        Ok(entry)
    }

    fn remember_root(&self, key: NodeKey, path: &str) {
        self.roots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, path.to_owned());
    }

    fn root_path(&self, key: NodeKey) -> Option<String> {
        if let Some(p) = self
            .roots
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
        {
            return Some(p.clone());
        }
        self.resolver.path_of(key).map(|p| p.to_string())
    }
}

impl DocSourceFactory for AtspiDocSourceFactory {
    fn is_web(&self, info: &AppInfo) -> bool {
        is_gecko(info) || is_blink(info)
    }

    fn needs_warm(&self, info: &AppInfo) -> bool {
        is_gecko(info)
    }

    fn warm(
        &self,
        app: AppId,
        _info: &AppInfo,
        hint: Option<NodeKey>,
    ) -> BoxFuture<'static, Result<WarmOutcome, SourceError>> {
        let me = Arc::clone(&self.inner);
        Box::pin(async move {
            let entry = me.entry(app).await?;
            let doc = match hint.and_then(|h| me.resolver.path_of(h)) {
                Some(p) => top_document(&entry.conn, &p).await,
                None => any_document(&entry.conn).await,
            };
            let Some(doc) = doc else {
                return Ok(WarmOutcome::NoDocument);
            };
            let r = warm_domains(
                &entry.conn,
                &entry.interner,
                &doc,
                Duration::from_millis(2000),
            )
            .await;
            Ok(WarmOutcome::Done(WarmReport {
                ready: r.ready,
                waited: r.waited,
                probes: r.probes,
                blocked: r.blocked,
                slept: r.slept,
                rounds: r.rounds,
                sampled_nodes: r.sampled_nodes,
                tagged_nodes: r.tagged_nodes,
            }))
        })
    }

    fn locate(
        &self,
        app: AppId,
        _info: &AppInfo,
        focus: NodeKey,
    ) -> BoxFuture<'static, Result<Option<NodeKey>, SourceError>> {
        let me = Arc::clone(&self.inner);
        Box::pin(async move {
            let Some(path) = me.resolver.path_of(focus) else {
                return Ok(None);
            };
            let entry = me.entry(app).await?;
            let Some(top) = top_document(&entry.conn, &path).await else {
                return Ok(None);
            };
            let key = NodeKey {
                app,
                local: gorca_dbus::drain::local_id(&top),
            };
            me.remember_root(key, &top);
            Ok(Some(key))
        })
    }

    fn create(
        &self,
        app: AppId,
        info: &AppInfo,
        root: NodeKey,
    ) -> BoxFuture<'static, Result<Box<dyn DocSource>, SourceError>> {
        let me = Arc::clone(&self.inner);
        let gecko = is_gecko(info);
        let blink = is_blink(info);
        Box::pin(async move {
            let entry = me.entry(app).await?;
            let path = me.root_path(root).ok_or(SourceError::NotSupported)?;
            let unit = if gecko {
                TextUnit::Utf16Bom
            } else {
                TextUnit::Scalar
            };
            let (_closed, events) = mpsc::channel(1);
            let mut config = SourceConfig::browse(unit, blink);
            config.fetch.quirks = if gecko {
                crate::extras::WebQuirks::GECKO
            } else if blink {
                crate::extras::WebQuirks::BLINK
            } else {
                crate::extras::WebQuirks::NONE
            };
            let (source, _metrics) =
                AtspiDocSource::new(Arc::clone(&entry.conn), path, config, events);
            let source = source.with_interner(Arc::clone(&entry.interner));
            Ok(Box::new(source) as Box<dyn DocSource>)
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use gorca_model::ConnectorKind;

    use super::*;

    /// What the connector hands over: the engine from its toolkit table.
    fn info(toolkit: &str, name: &str) -> AppInfo {
        AppInfo {
            toolkit: Arc::from(toolkit),
            app_name: Arc::from(name),
            pid: None,
            connector_kind: ConnectorKind::Atspi,
            verified_shell: false,
            dump_complete: false,
            text_unit: gorca_model::TextUnit::Scalar,
            text_insert_slack: 0,
            edit_quirks: gorca_model::EditQuirks::default(),
            page_events_settle_ms: 0,
            web_engine: gorca_model::WebEngine::None,
            root: None,
        }
    }

    fn with_engine(mut i: AppInfo, e: WebEngine) -> AppInfo {
        i.web_engine = e;
        i
    }

    #[test]
    fn the_web_engine_comes_from_the_connectors_row_not_from_the_name() {
        let g = with_engine(info("Gecko", "Firefox"), WebEngine::Gecko);
        let c = with_engine(info("Chromium", "Google Chrome"), WebEngine::Blink);
        let n = info("GTK 4.14", "Settings");
        assert!(is_gecko(&g) && !is_blink(&g));
        assert!(is_blink(&c) && !is_gecko(&c));
        assert!(!is_gecko(&n) && !is_blink(&n));
        // A name alone says nothing: the connector's classification is the one source.
        assert!(!is_blink(&info("gtk", "chromium")));
    }
}
