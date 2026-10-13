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
    /// Tests use the pid as the process group; the real `/proc` is never read.
    fn proc_info(&self, pid: u32) -> BoxFuture<'_, ProcInfo> {
        Box::pin(async move {
            ProcInfo {
                pgrp: Some(pid),
                start: None,
                ..ProcInfo::default()
            }
        })
    }

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

/// A non-focus event.
fn noise_signal() -> SignalView<'static> {
    SignalView {
        interface: "org.a11y.atspi.Event.Window",
        member: "Create",
        path: "/w",
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
    // The drain never waits: the answer is immediate even though the core takes 20 ms. Wall
    // time is the property here (a waiting resolve burns no CPU), so robustness to preemption
    // comes from the minimum of five first-resolves on fresh routers: a resolve that waits
    // for the core takes >= 20 ms every time, a preempted one is slow once at most.
    let fastest = (0..5)
        .map(|_| {
            let probe = rig(&c, Arc::new(Fixed(4242)));
            let t = std::time::Instant::now();
            assert!(is_retry(&probe.router.resolve(":1.42")));
            t.elapsed()
        })
        .min()
        .unwrap_or(Duration::MAX);
    assert!(fastest < Duration::from_millis(10), "{fastest:?}");
    let mut r = rig(&c, Arc::new(Fixed(4242)));
    assert!(is_retry(&r.router.resolve(":1.42")));
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
    assert_eq!(
        r.router.stats().parked_replayed,
        1,
        "two focus events coalesce to the newest"
    );
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
        r.router.park(":1.0", &noise_signal());
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
    assert!(is_retry(
        &r.router.resolve(&format!(":1.{}", MAX_NAMES_PER_OWNER + 1))
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

// ---- review round 2 (R2-1 .. R2-5) -------------------------------------------------------

fn last_focus_path(c: &Core, r: &Rig, app: AppId) -> Option<String> {
    let rx = c.halves.lock().unwrap().remove(&app)?;
    let d = rx.drain();
    d.events.iter().rev().find_map(|e| match e {
        NodeEvent::FocusChanged { node } => r.paths.get(*node).map(|p| p.to_string()),
        _ => None,
    })
}

/// Credentials by bus name: `:1.<n>` is pid `pids(n)`.
struct ByName(Box<dyn Fn(u32) -> Option<u32> + Send + Sync>);

impl Credentials for ByName {
    /// Tests use the pid as the process group; the real `/proc` is never read.
    fn proc_info(&self, pid: u32) -> BoxFuture<'_, ProcInfo> {
        Box::pin(async move {
            ProcInfo {
                pgrp: Some(pid),
                start: None,
                ..ProcInfo::default()
            }
        })
    }

    fn pid_of<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<u32>> {
        let n: u32 = name.trim_start_matches(":1.").parse().unwrap_or(0);
        let pid = (self.0)(n);
        Box::pin(async move { pid })
    }
}

/// P1 / R2-1: an app that emits more events than the park holds while it registers still gets
/// its newest focus; the newest focus sits in a slot outside the cap.
#[tokio::test]
async fn newest_focus_survives_the_park_cap() {
    let c = core(|_| Duration::from_millis(20));
    let mut r = rig(&c, Arc::new(Fixed(7)));
    assert!(is_retry(&r.router.resolve(":1.42")));
    let paths: Vec<String> = (0..40).map(|i| format!("/org/a/{i}")).collect();
    for p in &paths {
        r.router.park(":1.42", &focus_signal(p));
    }
    let name = r.to_register.try_recv().unwrap();
    r.router.complete(name).await;
    let RouteDecision::Route(route) = r.router.resolve(":1.42") else {
        panic!("registered")
    };
    let got = last_focus_path(&c, &r, route.app);
    assert_eq!(got.as_deref(), Some("/org/a/39"), "newest focus lost");
    assert_eq!(
        r.router.stats().parked_dropped,
        0,
        "focus is coalesced, not dropped"
    );
}

/// R2-1: a burst of non-focus events does not push the focus out, and the oldest of the burst
/// go first.
#[tokio::test]
async fn a_burst_of_other_events_never_displaces_the_focus() {
    let c = core(|_| Duration::from_millis(20));
    let mut r = rig(&c, Arc::new(Fixed(7)));
    assert!(is_retry(&r.router.resolve(":1.42")));
    r.router.park(":1.42", &focus_signal("/focus/node"));
    let noise = SignalView {
        interface: "org.a11y.atspi.Event.Window",
        member: "Create",
        path: "/w",
        body: &[],
        big_endian: false,
    };
    for _ in 0..500 {
        r.router.park(":1.42", &noise);
    }
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    let RouteDecision::Route(route) = r.router.resolve(":1.42") else {
        panic!("registered")
    };
    assert_eq!(
        last_focus_path(&c, &r, route.app).as_deref(),
        Some("/focus/node")
    );
    assert!(r.router.stats().parked_dropped >= 468);
}

/// P2 / R2-4: hostile senders fill their own shares; a new app's first focus is still kept.
#[tokio::test]
async fn hostile_parkers_cannot_drop_a_new_apps_focus() {
    let c = core(|_| Duration::from_secs(60)); // hostile registrations stay pending
    let r = rig(&c, Arc::new(Fixed(9)));
    let noise = SignalView {
        interface: "org.a11y.atspi.Event.Window",
        member: "Create",
        path: "/h",
        body: &[0u8; 400],
        big_endian: false,
    };
    for s in 0..(MAX_PENDING - 1) {
        let n = format!(":1.{}", 500 + s);
        assert!(is_retry(&r.router.resolve(&n)));
        for _ in 0..200 {
            r.router.park(&n, &noise);
        }
    }
    assert!(is_retry(&r.router.resolve(":1.900")), "the last free slot");
    let before = r.router.stats().parked_dropped;
    r.router.park(":1.900", &focus_signal("/victim/focus"));
    r.router.park(":1.900", &noise);
    assert_eq!(
        r.router.stats().parked_dropped,
        before,
        "nothing of the victim dropped"
    );
    let st = r.router.state();
    assert!(st.parked_bytes <= PARK_BYTES_TOTAL, "{}", st.parked_bytes);
    assert!(st.pending.get(":1.900").unwrap().focus.is_some());
}

/// P3 / R2-2: the registration publishes between the drain's `resolve` (Retry) and its `park`:
/// the event is delivered through the new route, not lost.
#[tokio::test]
async fn resolve_park_race_does_not_lose_the_event() {
    let c = core(|_| Duration::ZERO);
    let mut r = rig(&c, Arc::new(Fixed(11)));
    assert!(is_retry(&r.router.resolve(":1.77")));
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    r.router.park(":1.77", &focus_signal("/org/a/focus"));
    let RouteDecision::Route(route) = r.router.resolve(":1.77") else {
        panic!("registered")
    };
    assert_eq!(
        last_focus_path(&c, &r, route.app).as_deref(),
        Some("/org/a/focus"),
        "focus lost in the resolve/park window"
    );
    assert_eq!(r.router.stats().parked_dropped, 0);
}

/// R2-2: an event for a sender without a running registration (backoff or the pending cap)
/// keeps its newest focus for the next attempt.
#[tokio::test]
async fn focus_during_backoff_or_the_pending_cap_is_kept_for_the_next_attempt() {
    let c = core(|_| Duration::from_secs(60));
    let mut r = rig(&c, Arc::new(Fixed(3)));
    for i in 0..MAX_PENDING {
        assert!(is_retry(&r.router.resolve(&format!(":1.{i}"))));
    }
    while r.to_register.try_recv().is_ok() {}
    // Over the pending cap: no registration, but the focus is remembered, bounded.
    assert!(is_retry(&r.router.resolve(":1.4000")));
    r.router.park(":1.4000", &focus_signal("/first"));
    r.router.park(":1.4000", &focus_signal("/second"));
    assert_eq!(r.router.state().orphan_focus.len(), 1);
    // Once a slot is free the registration starts and adopts it.
    r.router.forget(":1.0");
    assert!(is_retry(&r.router.resolve(":1.4000")));
    {
        let st = r.router.state();
        assert_eq!(
            st.pending
                .get(":1.4000")
                .and_then(|p| p.focus.as_ref())
                .map(|f| f.path.as_str()),
            Some("/second")
        );
    }
    for i in 0..(MAX_ORPHAN_FOCUS * 3) {
        r.router
            .park(&format!(":1.{}", 5000 + i), &focus_signal("/x"));
    }
    assert!(r.router.state().orphan_focus.len() <= MAX_ORPHAN_FOCUS);
}

/// P4 / R2-5: the ignored set does not keep departed names.
#[tokio::test]
async fn the_ignored_set_does_not_grow_with_churn() {
    let c = core(|_| Duration::ZERO);
    let mut r = rig(&c, Arc::new(Fixed(13)));
    for i in 0..(MAX_NAMES_PER_OWNER + 2000) {
        let n = format!(":1.{}", 10_000 + i);
        let _ = r.router.resolve(&n);
        if let Ok(name) = r.to_register.try_recv() {
            r.router.complete(name).await;
        }
        if i >= MAX_NAMES_PER_OWNER {
            let _ = r.router.forget(&n);
        }
    }
    let ign = r.router.state().ignored.len();
    assert!(ign < 100, "ignored set keeps departed names: {ign}");
}

/// P5 / R2-3: oversized strings are refused at ingress; the park never holds megabytes.
#[tokio::test]
async fn parked_strings_are_bounded() {
    let c = core(|_| Duration::from_secs(60));
    let r = rig(&c, Arc::new(Fixed(5)));
    let rss = || -> u64 {
        let s = std::fs::read_to_string("/proc/self/status").unwrap();
        s.lines()
            .find(|l| l.starts_with("VmRSS:"))
            .unwrap()
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap()
    };
    let before = rss();
    let big = format!("/{}", "a".repeat(1 << 20));
    let long_name = "m".repeat(1 << 16);
    for s in 0..MAX_PENDING {
        let name = format!(":1.{}", 700 + s);
        let _ = r.router.resolve(&name);
        for _ in 0..MAX_PARKED_PER_SENDER {
            r.router.park(&name, &focus_signal(&big));
            r.router.park(
                &name,
                &SignalView {
                    member: &long_name,
                    ..focus_signal("/x")
                },
            );
        }
    }
    let grown = (rss() - before) / 1024;
    assert!(grown < 32, "park buffer held {grown} MiB");
    let st = r.router.state();
    assert_eq!(st.parked_bytes, 0);
    assert!(
        st.pending
            .values()
            .all(|p| p.focus.is_none() && p.events.is_empty())
    );
    assert_eq!(
        r.router.stats().parked_dropped,
        (MAX_PENDING * MAX_PARKED_PER_SENDER * 2) as u64
    );
}

/// R2-5: a global cap on applications. A process group that forked its way to the largest share
/// gives way; the newcomer of another group is served, and the largest group's own newcomer is
/// refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_application_cap_makes_the_largest_group_give_way() {
    let c = core(|_| Duration::ZERO);
    // pid 1 holds MAX_NAMES_PER_OWNER names, the rest one name each, until the cap is reached.
    let big = u32::try_from(MAX_NAMES_PER_OWNER).unwrap();
    let creds = ByName(Box::new(move |n| {
        Some(if n < big || n == 100_001 { 1 } else { n })
    }));
    let mut r = rig(&c, Arc::new(creds));
    for n in 0..u32::try_from(MAX_APPS_TOTAL).unwrap() {
        let name = format!(":1.{n}");
        assert!(is_retry(&r.router.resolve(&name)), "{name}");
        r.router.complete(r.to_register.try_recv().unwrap()).await;
    }
    assert_eq!(r.router.known_apps(), MAX_APPS_TOTAL);
    // A newcomer from another process: served; one of pid 1's applications is evicted.
    let newcomer = format!(":1.{}", 100_000);
    assert!(is_retry(&r.router.resolve(&newcomer)));
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    assert!(matches!(
        r.router.resolve(&newcomer),
        RouteDecision::Route(_)
    ));
    assert_eq!(r.router.known_apps(), MAX_APPS_TOTAL);
    let still_pid1 = (0..big)
        .filter(|n| {
            matches!(
                r.router.resolve(&format!(":1.{n}")),
                RouteDecision::Route(_)
            )
        })
        .count();
    // The evicted application asked to register again; that is its own business.
    while r.to_register.try_recv().is_ok() {}
    assert_eq!(
        still_pid1,
        MAX_NAMES_PER_OWNER - 1,
        "one of the largest group was evicted"
    );
    // The core side saw the retirement: no leaked halves beyond the cap.
    eventually("evicted app retired", || {
        c.halves.lock().unwrap().len() <= MAX_APPS_TOTAL
    })
    .await;
    // pid 1 is still the largest group: its own newcomer is refused, not served.
    let refused = format!(":1.{}", 100_001);
    assert!(is_retry(&r.router.resolve(&refused)));
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    assert!(
        is_retry(&r.router.resolve(&refused)),
        "refused at the cap: a backoff, not a verdict"
    );
    assert_eq!(r.router.known_apps(), MAX_APPS_TOTAL);
}

/// R2-5: connections without credentials share one small quota, so a slow credentials service
/// cannot bring the multiplier back.
#[tokio::test]
async fn unknown_credentials_share_one_small_quota() {
    let c = core(|_| Duration::ZERO);
    let mut r = rig(&c, Arc::new(NoCredentials));
    for i in 0..(MAX_UNKNOWN_NAMES + 20) {
        let _ = r.router.resolve(&format!(":1.{i}"));
        if let Ok(q) = r.to_register.try_recv() {
            r.router.complete(q).await;
        }
    }
    assert_eq!(r.router.known_apps(), MAX_UNKNOWN_NAMES);
}

// ---- review round 3 (R3-1 .. R3-3) --------------------------------------------------------

/// A router whose cap is reached with one big pid group and singletons; pid 1 holds
/// `MAX_NAMES_PER_OWNER` names (`:1.0` .. `:1.63`), everyone else one.
async fn filled(c: &Core, extra: Option<Arc<FocusedApp>>) -> Rig {
    let big = u32::try_from(MAX_NAMES_PER_OWNER).unwrap();
    let creds = ByName(Box::new(move |n| {
        Some(if n < big || n == 100_001 { 1 } else { n + 10 })
    }));
    let (new_tx, new_apps) = mpsc::channel(1024);
    let (reg_tx, to_register) = mpsc::channel(1024);
    let paths = Arc::new(PathTable::new());
    let mut router = AtspiRouter::new(
        c.registrar.clone(),
        Clock::System,
        Arc::clone(&paths),
        new_tx,
        reg_tx,
        Arc::new(creds),
    );
    if let Some(f) = extra {
        router = router.with_focused(f);
    }
    let mut r = Rig {
        router: Arc::new(router),
        new_apps,
        to_register,
        paths,
    };
    for n in 0..u32::try_from(MAX_APPS_TOTAL).unwrap() {
        assert!(is_retry(&r.router.resolve(&format!(":1.{n}"))));
        r.router.complete(r.to_register.try_recv().unwrap()).await;
    }
    assert_eq!(r.router.known_apps(), MAX_APPS_TOTAL);
    r
}

/// P6 / R3-1: a fork attack where every process holds ONE name must not evict an older,
/// legitimate application: the newest registration gives way, never an arbitrary one.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn singleton_forks_never_evict_the_first_application() {
    let c = core(|_| Duration::ZERO);
    let mut r = rig(&c, Arc::new(ByName(Box::new(|n| Some(n + 10)))));
    let victim = ":1.0";
    let total = u32::try_from(MAX_APPS_TOTAL).unwrap() + 2000;
    for n in 0..total {
        let _ = r.router.resolve(&format!(":1.{n}"));
        while let Ok(x) = r.to_register.try_recv() {
            r.router.complete(x).await;
        }
        assert!(
            r.router.state().apps.contains_key(victim),
            "victim evicted at attacker name #{n}"
        );
    }
    assert!(r.router.known_apps() <= MAX_APPS_TOTAL);
}

