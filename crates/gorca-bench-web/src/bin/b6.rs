//! `b6`: the B-6 web SourceLayer harness.
//!
//! ```text
//! b6 corpus [--tests-root tests/web]
//! b6 run --bus unix:path=... --pages large10k,spa-list-200 --scenarios build,churn,scale
//!        --runs 5 --out bench/results/web-prep/run1 [--full-matrix] [--firefox /usr/bin/firefox]
//! ```
//!
//! `run` must be started inside the private session created by `tests/web/b6/run_b6.sh`
//! (private dbus + a11y bus + headless gnome-shell). It launches its own Firefox with a temporary
//! `--profile` and stops only that process group.

use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use gorca_bench_web::arm::Target;
use gorca_bench_web::corpus::{Corpus, Kind};
use gorca_bench_web::report::{J, obj, s};
use gorca_bench_web::scenarios::{self, Ctx};
use gorca_bench_web::server::PageServer;
use gorca_bench_web::{discover, firefox};

#[derive(Debug)]
struct Args {
    cmd: String,
    tests_root: PathBuf,
    bus: String,
    pages: Vec<String>,
    scenarios: Vec<String>,
    runs: usize,
    out: PathBuf,
    firefox: String,
    browser: firefox::BrowserKind,
    full_matrix: bool,
    settle_secs: u64,
}

fn parse() -> Result<Args> {
    let mut it = std::env::args().skip(1);
    let cmd = it.next().unwrap_or_else(|| "help".into());
    let mut a = Args {
        cmd,
        tests_root: PathBuf::from("tests/web"),
        bus: String::new(),
        pages: Vec::new(),
        scenarios: vec!["build".into()],
        runs: 5,
        out: PathBuf::from("bench/results/web-prep/run"),
        firefox: "firefox".into(),
        browser: firefox::BrowserKind::Firefox,
        full_matrix: false,
        settle_secs: 2,
    };
    while let Some(k) = it.next() {
        let mut val = || it.next().with_context(|| format!("{k} needs a value"));
        match k.as_str() {
            "--tests-root" => a.tests_root = val()?.into(),
            "--bus" => a.bus = val()?,
            "--pages" => a.pages = val()?.split(',').map(str::to_owned).collect(),
            "--scenarios" => a.scenarios = val()?.split(',').map(str::to_owned).collect(),
            "--runs" => a.runs = val()?.parse()?,
            "--out" => a.out = val()?.into(),
            "--firefox" | "--browser-bin" => a.firefox = val()?,
            "--browser" => {
                a.browser = match val()?.as_str() {
                    "firefox" => firefox::BrowserKind::Firefox,
                    "chromium" => firefox::BrowserKind::Chromium,
                    other => bail!("unknown browser {other}"),
                }
            }
            "--settle-secs" => a.settle_secs = val()?.parse()?,
            "--full-matrix" => a.full_matrix = true,
            other => bail!("unknown argument {other}"),
        }
    }
    Ok(a)
}

fn write(out: &Path, name: &str, j: &J) -> Result<()> {
    std::fs::create_dir_all(out)?;
    let p = out.join(name);
    std::fs::write(&p, j.render())?;
    eprintln!("wrote {}", p.display());
    Ok(())
}

