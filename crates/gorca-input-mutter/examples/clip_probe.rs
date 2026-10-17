//! Manual probe of the clipboard owner against a PRIVATE session bus:
//! `clip_probe ADDRESS TEXT SECONDS` owns the clipboard with TEXT for SECONDS seconds.
//! Never point it at the live session.

use std::time::Duration;

use gorca_rt::{CancellationToken, mpsc};

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt().with_env_filter("debug").init();
    let mut args = std::env::args().skip(1);
    let (Some(address), Some(text), Some(secs)) = (args.next(), args.next(), args.next()) else {
        eprintln!("usage: clip_probe ADDRESS TEXT SECONDS");
        return;
    };
    let secs: u64 = secs.parse().unwrap_or(5);
    let (tx, rx) = mpsc::channel(4);
    let stop = CancellationToken::new();
    let task = tokio::spawn(gorca_input_mutter::clipboard::run(
        address,
        rx,
        stop.clone(),
    ));
    let _ = tx.send(text).await;
    tokio::time::sleep(Duration::from_secs(secs)).await;
    stop.cancel();
    let _ = task.await;
}
