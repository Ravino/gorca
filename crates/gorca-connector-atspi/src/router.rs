//! Sender -> application routing for the event drain.
//!
//! The drain asks [`AtspiRouter::route`] once per distinct bus name per connection, on its own
//! thread (`gorca_dbus::drain::Router`). The first event of an unknown application announces it
//! to the core through the ordinary `AppRegistrar`, with a short blocking handshake: that cost
//! is paid once per application, and the contract allows a brief block. Everything after that
//! is a hash lookup in the drain's own cache.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use gorca_connector_api::{AppPort, AppRegistrar, RegistrarError, ScopeKey};
use gorca_dbus::drain::{AppRoute, Router};
use gorca_model::{AppId, AppInfo, ConnectorKind, NodeKey};
use gorca_rt::{CancellationToken, Clock, Deadline, mpsc};
use tokio::runtime::Handle;

use crate::paths::PathTable;

/// How long the drain thread may wait for the core to accept a new application. Only a stalled
/// core ever takes more than a few hundred microseconds.
pub const ANNOUNCE_WAIT: Duration = Duration::from_millis(250);

/// A freshly announced application, handed from the drain thread to the connector task, which
/// owns the task that serves its requests.
#[derive(Debug)]
pub struct NewApp {
    pub port: AppPort,
    pub bus_name: Arc<str>,
    /// Cancelled when the application's bus name vanishes.
    pub gone: CancellationToken,
}

#[derive(Debug)]
struct Entry {
    route: AppRoute,
    gone: CancellationToken,
}

#[derive(Debug, Default)]
struct State {
    apps: HashMap<Arc<str>, Entry>,
    ignored: HashSet<Arc<str>>,
}

/// Counters, for tests and diagnostics.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq)]
pub struct RouterStats {
    pub announced: u64,
    pub announce_failed: u64,
    pub ignored: u64,
}

#[derive(Debug)]
pub struct AtspiRouter {
    handle: Handle,
    registrar: AppRegistrar,
    clock: Clock,
    paths: Arc<PathTable>,
    new_apps: mpsc::Sender<NewApp>,
    state: Mutex<State>,
    stats: Mutex<RouterStats>,
}

