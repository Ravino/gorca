//! Tiny HTTP server for the corpus: serves pages (with the B-6 driver injected), the driver
//! script, and the command/acknowledge long-poll the page uses to take orders from the harness.
//!
//! Loopback only, no TLS, one purpose. Not a general web server: it understands `GET`/`POST`
//! with a `Content-Length` body and nothing else.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Notify, mpsc, oneshot};

const POLL_TIMEOUT: Duration = Duration::from_secs(10);

/// What the page reported after running a command (`t*` are epoch milliseconds in the page).
#[derive(Clone, Debug, Default)]
pub struct Ack {
    pub id: u64,
    pub op: String,
    pub t0: f64,
    pub t1: f64,
    pub t2: f64,
    pub info: String,
}

#[derive(Default)]
struct Shared {
    pages: Mutex<HashMap<String, Arc<Vec<u8>>>>,
    driver: Mutex<Arc<Vec<u8>>>,
    waiting: Mutex<HashMap<u64, oneshot::Sender<Ack>>>,
    loaded: Mutex<Option<Ack>>,
    loaded_notify: Notify,
    hits: Mutex<u64>,
}

pub struct PageServer {
    pub addr: SocketAddr,
    shared: Arc<Shared>,
    cmd_tx: mpsc::UnboundedSender<String>,
    next_id: Mutex<u64>,
}

fn pct_decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let h = std::str::from_utf8(&b[i + 1..i + 3])
                    .ok()
                    .and_then(|h| u8::from_str_radix(h, 16).ok());
                if let Some(v) = h {
                    out.push(v);
                    i += 3;
                    continue;
                }
                out.push(b'%');
                i += 1;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_ack(body: &str) -> Ack {
    let mut a = Ack::default();
    for kv in body.split('&') {
        let Some((k, v)) = kv.split_once('=') else {
            continue;
        };
        match k {
            "id" => a.id = v.parse().unwrap_or(0),
            "op" => a.op = pct_decode(v),
            "t0" => a.t0 = v.parse().unwrap_or(0.0),
            "t1" => a.t1 = v.parse().unwrap_or(0.0),
            "t2" => a.t2 = v.parse().unwrap_or(0.0),
            "info" => a.info = pct_decode(v),
            _ => {}
        }
    }
    a
}

impl PageServer {
    /// Bind on loopback (`port` 0 picks one) and start serving.
    pub async fn start(port: u16, driver_js: Vec<u8>) -> Result<Arc<Self>> {
        let listener = TcpListener::bind(("127.0.0.1", port))
            .await
            .context("bind page server")?;
        let addr = listener.local_addr()?;
        let shared = Arc::new(Shared::default());
        *shared
            .driver
            .lock()
            .map_err(|_| anyhow::anyhow!("poisoned"))? = Arc::new(driver_js);
        let (cmd_tx, cmd_rx) = mpsc::unbounded_channel::<String>();
        let cmd_rx = Arc::new(tokio::sync::Mutex::new(cmd_rx));
        let srv = Arc::new(Self {
            addr,
            shared: Arc::clone(&shared),
            cmd_tx,
            next_id: Mutex::new(1),
        });
        tokio::spawn(async move {
            loop {
                let Ok((sock, _)) = listener.accept().await else {
                    continue;
                };
                let sh = Arc::clone(&shared);
                let rx = Arc::clone(&cmd_rx);
                tokio::spawn(async move {
                    let _ = serve_conn(sock, sh, rx).await;
                });
            }
        });
        Ok(srv)
    }

    pub fn add_page(&self, path: &str, body: Vec<u8>) {
        if let Ok(mut p) = self.shared.pages.lock() {
            p.insert(path.to_owned(), Arc::new(body));
        }
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.addr.port(), path)
    }

    /// Wait for the page's `load` acknowledgement (consumes it).
    pub async fn wait_loaded(&self, timeout: Duration) -> Option<Ack> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Ok(mut l) = self.shared.loaded.lock()
                && let Some(a) = l.take()
            {
                return Some(a);
            }
            tokio::time::timeout_at(deadline, self.shared.loaded_notify.notified())
                .await
                .ok()?;
        }
    }

    /// Forget a stale load acknowledgement (before navigating to a new page).
    pub fn clear_loaded(&self) {
        if let Ok(mut l) = self.shared.loaded.lock() {
            *l = None;
        }
    }

    /// Send a command to the page and wait for its acknowledgement. `json_args` is the inside of
    /// the JSON object after `"op"`, for example `,"n":100`.
    pub async fn command(&self, op: &str, json_args: &str, timeout: Duration) -> Result<Ack> {
        let id = {
            let mut g = self
                .next_id
                .lock()
                .map_err(|_| anyhow::anyhow!("poisoned"))?;
            *g += 1;
            *g
        };
        let (tx, rx) = oneshot::channel();
        self.shared
            .waiting
            .lock()
            .map_err(|_| anyhow::anyhow!("poisoned"))?
            .insert(id, tx);
        self.cmd_tx
            .send(format!("{{\"id\":{id},\"op\":\"{op}\"{json_args}}}"))
            .map_err(|_| anyhow::anyhow!("server gone"))?;
        let ack = tokio::time::timeout(timeout, rx)
            .await
            .map_err(|_| anyhow::anyhow!("page did not acknowledge {op} in {timeout:?}"))?
            .map_err(|_| anyhow::anyhow!("ack channel closed"))?;
        Ok(ack)
    }

    pub fn hits(&self) -> u64 {
        self.shared.hits.lock().map_or(0, |g| *g)
    }
}

