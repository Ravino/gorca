use std::sync::atomic::{AtomicU64, Ordering};

use gorca_connector_api::{RegistrarRequest, ScopeRegistry, registrar_channel};
use gorca_ingress::{IngressConfig, IngressReceiver, SlotBudget};
use gorca_model::{ConnectorId, LocalId};

use super::*;

/// A core stand-in on the test runtime: charges a real budget, keeps each app's core half until
/// it is retired (like the real app actor) and answers announcements after a per-call delay.
struct Core {
    registrar: AppRegistrar,
    budget: Arc<SlotBudget>,
    table: Arc<Mutex<ScopeRegistry>>,
    halves: Arc<Mutex<HashMap<AppId, IngressReceiver>>>,
}

fn core(delay: impl Fn(u64) -> Duration + Send + 'static) -> Core {
    let (registrar, mut rx) = registrar_channel(ConnectorId(1), 256);
    let budget = SlotBudget::new(100_000);
    let table = Arc::new(Mutex::new(ScopeRegistry::new(Arc::clone(&budget))));
    let halves: Arc<Mutex<HashMap<AppId, IngressReceiver>>> = Arc::default();
    let (t, h) = (Arc::clone(&table), Arc::clone(&halves));
    tokio::spawn(async move {
        let n = Arc::new(AtomicU64::new(0));
        while let Some(req) = rx.recv().await {
            match req {
                RegistrarRequest::Announce { scope, reply, .. } => {
                    let (t, h) = (Arc::clone(&t), Arc::clone(&h));
                    let d = delay(n.fetch_add(1, Ordering::Relaxed));
                    // Each announcement is served concurrently, like independent requests.
                    tokio::spawn(async move {
                        tokio::time::sleep(d).await;
                        let r = t.lock().unwrap().register(
                            ConnectorId(1),
                            scope,
                            IngressConfig::default(),
                        );
                        let r = r.map(|c| {
                            let app = c.port.app;
                            // Keep the core half (control and request ends) alive with the
                            // ingress, as the app actor would.
                            std::mem::forget((c.control, c.requests));
                            h.lock().unwrap().insert(app, c.ingress);
                            c.port
                        });
                        let _ = reply.send(r);
                    });
                }
                RegistrarRequest::Retire { app } => {
                    t.lock().unwrap().retire(app);
                    h.lock().unwrap().remove(&app);
                }
            }
        }
    });
    Core {
        registrar,
        budget,
        table,
        halves,
    }
}

struct Fixed(u32);

impl Credentials for Fixed {
    fn pid_of<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, Option<u32>> {
        let pid = self.0;
        Box::pin(async move { Some(pid) })
    }
}

struct Rig {
    router: Arc<AtspiRouter>,
    new_apps: mpsc::Receiver<NewApp>,
    to_register: mpsc::Receiver<Arc<str>>,
    paths: Arc<PathTable>,
}

fn rig(c: &Core, credentials: Arc<dyn Credentials>) -> Rig {
    let (new_tx, new_apps) = mpsc::channel(1024);
    let (reg_tx, to_register) = mpsc::channel(1024);
    let paths = Arc::new(PathTable::new());
    let router = Arc::new(AtspiRouter::new(
        c.registrar.clone(),
        Clock::System,
        Arc::clone(&paths),
        new_tx,
        reg_tx,
        credentials,
    ));
    Rig {
        router,
        new_apps,
        to_register,
        paths,
    }
}

fn focus_signal(path: &str) -> SignalView<'_> {
    SignalView {
        interface: "org.a11y.atspi.Event.Focus",
        member: "Focus",
        path,
        body: &[],
        big_endian: false,
    }
}

fn is_retry(d: &RouteDecision) -> bool {
    matches!(d, RouteDecision::Retry)
}