/// P7 / R3-3: the per-process name cap holds under concurrent registrations
/// (reserve-then-commit).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_name_cap_holds_under_concurrency() {
    let c = core(|_| Duration::from_millis(50));
    let mut r = rig(&c, Arc::new(ByName(Box::new(|_| Some(4242)))));
    for n in 0..60u32 {
        let _ = r.router.resolve(&format!(":1.{n}"));
        r.router.complete(r.to_register.try_recv().unwrap()).await;
    }
    let mut names = Vec::new();
    for n in 1000..(1000 + u32::try_from(MAX_PENDING).unwrap()) {
        let _ = r.router.resolve(&format!(":1.{n}"));
        names.push(r.to_register.try_recv().unwrap());
    }
    let mut set = tokio::task::JoinSet::new();
    for n in names {
        let router = Arc::clone(&r.router);
        set.spawn(async move { router.complete(n).await });
    }
    while set.join_next().await.is_some() {}
    let st = r.router.state();
    let held = st.names_per_group.get(&Some(4242)).copied().unwrap_or(0);
    assert_eq!(held, MAX_NAMES_PER_OWNER, "reservations are exact");
    assert_eq!(st.apps.len(), MAX_NAMES_PER_OWNER);
}

/// R3-1: the focused application is never evicted, even when it is exactly the one the policy
/// would pick (the newest name of the largest group).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_focused_application_is_never_evicted() {
    let c = core(|_| Duration::ZERO);
    let focused = Arc::new(FocusedApp::new());
    let mut r = filled(&c, Some(Arc::clone(&focused))).await;
    // The newest name of pid 1's group is `:1.63` (registered last of the group).
    let RouteDecision::Route(route) = r.router.resolve(":1.63") else {
        panic!("registered")
    };
    focused.publish(route.app);
    for i in 0..10 {
        let newcomer = format!(":1.{}", 200_000 + i);
        assert!(is_retry(&r.router.resolve(&newcomer)));
        r.router.complete(r.to_register.try_recv().unwrap()).await;
        assert!(
            r.router.state().apps.contains_key(":1.63"),
            "focused app evicted"
        );
    }
    assert_eq!(r.router.known_apps(), MAX_APPS_TOTAL);
    assert!(r.router.stats().evicted >= 10);
}