async fn serve_conn(
    mut sock: TcpStream,
    sh: Arc<Shared>,
    cmd_rx: Arc<tokio::sync::Mutex<mpsc::UnboundedReceiver<String>>>,
) -> Result<()> {
    let mut buf = Vec::with_capacity(2048);
    let (head_end, content_len) = loop {
        let mut tmp = [0u8; 2048];
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&tmp[..n]);
        if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            let head = String::from_utf8_lossy(&buf[..p]).to_string();
            let cl = head
                .lines()
                .find_map(|l| {
                    l.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap_or(0))
                })
                .unwrap_or(0);
            break (p + 4, cl);
        }
        if buf.len() > 64 * 1024 {
            return Ok(());
        }
    };
    while buf.len() < head_end + content_len {
        let mut tmp = [0u8; 4096];
        let n = sock.read(&mut tmp).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&tmp[..n]);
    }
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut first = head.lines().next().unwrap_or("").split(' ');
    let (method, target) = (first.next().unwrap_or(""), first.next().unwrap_or("/"));
    let path_only = target.split('?').next().unwrap_or(target);
    let body = String::from_utf8_lossy(&buf[head_end..(head_end + content_len).min(buf.len())])
        .to_string();
    if let Ok(mut h) = sh.hits.lock() {
        *h += 1;
    }

    let (status, ctype, payload): (&str, &str, Vec<u8>) = match (method, path_only) {
        ("GET", "/b6/driver.js") => {
            let d = sh.driver.lock().map(|g| Arc::clone(&g)).unwrap_or_default();
            ("200 OK", "application/javascript", d.as_ref().clone())
        }
        ("GET", "/b6/poll") => {
            let mut rx = cmd_rx.lock().await;
            let cmd = tokio::time::timeout(POLL_TIMEOUT, rx.recv())
                .await
                .ok()
                .flatten();
            (
                "200 OK",
                "application/json",
                cmd.unwrap_or_else(|| "{\"op\":\"noop\"}".into())
                    .into_bytes(),
            )
        }
        ("POST", "/b6/ack") => {
            let a = parse_ack(&body);
            if a.op == "loaded" {
                if let Ok(mut l) = sh.loaded.lock() {
                    *l = Some(a);
                }
                sh.loaded_notify.notify_waiters();
                sh.loaded_notify.notify_one();
            } else if let Some(tx) = sh.waiting.lock().ok().and_then(|mut w| w.remove(&a.id)) {
                let _ = tx.send(a);
            }
            ("200 OK", "text/plain", b"ok".to_vec())
        }
        ("GET", p) => {
            let page = sh.pages.lock().ok().and_then(|m| m.get(p).cloned());
            match page {
                Some(b) => {
                    let mut html = String::from_utf8_lossy(&b).into_owned();
                    let tag = "<script src=\"/b6/driver.js\"></script>";
                    match html.rfind("</body>") {
                        Some(i) => html.insert_str(i, tag),
                        None => html.push_str(tag),
                    }
                    ("200 OK", "text/html; charset=utf-8", html.into_bytes())
                }
                None => ("404 Not Found", "text/plain", b"not found".to_vec()),
            }
        }
        _ => ("405 Method Not Allowed", "text/plain", b"no".to_vec()),
    };
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(&payload).await?;
    sock.shutdown().await.ok();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ack_parsing_decodes_info() {
        let a = parse_ack("id=7&op=reverse&t0=1.5&t1=2.5&t2=3.5&info=reverse%2050");
        assert_eq!(
            (a.id, a.op.as_str(), a.info.as_str()),
            (7, "reverse", "reverse 50")
        );
        assert!((a.t2 - 3.5).abs() < 1e-9);
    }

    #[tokio::test]
    async fn page_poll_ack_roundtrip() {
        let srv = PageServer::start(0, b"//js".to_vec()).await.unwrap();
        srv.add_page("/pages/x.html", b"<html><body>hi</body></html>".to_vec());
        let port = srv.addr.port();
        // A fake page: GET page, long-poll, ack.
        let fake = tokio::spawn(async move {
            let get = |req: String| async move {
                let mut s = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
                s.write_all(req.as_bytes()).await.unwrap();
                let mut out = Vec::new();
                s.read_to_end(&mut out).await.unwrap();
                String::from_utf8_lossy(&out).into_owned()
            };
            let html = get("GET /pages/x.html HTTP/1.1\r\nHost: x\r\n\r\n".into()).await;
            assert!(html.contains("/b6/driver.js"));
            let cmd = get("GET /b6/poll HTTP/1.1\r\nHost: x\r\n\r\n".into()).await;
            assert!(cmd.contains("\"op\":\"edit\""));
            let id: u64 = cmd
                .split("\"id\":")
                .nth(1)
                .unwrap()
                .split(',')
                .next()
                .unwrap()
                .parse()
                .unwrap();
            let body = format!("id={id}&op=edit&t0=1&t1=2&t2=3&info=done");
            let req = format!(
                "POST /b6/ack HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            );
            get(req).await;
        });
        let ack = srv
            .command("edit", ",\"n\":3", Duration::from_secs(5))
            .await
            .unwrap();
        assert_eq!(ack.info, "done");
        fake.await.unwrap();
    }
}