impl AtspiRouter {
    pub fn new(
        handle: Handle,
        registrar: AppRegistrar,
        clock: Clock,
        paths: Arc<PathTable>,
        new_apps: mpsc::Sender<NewApp>,
    ) -> Self {
        Self {
            handle,
            registrar,
            clock,
            paths,
            new_apps,
            state: Mutex::new(State::default()),
            stats: Mutex::new(RouterStats::default()),
        }
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn bump(&self, f: impl FnOnce(&mut RouterStats)) {
        f(&mut self.stats.lock().unwrap_or_else(PoisonError::into_inner));
    }

    pub fn stats(&self) -> RouterStats {
        *self.stats.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Bus names that emit AT-SPI signals but are not applications (the registry daemon, our
    /// own connections).
    pub fn ignore(&self, name: &str) {
        self.state().ignored.insert(Arc::from(name));
    }

    /// The application owning `name` left the bus. Returns its id so the caller can retire it.
    pub fn forget(&self, name: &str) -> Option<AppId> {
        let entry = self.state().apps.remove(name)?;
        entry.gone.cancel();
        self.paths.forget_app(entry.route.app);
        Some(entry.route.app)
    }

    /// Forget every application (connector shutdown or restart) and return their ids, so the
    /// caller can retire them in the core.
    pub fn take_all(&self) -> Vec<AppId> {
        let apps: Vec<Entry> = self.state().apps.drain().map(|(_, e)| e).collect();
        apps.into_iter()
            .map(|e| {
                e.gone.cancel();
                self.paths.forget_app(e.route.app);
                e.route.app
            })
            .collect()
    }

    pub fn known_apps(&self) -> usize {
        self.state().apps.len()
    }

    fn announce(&self, sender: &str) -> Option<AppRoute> {
        let scope = ScopeKey(Arc::from(sender));
        let info = AppInfo {
            // Replaced by `AppLifecycle::Appeared` once the app root has answered.
            toolkit: Arc::from("unknown"),
            app_name: Arc::from(sender),
            pid: None,
            connector_kind: ConnectorKind::Atspi,
        };
        let deadline = Deadline::after(&self.clock, ANNOUNCE_WAIT);
        let port = match self
            .handle
            .block_on(self.registrar.announce(scope, info, deadline))
        {
            Ok(port) => port,
            Err(RegistrarError::Duplicate) => {
                // Lost a race with another drain connection: the other one has the entry.
                return self.state().apps.get(sender).map(|e| e.route.clone());
            }
            Err(e) => {
                tracing::warn!(sender, error = %e, "application announce failed");
                self.bump(|s| s.announce_failed += 1);
                return None;
            }
        };
        let route = AppRoute {
            app: port.app,
            tx: port.ingress_sender(),
        };
        let gone = CancellationToken::new();
        let bus_name: Arc<str> = Arc::from(sender);
        let app = port.app;
        let new = NewApp {
            port,
            bus_name: Arc::clone(&bus_name),
            gone: gone.clone(),
        };
        if self.new_apps.try_send(new).is_err() {
            tracing::warn!(sender, "connector is not accepting applications");
            let _ = self.registrar.retire(app);
            self.bump(|s| s.announce_failed += 1);
            return None;
        }
        tracing::debug!(sender, ?app, "application announced");
        self.bump(|s| s.announced += 1);
        self.state().apps.insert(
            bus_name,
            Entry {
                route: route.clone(),
                gone,
            },
        );
        Some(route)
    }
}

impl Router for AtspiRouter {
    fn route(&self, sender: &str) -> Option<AppRoute> {
        {
            let st = self.state();
            if st.ignored.contains(sender) {
                drop(st);
                self.bump(|s| s.ignored += 1);
                return None;
            }
            if let Some(e) = st.apps.get(sender) {
                return Some(e.route.clone());
            }
        }
        // Bus unique names start with ':'; a well-known name never emits signals.
        if !sender.starts_with(':') {
            return None;
        }
        self.announce(sender)
    }

    fn focus_seen(&self, _app: AppId, node: NodeKey, path: &str) {
        self.paths.insert(node, path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gorca_connector_api::{RegistrarRequest, ScopeRegistry, registrar_channel};
    use gorca_ingress::IngressConfig;
    use gorca_model::{ConnectorId, LocalId};

    /// A core stand-in on its own runtime thread: answers announcements from a `ScopeRegistry`.
    struct Core {
        rt: tokio::runtime::Runtime,
        registrar: AppRegistrar,
    }

    fn core(reachable: bool) -> Core {
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let (registrar, mut rx) = registrar_channel(ConnectorId(1), 8);
        if reachable {
            rt.spawn(async move {
                let mut table = ScopeRegistry::unbounded();
                while let Some(req) = rx.recv().await {
                    match req {
                        RegistrarRequest::Announce { scope, reply, .. } => {
                            let r = table
                                .register(ConnectorId(1), scope, IngressConfig::default())
                                .map(|c| {
                                    // Keep the core half alive for the test's duration.
                                    std::mem::forget((c.ingress, c.control, c.requests));
                                    c.port
                                });
                            let _ = reply.send(r);
                        }
                        RegistrarRequest::Retire { app } => table.retire(app),
                    }
                }
            });
        } else {
            rt.spawn(async move {
                let _keep = rx;
                std::future::pending::<()>().await;
            });
        }
        Core { rt, registrar }
    }

    fn router(c: &Core) -> (AtspiRouter, mpsc::Receiver<NewApp>) {
        let (tx, rx) = mpsc::channel(8);
        let r = AtspiRouter::new(
            c.rt.handle().clone(),
            c.registrar.clone(),
            Clock::System,
            Arc::new(PathTable::new()),
            tx,
        );
        (r, rx)
    }

    #[test]
    fn first_event_announces_once_and_hands_the_port_over() {
        let c = core(true);
        let (r, mut new_apps) = router(&c);
        let a = r.route(":1.42").expect("announced");
        let b = r.route(":1.42").expect("cached");
        assert_eq!(a.app, b.app);
        assert_eq!(r.stats().announced, 1);
        let new = new_apps.try_recv().expect("port handed to the connector");
        assert_eq!(&*new.bus_name, ":1.42");
        assert_eq!(new.port.app, a.app);
        assert!(new_apps.try_recv().is_err(), "exactly one hand-over");
    }

    #[test]
    fn ignored_and_well_known_names_are_not_applications() {
        let c = core(true);
        let (r, _rx) = router(&c);
        r.ignore(":1.3");
        assert!(r.route(":1.3").is_none());
        assert!(r.route("org.example.Name").is_none());
        assert_eq!(r.known_apps(), 0);
        assert_eq!(r.stats().ignored, 1);
    }

    #[test]
    fn a_stalled_core_costs_the_drain_a_bounded_wait() {
        let c = core(false);
        let (r, _rx) = router(&c);
        let t = std::time::Instant::now();
        assert!(r.route(":1.9").is_none());
        let waited = t.elapsed();
        assert!(waited < ANNOUNCE_WAIT * 4, "{waited:?}");
        assert_eq!(r.stats().announce_failed, 1);
    }

    #[test]
    fn take_all_returns_every_app_once() {
        let c = core(true);
        let (r, mut new_apps) = router(&c);
        let a = r.route(":1.1").unwrap().app;
        let b = r.route(":1.2").unwrap().app;
        let mut all = r.take_all();
        all.sort();
        let mut want = vec![a, b];
        want.sort();
        assert_eq!(all, want);
        assert_eq!(r.known_apps(), 0);
        assert!(new_apps.try_recv().unwrap().gone.is_cancelled());
    }

    #[test]
    fn forget_cancels_the_app_and_drops_its_paths() {
        let c = core(true);
        let (r, mut new_apps) = router(&c);
        let route = r.route(":1.5").expect("announced");
        let node = NodeKey {
            app: route.app,
            local: LocalId(1),
        };
        r.focus_seen(route.app, node, "/x/1");
        assert!(r.paths.get(node).is_some());
        let new = new_apps.try_recv().unwrap();
        assert!(!new.gone.is_cancelled());
        assert_eq!(r.forget(":1.5"), Some(route.app));
        assert!(new.gone.is_cancelled());
        assert!(r.paths.get(node).is_none());
        assert_eq!(r.forget(":1.5"), None);
    }
}