/// R3-1: the shell's process is never evicted nor refused, even as an all-singleton table
/// would otherwise sacrifice its newest member.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_shell_is_never_evicted_or_refused() {
    let c = core(|_| Duration::ZERO);
    let mut r = rig(&c, Arc::new(Scripted::singletons()));
    // Shell registered last before the cap is hit: it is the newest, hence the usual victim.
    r.router
        .set_protected_process(Some((SHELL_PID, SHELL_START)));
    for n in 0..u32::try_from(MAX_APPS_TOTAL).unwrap() {
        let _ = r.router.resolve(&format!(":1.{n}"));
        r.router.complete(r.to_register.try_recv().unwrap()).await;
    }
    let shell = format!(":1.{}", MAX_APPS_TOTAL - 1);
    for i in 0..5 {
        let _ = r.router.resolve(&format!(":1.{}", 300_000 + i));
        r.router.complete(r.to_register.try_recv().unwrap()).await;
        assert!(
            r.router.state().apps.contains_key(shell.as_str()),
            "shell evicted"
        );
    }
}

/// R3-1: an evicted application goes through the backoff path and is not served at once, and
/// an eviction is not a permanent mute: after the backoff it registers again.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_evicted_application_comes_back_through_the_backoff_path() {
    let c = core(|_| Duration::ZERO);
    let mut r = filled(&c, None).await;
    let newcomer = ":1.200000";
    assert!(is_retry(&r.router.resolve(newcomer)));
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    // pid 1's newest name was evicted: `:1.63`.
    assert!(!r.router.state().apps.contains_key(":1.63"));
    assert!(
        is_retry(&r.router.resolve(":1.63")),
        "backoff, not NotAnApp"
    );
    assert!(
        r.to_register.try_recv().is_err(),
        "no immediate registration"
    );
    tokio::time::sleep(BACKOFF_MIN * 4).await;
    assert!(is_retry(&r.router.resolve(":1.63")));
    assert!(
        r.to_register.try_recv().is_ok(),
        "it registers again after the backoff"
    );
}