async fn eventually(what: &str, cond: impl Fn() -> bool) {
    let end = std::time::Instant::now() + Duration::from_secs(10);
    while !cond() {
        assert!(std::time::Instant::now() < end, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

#[tokio::test]
async fn the_first_event_registers_asynchronously_and_is_replayed_in_order() {
    let c = core(|_| Duration::from_millis(20));
    let mut r = rig(&c, Arc::new(Fixed(4242)));
    // The drain never waits: the answer is immediate even though the core takes 20 ms.
    let t = std::time::Instant::now();
    assert!(is_retry(&r.router.resolve(":1.42")));
    assert!(t.elapsed() < Duration::from_millis(10));
    // Two more events while the registration runs: parked, not lost.
    r.router.park(":1.42", &focus_signal("/org/a/1"));
    r.router.park(":1.42", &focus_signal("/org/a/2"));
    assert_eq!(r.router.pending_registrations(), 1);
    // Exactly one registration was requested.
    let name = r.to_register.try_recv().expect("registration requested");
    assert!(r.to_register.try_recv().is_err());
    assert!(is_retry(&r.router.resolve(":1.42")), "still pending");
    assert!(r.to_register.try_recv().is_err(), "no second request");

    r.router.complete(name).await;
    let RouteDecision::Route(route) = r.router.resolve(":1.42") else {
        panic!("registered");
    };
    let new = r.new_apps.try_recv().expect("port handed to the connector");
    assert_eq!(&*new.bus_name, ":1.42");
    assert_eq!(new.port.app, route.app);
    // The parked events were pushed into the ingress, and the paths recorded for fetching.
    let rx = c.halves.lock().unwrap().remove(&route.app).unwrap();
    assert_eq!(rx.pending(), 1, "two focus events coalesce to the last one");
    let drained = rx.drain();
    let [NodeEvent::FocusChanged { node }] = drained.events.as_slice() else {
        panic!("expected one FocusChanged, got {:?}", drained.events);
    };
    assert_eq!(r.paths.get(*node).as_deref(), Some("/org/a/2"));
    assert_eq!(r.router.stats().parked_replayed, 2);
}

#[tokio::test]
async fn ignored_and_well_known_names_are_not_applications() {
    let c = core(|_| Duration::ZERO);
    let r = rig(&c, Arc::new(NoCredentials));
    r.router.ignore(":1.3");
    assert!(matches!(r.router.resolve(":1.3"), RouteDecision::NotAnApp));
    assert!(matches!(
        r.router.resolve("org.example.Name"),
        RouteDecision::NotAnApp
    ));
    assert_eq!(r.router.known_apps(), 0);
}

/// Regression (review F4): a failed registration backs off, then tries again. It never mutes
/// the sender for good.
#[tokio::test]
async fn a_failed_registration_backs_off_and_then_succeeds() {
    // A core that is gone: announce fails at once with `Closed`.
    let (registrar, rx) = registrar_channel(ConnectorId(1), 8);
    drop(rx);
    let (new_tx, _new_apps) = mpsc::channel(8);
    let (reg_tx, mut to_register) = mpsc::channel(8);
    let router = AtspiRouter::new(
        registrar,
        Clock::System,
        Arc::new(PathTable::new()),
        new_tx,
        reg_tx,
        Arc::new(NoCredentials),
    );
    assert!(is_retry(&router.resolve(":1.8")));
    router.complete(to_register.try_recv().unwrap()).await;
    assert_eq!(router.stats().announce_failed, 1);
    // Inside the backoff window the drain only gets `Retry`; no new registration starts.
    assert!(is_retry(&router.resolve(":1.8")));
    assert!(to_register.try_recv().is_err());
    // After it, the next event starts a new attempt.
    tokio::time::sleep(BACKOFF_MIN * 3).await;
    assert!(is_retry(&router.resolve(":1.8")));
    assert!(
        to_register.try_recv().is_ok(),
        "a second attempt was started"
    );
}

#[tokio::test]
async fn registrations_in_flight_and_parked_events_are_bounded() {
    let c = core(|_| Duration::from_secs(60));
    let mut r = rig(&c, Arc::new(NoCredentials));
    for i in 0..(MAX_PENDING + 40) {
        assert!(is_retry(&r.router.resolve(&format!(":1.{i}"))));
    }
    let mut asked = 0;
    while r.to_register.try_recv().is_ok() {
        asked += 1;
    }
    assert_eq!(asked, MAX_PENDING);
    for _ in 0..(MAX_PARKED_PER_SENDER + 10) {
        r.router.park(":1.0", &focus_signal("/x"));
    }
    let big = vec![0u8; MAX_PARKED_BODY + 1];
    r.router.park(
        ":1.1",
        &SignalView {
            body: &big,
            ..focus_signal("/x")
        },
    );
    assert_eq!(r.router.stats().parked_dropped, 11);
}

/// Regression (security review): an application that leaves the bus while its registration is
/// in flight must leave no route, no registry entry, no core half and no budget behind.
#[tokio::test]
async fn an_app_dying_mid_registration_leaks_nothing() {
    let c = core(|_| Duration::from_millis(200));
    let mut r = rig(&c, Arc::new(NoCredentials));
    assert!(is_retry(&r.router.resolve(":1.77")));
    r.router.park(":1.77", &focus_signal("/x"));
    let name = r.to_register.try_recv().unwrap();
    let router = Arc::clone(&r.router);
    let reg = tokio::spawn(async move { router.complete(name).await });
    // The registration is waiting for the (slow) core; the app vanishes.
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert_eq!(r.router.forget(":1.77"), None, "not published yet");
    reg.await.unwrap();
    assert_eq!(r.router.known_apps(), 0);
    assert!(r.new_apps.try_recv().is_err(), "no port was handed over");
    eventually("core half retired", || c.halves.lock().unwrap().is_empty()).await;
    assert_eq!(c.table.lock().unwrap().len(), 0);
    assert_eq!(c.budget.used(), 0);
    assert!(
        matches!(r.router.resolve(":1.77"), RouteDecision::NotAnApp),
        "stragglers of a dead sender do not bring it back"
    );
}

/// Regression (review F11): registration and departure race for real. Each name gets its
/// registration and its `NameOwnerChanged` at jittered, interleaved times, so for a large share
/// of them the departure lands while the registration is in flight. Without the departed
/// tombstone checked at publication, those leak a route, a registry entry, a core half and budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn registration_and_departure_churn_leaks_nothing() {
    let c = core(|n| Duration::from_micros((n % 7) * 700));
    let r = rig(&c, Arc::new(Fixed(1)));
    let router = Arc::clone(&r.router);
    let registrar = c.registrar.clone();
    let mut tasks = tokio::task::JoinSet::new();
    let mut to_register = r.to_register;
    let mut new_apps = r.new_apps;
    for i in 0..400u64 {
        let name = format!(":1.{}", 1000 + i);
        // Registrations in flight are bounded: when the cap is reached the drain just gets
        // `Retry`, so ask again until the registration is accepted.
        let queued = loop {
            assert!(is_retry(&router.resolve(&name)));
            if let Ok(q) = to_register.try_recv() {
                break q;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        };
        let (reg, dep) = (Arc::clone(&router), Arc::clone(&router));
        let registrar = registrar.clone();
        tasks.spawn(async move { reg.complete(queued).await });
        tasks.spawn(async move {
            tokio::time::sleep(Duration::from_micros((i % 5) * 600)).await;
            // Like the connector's NameOwnerChanged handler.
            if let Some(app) = dep.forget(&name) {
                let _ = registrar.retire(app);
            }
        });
        // Keep the connector side draining and dropping ports, as it would.
        while let Ok(n) = new_apps.try_recv() {
            drop(n);
        }
    }
    while tasks.join_next().await.is_some() {}
    while let Ok(n) = new_apps.try_recv() {
        drop(n);
    }
    // A registration that won the race published an app whose departure then removed it; one
    // that lost never published. Either way, nothing is left.
    assert_eq!(router.known_apps(), 0);
    eventually("all core halves retired", || {
        c.halves.lock().unwrap().is_empty()
    })
    .await;
    assert_eq!(c.table.lock().unwrap().len(), 0);
    assert_eq!(c.budget.used(), 0, "a dead receiver charges nothing (AU-2)");
    assert_eq!(r.paths.len(), 0);
    let s = router.stats();
    assert!(
        s.announced > 0 && s.announce_failed > 0,
        "both orders happened: {s:?}"
    );
}

#[tokio::test]
async fn one_process_cannot_hold_unbounded_names() {
    let c = core(|_| Duration::ZERO);
    let mut r = rig(&c, Arc::new(Fixed(31337)));
    for i in 0..(MAX_NAMES_PER_OWNER + 5) {
        let name = format!(":1.{i}");
        assert!(is_retry(&r.router.resolve(&name)));
        let q = r.to_register.try_recv().unwrap();
        r.router.complete(q).await;
    }
    assert_eq!(r.router.known_apps(), MAX_NAMES_PER_OWNER);
    assert!(matches!(
        r.router.resolve(&format!(":1.{}", MAX_NAMES_PER_OWNER + 1)),
        RouteDecision::NotAnApp
    ));
    // Another process is not affected.
    assert!(is_retry(&r.router.resolve(":1.9999")));
}

#[test]
fn forget_marks_departed_and_departed_names_expire() {
    let mut st = State::default();
    let t0 = Instant::now();
    st.mark_departed(":1.1", t0);
    assert!(st.departed.contains(":1.1"));
    st.mark_departed(":1.2", t0 + DEPARTED_TTL + Duration::from_secs(1));
    assert!(!st.departed.contains(":1.1"), "expired");
    assert!(st.departed.contains(":1.2"));
}

#[tokio::test]
async fn forget_cancels_the_app_and_drops_its_paths() {
    let c = core(|_| Duration::ZERO);
    let mut r = rig(&c, Arc::new(Fixed(5)));
    assert!(is_retry(&r.router.resolve(":1.5")));
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    let RouteDecision::Route(route) = r.router.resolve(":1.5") else {
        panic!("registered");
    };
    let node = NodeKey {
        app: route.app,
        local: LocalId(1),
    };
    r.router.focus_seen(route.app, node, "/x/1");
    assert!(r.paths.get(node).is_some());
    let new = r.new_apps.try_recv().unwrap();
    assert!(!new.gone.is_cancelled());
    assert_eq!(r.router.forget(":1.5"), Some(route.app));
    assert!(new.gone.is_cancelled());
    assert!(r.paths.get(node).is_none());
    assert_eq!(r.router.forget(":1.5"), None);
}

#[tokio::test]
async fn take_all_returns_every_app_once() {
    let c = core(|_| Duration::ZERO);
    let mut r = rig(&c, Arc::new(NoCredentials));
    for n in [":1.1", ":1.2"] {
        assert!(is_retry(&r.router.resolve(n)));
        r.router.complete(r.to_register.try_recv().unwrap()).await;
    }
    assert_eq!(r.router.take_all().len(), 2);
    assert_eq!(r.router.known_apps(), 0);
    assert!(r.new_apps.try_recv().unwrap().gone.is_cancelled());
}