/// One browser launch per (page, scenario): scenarios mutate the page, so each starts from a
/// fresh document.
async fn run_one(a: &Args, corpus: &Corpus, page: &str, scenario: &str) -> Result<()> {
    let entry = corpus
        .get(page)
        .with_context(|| format!("no corpus entry {page}"))?
        .clone();
    if matches!(entry.kind, Kind::Spa(_)) {
        eprintln!("{page}: SPA entries need the SPA server; skipped");
        return Ok(());
    }
    let (path, body) = corpus.render(&entry)?.context("page body")?;
    let driver = std::fs::read(a.tests_root.join("b6/driver.js")).context("driver.js")?;
    let server = PageServer::start(0, driver).await?;
    let bare = path.split('?').next().unwrap_or(&path).to_owned();
    server.add_page(&bare, body);
    // `prewarm` starts on a tiny page, activates the cache domains there, then navigates to the
    // page under test.
    let (start_entry, url) = if scenario == "prewarm" {
        let w = corpus.get("warmup").context("corpus entry warmup")?.clone();
        let (wp, wb) = corpus.render(&w)?.context("warmup body")?;
        server.add_page(&wp, wb);
        let u = server.url(&wp);
        (w, u)
    } else {
        (entry.clone(), server.url(&path))
    };

    let tag = format!("{}-{}-{}", std::process::id(), page, scenario);
    let tmp = std::env::temp_dir().join(format!("gorca-b6-{tag}"));
    let log = std::env::temp_dir().join(format!("gorca-b6-{tag}.log"));
    server.clear_loaded();
    let ff = firefox::PrivateFirefox::launch(a.browser, &a.firefox, &tmp, &url, &log)?;
    eprintln!("{page}/{scenario}: browser pid {} -> {url}", ff.pid);

    let result = async {
        let bus = discover::connect_bus(&a.bus).await?;
        let (app_bus, app_root) =
            discover::wait_for_app(&bus, a.browser.app_needle(), Duration::from_secs(60)).await?;
        let app = discover::open_app(&bus, &app_bus, 4).await?;
        eprintln!("{page}/{scenario}: app {app_bus} p2p={}", app.is_p2p());
        let doc = discover::wait_for_document(
            &app,
            &app_root,
            &start_entry.title,
            Duration::from_secs(90),
        )
        .await?;
        eprintln!(
            "{page}/{scenario}: document {:?} at {} showing={}",
            doc.name, doc.path, doc.showing
        );
        let loaded = server.wait_loaded(Duration::from_secs(60)).await;
        eprintln!("{page}/{scenario}: page load ack: {}", loaded.is_some());
        tokio::time::sleep(Duration::from_secs(a.settle_secs)).await;
        let target = Target {
            bus,
            app_bus_name: app_bus,
            app,
            doc_path: doc.path,
            app_root,
        };
        let ctx = Ctx::new(target, server.clone(), entry.clone(), a.runs, a.out.clone());
        let bname = format!("{:?}", a.browser).to_lowercase();
        match scenario {
            "build" => {
                let vs = scenarios::variants(a.full_matrix);
                let j = scenarios::build(&ctx, &vs).await?;
                write(&a.out, &format!("{page}.build-{bname}.json"), &j)?;
            }
            "churn" => {
                let j = scenarios::churn(&ctx, true).await?;
                write(&a.out, &format!("{page}.churn-{bname}.json"), &j)?;
            }
            "churn-nodomid" => {
                // Ablation: hide DOM ids from the identity ladder (IL-2 off).
                let j = scenarios::churn(&ctx, false).await?;
                write(&a.out, &format!("{page}.churn-nodomid-{bname}.json"), &j)?;
            }
            "research" => {
                let j = gorca_bench_web::research::run(&ctx, &bname).await?;
                write(&a.out, &format!("{page}.research-{bname}.json"), &j)?;
            }
            "prewarm" => {
                let j = scenarios::prewarm(&ctx, &path).await?;
                write(&a.out, &format!("{page}.prewarm-{bname}.json"), &j)?;
            }
            "scale" => {
                let j = scenarios::scale(&ctx).await?;
                write(&a.out, &format!("{page}.scale-{bname}.json"), &j)?;
            }
            other => bail!("unknown scenario {other}"),
        }
        anyhow::Ok(())
    }
    .await;

    ff.shutdown().await;
    result
}

#[tokio::main]
async fn main() -> Result<()> {
    let a = parse()?;
    match a.cmd.as_str() {
        "corpus" => {
            let c = Corpus::load(&a.tests_root)?;
            let mut rows = Vec::new();
            for e in &c.entries {
                let ok = match &e.kind {
                    Kind::Spa(_) => "needs spa server".to_owned(),
                    _ => match c.render(e) {
                        Ok(Some((p, b))) => format!("ok {p} {} bytes", b.len()),
                        Ok(None) => "n/a".into(),
                        Err(err) => format!("MISSING: {err:#}"),
                    },
                };
                println!(
                    "{:16} ~{:>6} nodes  {:50} {}",
                    e.name, e.approx_nodes, ok, e.purpose
                );
                rows.push(obj([("name", s(&e.name)), ("status", s(ok))]));
            }
            Ok(())
        }
        "run" => {
            firefox::assert_private_session(&a.bus)?;
            let c = Corpus::load(&a.tests_root)?;
            if a.pages.is_empty() {
                bail!("--pages is required");
            }
            let mut failed = Vec::new();
            for p in &a.pages {
                for sc in &a.scenarios {
                    if let Err(e) = run_one(&a, &c, p, sc).await {
                        eprintln!("{p}/{sc}: FAILED: {e:#}");
                        failed.push(format!("{p}/{sc}: {e:#}"));
                    }
                }
            }
            if !failed.is_empty() {
                let j = J::A(failed.iter().map(s).collect());
                write(&a.out, "failures.json", &j)?;
                bail!("{} page(s) failed", failed.len());
            }
            Ok(())
        }
        _ => {
            println!(
                "usage: b6 corpus | b6 run --bus ADDR --pages p1,p2 --scenarios build,churn,scale [--runs N] [--out DIR] [--full-matrix]"
            );
            Ok(())
        }
    }
}