/// R3-2: a refusal at the cap is temporary: the sender backs off and is retried, never cached
/// as "not an application".
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refusal_at_the_cap_is_never_permanent() {
    let c = core(|_| Duration::ZERO);
    let mut r = filled(&c, None).await;
    // pid 1 is the biggest group: its own newcomer is refused...
    let refused = ":1.100001";
    // (free one pid-1 slot first so the per-process cap is not what refuses it)
    let _ = r.router.forget(":1.0");
    // ...refilled by a newcomer of another process, so the table is full again.
    assert!(is_retry(&r.router.resolve(":1.250000")));
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    assert_eq!(r.router.known_apps(), MAX_APPS_TOTAL);
    assert!(is_retry(&r.router.resolve(refused)));
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    assert!(
        is_retry(&r.router.resolve(refused)),
        "temporary: Retry, never NotAnApp"
    );
    assert!(!r.router.state().ignored.contains(refused));
    // ...and is tried again after the backoff.
    tokio::time::sleep(BACKOFF_MIN * 4).await;
    assert!(is_retry(&r.router.resolve(refused)));
    assert!(r.to_register.try_recv().is_ok());
}

/// R3-2: the per-process name cap is temporary too.
#[tokio::test]
async fn the_per_process_name_cap_refuses_with_a_backoff_not_a_verdict() {
    let c = core(|_| Duration::ZERO);
    let mut r = rig(&c, Arc::new(Fixed(31337)));
    for i in 0..=MAX_NAMES_PER_OWNER {
        let _ = r.router.resolve(&format!(":1.{i}"));
        r.router.complete(r.to_register.try_recv().unwrap()).await;
    }
    assert_eq!(r.router.known_apps(), MAX_NAMES_PER_OWNER);
    let over = format!(":1.{MAX_NAMES_PER_OWNER}");
    assert!(is_retry(&r.router.resolve(&over)));
    assert!(!r.router.state().ignored.contains(over.as_str()));
    // A name of that process leaves: the refused one gets in on its next attempt.
    r.router.forget(":1.0");
    tokio::time::sleep(BACKOFF_MIN * 40).await;
    assert!(is_retry(&r.router.resolve(&over)));
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    assert!(matches!(r.router.resolve(&over), RouteDecision::Route(_)));
}

/// R3-2: a slow credentials service does not push a name into the "unknown" group: the lookup
/// is retried first.
#[tokio::test]
async fn credentials_are_retried_before_a_name_is_filed_as_unknown() {
    struct Flaky(std::sync::atomic::AtomicUsize);
    impl Credentials for Flaky {
        /// Tests use the pid as the process group; the real `/proc` is never read.
        fn proc_info(&self, pid: u32) -> BoxFuture<'_, ProcInfo> {
            Box::pin(async move {
                ProcInfo {
                    pgrp: Some(pid),
                    start: None,
                    ..ProcInfo::default()
                }
            })
        }

        fn pid_of<'a>(&'a self, _name: &'a str) -> BoxFuture<'a, Option<u32>> {
            let n = self.0.fetch_add(1, Ordering::SeqCst);
            Box::pin(async move { (n >= 2).then_some(777) })
        }
    }
    let c = core(|_| Duration::ZERO);
    let mut r = rig(&c, Arc::new(Flaky(std::sync::atomic::AtomicUsize::new(0))));
    let _ = r.router.resolve(":1.5");
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    let st = r.router.state();
    assert_eq!(st.names_per_group.get(&Some(777)), Some(&1));
    assert_eq!(st.names_per_group.get(&None), None, "not filed as unknown");
}

// ---- security scan of the round-3 fixes ---------------------------------------------------

/// pid and process info from closures, so a test can say exactly who is who.
struct Scripted {
    pid: Box<dyn Fn(u32) -> Option<u32> + Send + Sync>,
    info: Box<dyn Fn(u32) -> ProcInfo + Send + Sync>,
}

/// Start time of the shell process in these tests: `SHELL_PID` below, `pid * 10` for the rest.
const SHELL_START: u64 = 5210;
const SHELL_PID: u32 = 10 + 511;

impl Scripted {
    /// Every name `:1.n` is its own process (pid `n + 10`, its own process group).
    fn singletons() -> Self {
        Self {
            pid: Box::new(|n| Some(n + 10)),
            info: Box::new(|pid| ProcInfo {
                pgrp: Some(pid),
                start: Some(u64::from(pid) * 10),
                ..ProcInfo::default()
            }),
        }
    }
}

impl Credentials for Scripted {
    fn pid_of<'a>(&'a self, name: &'a str) -> BoxFuture<'a, Option<u32>> {
        let n: u32 = name.trim_start_matches(":1.").parse().unwrap_or(0);
        let pid = (self.pid)(n);
        Box::pin(async move { pid })
    }

    fn proc_info(&self, pid: u32) -> BoxFuture<'_, ProcInfo> {
        let info = (self.info)(pid);
        Box::pin(async move { info })
    }
}

/// Scan item 3: the attacker fills the cap first (one process group, many processes), then a
/// legitimate application launches, gains focus, and from then on cannot be pushed out by the
/// attacker's further registrations.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_legitimate_app_launched_after_the_attacker_filled_the_cap_wins_and_stays() {
    let c = core(|_| Duration::ZERO);
    // Attacker: names 0..512 are processes 1000.. in ONE process group (5000); the legitimate
    // application (name 900000) is its own process and group.
    let creds = Scripted {
        pid: Box::new(|n| Some(if n < 100_000 { 1000 + n } else { n })),
        info: Box::new(|pid| ProcInfo {
            pgrp: Some(if (1000..100_000).contains(&pid) {
                5000
            } else {
                pid
            }),
            start: Some(u64::from(pid)),
            ..ProcInfo::default()
        }),
    };
    let focused = Arc::new(FocusedApp::new());
    let (new_tx, mut new_apps) = mpsc::channel(4096);
    let (reg_tx, mut to_register) = mpsc::channel(4096);
    let router = Arc::new(
        AtspiRouter::new(
            c.registrar.clone(),
            Clock::System,
            Arc::new(PathTable::new()),
            new_tx,
            reg_tx,
            Arc::new(creds),
        )
        .with_focused(Arc::clone(&focused)),
    );
    let mut register = |name: String| {
        let router = Arc::clone(&router);
        let q = {
            let _ = router.resolve(&name);
            to_register.try_recv().ok()
        };
        async move {
            if let Some(q) = q {
                router.complete(q).await;
            }
        }
    };
    for n in 0..u32::try_from(MAX_APPS_TOTAL).unwrap() {
        register(format!(":1.{n}")).await;
    }
    assert_eq!(router.known_apps(), MAX_APPS_TOTAL);

    // The legitimate application launches: it wins a slot at the attacker's expense.
    register(":1.900000".to_owned()).await;
    assert!(
        matches!(router.resolve(":1.900000"), RouteDecision::Route(_)),
        "the user's new application was not served"
    );
    assert_eq!(router.known_apps(), MAX_APPS_TOTAL);
    // It gains focus: the Reader accepted and presented it.
    let RouteDecision::Route(legit) = router.resolve(":1.900000") else {
        panic!("registered")
    };
    focused.publish(legit.app);

    // The attacker keeps registering: it only ever evicts or refuses itself.
    for n in 10_000..10_400u32 {
        register(format!(":1.{n}")).await;
        assert!(
            router.state().apps.contains_key(":1.900000"),
            "the focused legitimate app was pushed out at attacker name {n}"
        );
    }
    while new_apps.try_recv().is_ok() {}
}

/// Scan item 3: with every group a singleton there is no group "larger" than the newcomer's, so
/// the newcomer is refused. Evicting the newest existing application instead would let one more
/// registration push out an application that had just launched.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn with_only_singletons_the_newcomer_is_refused_and_nobody_is_evicted() {
    let c = core(|_| Duration::ZERO);
    let mut r = rig(&c, Arc::new(Scripted::singletons()));
    for n in 0..u32::try_from(MAX_APPS_TOTAL).unwrap() {
        let _ = r.router.resolve(&format!(":1.{n}"));
        r.router.complete(r.to_register.try_recv().unwrap()).await;
    }
    let before: std::collections::BTreeSet<String> = r
        .router
        .state()
        .apps
        .keys()
        .map(std::string::ToString::to_string)
        .collect();
    for n in 0..50u32 {
        let name = format!(":1.{}", 700_000 + n);
        let _ = r.router.resolve(&name);
        r.router.complete(r.to_register.try_recv().unwrap()).await;
        assert!(
            is_retry(&r.router.resolve(&name)),
            "refused with a backoff, not served and not a verdict"
        );
    }
    let after: std::collections::BTreeSet<String> = r
        .router
        .state()
        .apps
        .keys()
        .map(std::string::ToString::to_string)
        .collect();
    assert_eq!(before, after, "no application was evicted");
    assert_eq!(r.router.stats().evicted, 0);
}

/// Scan item 2: protection is keyed on (pid, process start time). An impostor that inherits the
/// shell's pid after it exited has a different start time and is not protected.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_recycled_shell_pid_does_not_inherit_protection() {
    let c = core(|_| Duration::ZERO);
    // Name 511 is the shell's pid with the right start time; name 510 has the SAME pid but a
    // different start time (a recycled pid).
    let creds = Scripted {
        pid: Box::new(|n| Some(if n == 510 { SHELL_PID } else { n + 10 })),
        info: Box::new(|pid| ProcInfo {
            pgrp: Some(pid + 100_000),
            start: Some(u64::from(pid) * 10),
            ..ProcInfo::default()
        }),
    };
    let mut r = rig(&c, Arc::new(creds));
    // Pretend the shell is the process (SHELL_PID, SHELL_START); 510 has start SHELL_PID*10
    // = 5210 as well in the closure above, so make the shell's start different: the shell is
    // registered as 5211.
    r.router
        .set_protected_process(Some((SHELL_PID, SHELL_START + 1)));
    for n in 0..u32::try_from(MAX_NAMES_PER_OWNER).unwrap().min(2) {
        let _ = r.router.resolve(&format!(":1.{}", 510 + n));
        r.router.complete(r.to_register.try_recv().unwrap()).await;
    }
    let st = r.router.state();
    let impostor = st.apps.get(":1.510").unwrap();
    assert!(
        !st.is_shell(impostor.group, impostor.start),
        "same pid, different start time: not the shell"
    );
    drop(st);
    // And a handover clears the previous protection.
    r.router
        .set_protected_process(Some((SHELL_PID, SHELL_START)));
    {
        let st = r.router.state();
        let impostor = st.apps.get(":1.510").unwrap();
        assert!(
            st.is_shell(impostor.group, impostor.start),
            "now it matches"
        );
    }
    r.router.set_protected_process(None);
    let st = r.router.state();
    let impostor = st.apps.get(":1.510").unwrap();
    assert!(!st.is_shell(impostor.group, impostor.start));
}

/// The Shell profile is granted by the router's verified identity, never by what the application
/// says about itself: only the registration matching the protected process is `verified_shell`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_the_protected_process_is_announced_as_the_verified_shell() {
    let c = core(|_| Duration::ZERO);
    let creds = Scripted {
        pid: Box::new(|n| Some(n + 10)),
        info: Box::new(|pid| ProcInfo {
            pgrp: Some(pid),
            start: Some(u64::from(pid) * 10),
            ..ProcInfo::default()
        }),
    };
    let mut r = rig(&c, Arc::new(creds));
    r.router
        .set_protected_process(Some((SHELL_PID, SHELL_START)));
    for n in [511u32, 512] {
        let _ = r.router.resolve(&format!(":1.{n}"));
        r.router.complete(r.to_register.try_recv().unwrap()).await;
    }
    let mut seen = std::collections::HashMap::new();
    while let Ok(n) = r.new_apps.try_recv() {
        seen.insert(n.bus_name.to_string(), n.verified_shell);
    }
    assert_eq!(seen.get(":1.511"), Some(&true));
    assert_eq!(seen.get(":1.512"), Some(&false));
}

/// The shell restarting at a full table is never refused: as a newcomer it takes the newest
/// unprotected application's slot.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_shell_as_a_newcomer_is_admitted_at_the_cap() {
    let c = core(|_| Duration::ZERO);
    let mut r = rig(&c, Arc::new(Scripted::singletons()));
    for n in 0..u32::try_from(MAX_APPS_TOTAL).unwrap() {
        let _ = r.router.resolve(&format!(":1.{n}"));
        r.router.complete(r.to_register.try_recv().unwrap()).await;
    }
    // The shell comes back as name 999999: pid 1000009, started at 77.
    r.router
        .set_protected_process(Some((1_000_009, 10_000_090)));
    let _ = r.router.resolve(":1.999999");
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    assert!(
        matches!(r.router.resolve(":1.999999"), RouteDecision::Route(_)),
        "the shell must be served"
    );
    assert_eq!(r.router.known_apps(), MAX_APPS_TOTAL);
    assert_eq!(r.router.stats().evicted, 1);
}

// ---- fail-open state drift ----------------------------------------------------------------

/// A `NameOwnerChanged` that was never delivered must not leave a registered name, its quota or
/// its protection behind: reconciliation against `ListNames` retires it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reconciliation_retires_names_the_bus_no_longer_has() {
    let c = core(|_| Duration::ZERO);
    let focused = Arc::new(FocusedApp::new());
    let (new_tx, _new_apps) = mpsc::channel(64);
    let (reg_tx, mut to_register) = mpsc::channel(64);
    let router = Arc::new(
        AtspiRouter::new(
            c.registrar.clone(),
            Clock::System,
            Arc::new(PathTable::new()),
            new_tx,
            reg_tx,
            Arc::new(Scripted::singletons()),
        )
        .with_focused(Arc::clone(&focused)),
    );
    for n in 1..=3 {
        let _ = router.resolve(&format!(":1.{n}"));
        router.complete(to_register.try_recv().unwrap()).await;
    }
    let RouteDecision::Route(gone_app) = router.resolve(":1.3") else {
        panic!("registered")
    };
    focused.publish(gone_app.app); // the dead application still holds focus protection
    assert!(focused.protects(gone_app.app));

    // The bus lists :1.1 and :1.2 only; :1.3 left and its NameOwnerChanged was missed.
    let live: HashSet<String> = [":1.1", ":1.2", "org.freedesktop.DBus"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    let retired = router.reconcile_with(&live);
    assert_eq!(retired, [gone_app.app]);
    assert_eq!(router.known_apps(), 2);
    assert!(
        !focused.protects(gone_app.app),
        "no protection for a name that is gone"
    );
    assert!(matches!(router.resolve(":1.3"), RouteDecision::NotAnApp));
    eventually("core half retired", || c.halves.lock().unwrap().len() == 2).await;
    {
        let st = router.state();
        assert_eq!(st.names_per_group.values().sum::<usize>(), 2);
        assert_eq!(st.names_per_evg.values().sum::<usize>(), 2);
    }
    // Reconciling again, with everything present, changes nothing.
    assert!(router.reconcile_with(&live).is_empty());
}

/// When the bus cannot be asked, eviction is frozen: the validated protection is kept (not
/// cleared), and every application is still served.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn when_revalidation_fails_eviction_freezes_and_reading_continues() {
    let c = core(|_| Duration::ZERO);
    let focused = Arc::new(FocusedApp::new());
    let (new_tx, _new_apps) = mpsc::channel(64);
    let (reg_tx, mut to_register) = mpsc::channel(64);
    let router = Arc::new(
        AtspiRouter::new(
            c.registrar.clone(),
            Clock::System,
            Arc::new(PathTable::new()),
            new_tx,
            reg_tx,
            Arc::new(Scripted::singletons()),
        )
        .with_focused(Arc::clone(&focused)),
    );
    let _ = router.resolve(":1.5");
    router.complete(to_register.try_recv().unwrap()).await;
    let RouteDecision::Route(app) = router.resolve(":1.5") else {
        panic!("registered")
    };
    focused.publish(app.app);
    router.set_protected_process(Some((15, 150)));
    router.freeze_eviction();
    assert!(router.eviction_frozen());
    assert!(
        focused.protects(app.app),
        "the validated protection is kept"
    );
    assert!(
        router.state().protected_process.is_some(),
        "the shell identity is kept"
    );
    // Reading is untouched: the application is still routed.
    assert!(matches!(router.resolve(":1.5"), RouteDecision::Route(_)));
}

/// Fail closed on unknown identity: a registration whose process start time could not be read
/// has no identity, so it can never match the shell's protected `(pid, start)`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unknown_start_time_never_matches_the_shell() {
    let c = core(|_| Duration::ZERO);
    let creds = Scripted {
        pid: Box::new(|_| Some(SHELL_PID)),
        info: Box::new(|pid| ProcInfo {
            pgrp: Some(pid),
            start: None, // /proc was unreadable
            ..ProcInfo::default()
        }),
    };
    let mut r = rig(&c, Arc::new(creds));
    r.router
        .set_protected_process(Some((SHELL_PID, SHELL_START)));
    let _ = r.router.resolve(":1.1");
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    let st = r.router.state();
    let e = st.apps.get(":1.1").unwrap();
    assert!(!st.is_shell(e.group, e.start));
}

/// TOCTOU: the victim is chosen on a snapshot of who is protected. If the Reader protects exactly
/// that application before the eviction commits, the choice is re-validated and another victim is
/// taken; the focused application survives.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_victim_that_becomes_focused_before_the_commit_is_spared() {
    let c = core(|_| Duration::ZERO);
    let focused = Arc::new(FocusedApp::new());
    let mut r = filled(&c, Some(Arc::clone(&focused))).await;
    // Without interference the newest name of the big group (:1.63) would be the victim.
    let RouteDecision::Route(about_to_be_victim) = r.router.resolve(":1.63") else {
        panic!("registered")
    };
    let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    {
        let (focused, fired) = (Arc::clone(&focused), Arc::clone(&fired));
        *r.router.commit_hook.lock().unwrap() = Some(Box::new(move || {
            if !fired.swap(true, Ordering::SeqCst) {
                // The Reader accepts a focus on that very application, mid-decision.
                focused.publish(about_to_be_victim.app);
            }
        }));
    }
    let newcomer = ":1.200000";
    assert!(is_retry(&r.router.resolve(newcomer)));
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    assert!(fired.load(Ordering::SeqCst), "the race was exercised");
    let st = r.router.state();
    assert!(
        st.apps.contains_key(":1.63"),
        "the application that became focused was evicted"
    );
    assert!(st.apps.contains_key(newcomer), "the newcomer was served");
    assert!(!st.apps.contains_key(":1.62"), "the next one gave way");
}

/// A failed reconciliation costs protection, never reading; the next successful one gives the
/// protection back.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn frozen_eviction_thaws_at_the_next_good_reconciliation() {
    let c = core(|_| Duration::ZERO);
    let focused = Arc::new(FocusedApp::new());
    let (new_tx, _new_apps) = mpsc::channel(64);
    let (reg_tx, mut to_register) = mpsc::channel(64);
    let router = Arc::new(
        AtspiRouter::new(
            c.registrar.clone(),
            Clock::System,
            Arc::new(PathTable::new()),
            new_tx,
            reg_tx,
            Arc::new(Scripted::singletons()),
        )
        .with_focused(Arc::clone(&focused)),
    );
    let _ = router.resolve(":1.5");
    router.complete(to_register.try_recv().unwrap()).await;
    let RouteDecision::Route(app) = router.resolve(":1.5") else {
        panic!("registered")
    };
    focused.publish(app.app);
    router.freeze_eviction();
    assert!(
        focused.protects(app.app),
        "protection is kept while unverified"
    );
    assert!(
        matches!(router.resolve(":1.5"), RouteDecision::Route(_)),
        "reading continues"
    );
    // The bus answers again and still lists the application.
    let live: HashSet<String> = [":1.5".to_owned()].into_iter().collect();
    assert!(router.reconcile_with(&live).is_empty());
    router.resume_eviction();
    assert!(!router.eviction_frozen());
    assert!(focused.protects(app.app), "still protected after the thaw");
}

/// P8 / R4-1: while protection cannot be verified (ListNames failing), the focused application
/// must not be evictable. Eviction is frozen: at the cap the newcomer is refused with a backoff,
/// nobody is evicted, and the newcomer is served after the thaw.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_focused_app_survives_a_newcomer_while_protection_is_unverified() {
    let c = core(|_| Duration::ZERO);
    let focused = Arc::new(FocusedApp::new());
    let mut r = filled(&c, Some(Arc::clone(&focused))).await;
    let RouteDecision::Route(route) = r.router.resolve(":1.63") else {
        panic!("registered")
    };
    focused.publish(route.app);
    r.router.freeze_eviction(); // reconcile_loop: ListNames failed
    let newcomer = ":1.200000";
    assert!(is_retry(&r.router.resolve(newcomer)));
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    assert!(
        r.router.state().apps.contains_key(":1.63"),
        "focused app evicted while protection was unverified"
    );
    assert_eq!(r.router.stats().evicted, 0, "nobody was evicted");
    assert!(
        is_retry(&r.router.resolve(newcomer)),
        "the newcomer backs off instead"
    );
    // The bus answers again: after its backoff the newcomer is served, at the big group's cost.
    r.router.resume_eviction();
    tokio::time::sleep(BACKOFF_MIN * 4).await;
    assert!(is_retry(&r.router.resolve(newcomer)));
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    assert!(matches!(
        r.router.resolve(newcomer),
        RouteDecision::Route(_)
    ));
    assert!(
        r.router.state().apps.contains_key(":1.63"),
        "still the focused one"
    );
}

/// The shell, the one newcomer that is never refused, is admitted over the cap while eviction is
/// frozen, without evicting anybody.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_shell_is_admitted_over_the_cap_while_eviction_is_frozen() {
    let c = core(|_| Duration::ZERO);
    let mut r = rig(&c, Arc::new(Scripted::singletons()));
    for n in 0..u32::try_from(MAX_APPS_TOTAL).unwrap() {
        let _ = r.router.resolve(&format!(":1.{n}"));
        r.router.complete(r.to_register.try_recv().unwrap()).await;
    }
    r.router
        .set_protected_process(Some((1_000_009, 10_000_090)));
    r.router.freeze_eviction();
    let _ = r.router.resolve(":1.999999");
    r.router.complete(r.to_register.try_recv().unwrap()).await;
    assert!(matches!(
        r.router.resolve(":1.999999"),
        RouteDecision::Route(_)
    ));
    assert_eq!(r.router.stats().evicted, 0);
    assert_eq!(r.router.known_apps(), MAX_APPS_TOTAL + 1);
}

/// R4-2: applications the shell launched share the shell's process group but each has its own
/// systemd scope. A user's freshly launched application must not be grouped with the shell
/// and its siblings: with the scope as the key it is a group of one and gets served at the cap,
/// at the expense of the big attacker group.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn shell_launched_apps_are_grouped_by_scope_not_by_process_group() {
    let c = core(|_| Duration::ZERO);
    // Every name shares process group 532394 (the shell's). Names below 500 are an attacker's
    // children in ONE scope; the rest are ordinary applications, one scope each.
    let creds = Scripted {
        pid: Box::new(|n| Some(n + 10)),
        info: Box::new(|pid| ProcInfo {
            pgrp: Some(532_394),
            start: Some(u64::from(pid)),
            scope: Some(if pid < 510 {
                "app-attacker-1.scope".to_owned()
            } else {
                format!("app-gnome-thing-{pid}.scope")
            }),
        }),
    };
    let (new_tx, mut new_apps) = mpsc::channel(4096);
    let (reg_tx, mut to_register) = mpsc::channel(4096);
    let router = Arc::new(AtspiRouter::new(
        c.registrar.clone(),
        Clock::System,
        Arc::new(PathTable::new()),
        new_tx,
        reg_tx,
        Arc::new(creds),
    ));
    for n in 0..u32::try_from(MAX_APPS_TOTAL).unwrap() {
        let _ = router.resolve(&format!(":1.{n}"));
        router.complete(to_register.try_recv().unwrap()).await;
    }
    // With the process group as the key all 512 would be ONE group (the newcomer's own) and the
    // newcomer would be refused. By scope the attacker's 500 are the biggest group.
    let _ = router.resolve(":1.700000");
    router.complete(to_register.try_recv().unwrap()).await;
    assert!(
        matches!(router.resolve(":1.700000"), RouteDecision::Route(_)),
        "a freshly launched application was refused because it shared the shell's process group"
    );
    assert_eq!(router.stats().evicted, 1);
    // The evicted one was the attacker's newest name, not an ordinary application.
    let st = router.state();
    assert!(
        (0..500u32).any(|n| !st.apps.contains_key(format!(":1.{n}").as_str())),
        "an attacker name was evicted"
    );
    assert!(
        (500..512u32).all(|n| st.apps.contains_key(format!(":1.{n}").as_str())),
        "ordinary applications survived"
    );
    while new_apps.try_recv().is_ok() {}
}
