//! Private dbus-daemon for the fake app's own tests. Isolated: its own socket under a fresh
//! temporary directory, no service directories (nothing can be activated), no ambient bus.

#![allow(
    dead_code,
    clippy::disallowed_methods,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    clippy::type_complexity,
    clippy::redundant_closure_for_method_calls,
    clippy::items_after_statements,
    clippy::too_many_lines
)]

use std::io::{BufRead, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};

use b1_fakeapp::{FakeApp, Options};
use gorca_testkit::b1::proto::ModelSpec;
use serde::Serialize;
use zbus::{Connection, Message};
use zvariant::DynamicType;

static N: AtomicU32 = AtomicU32::new(0);

pub struct Bus {
    child: Child,
    pub dir: PathBuf,
    pub address: String,
}

impl Bus {
    pub fn start() -> Bus {
        let dir = std::env::temp_dir().join(format!(
            "b1fa.{}.{}",
            std::process::id(),
            N.fetch_add(1, Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let sock = dir.join("bus");
        let cfg = dir.join("bus.conf");
        std::fs::write(
            &cfg,
            format!(
                "<busconfig><type>session</type><listen>unix:path={}</listen><auth>EXTERNAL</auth>\
                 <policy context=\"default\"><allow send_destination=\"*\" eavesdrop=\"true\"/>\
                 <allow eavesdrop=\"true\"/><allow own=\"*\"/></policy></busconfig>",
                sock.display()
            ),
        )
        .unwrap();
        let mut child = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", cfg.display()))
            .arg("--nofork")
            .arg("--print-address")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("dbus-daemon");
        let mut line = String::new();
        BufReader::new(child.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let address = line.trim().to_owned();
        assert!(address.starts_with("unix:"), "dbus-daemon said {address:?}");
        Bus {
            child,
            dir,
            address,
        }
    }

    pub async fn client(&self) -> Connection {
        zbus::connection::Builder::address(self.address.as_str())
            .unwrap()
            .build()
            .await
            .unwrap()
    }

    pub async fn app(&self, spec: ModelSpec, label: &str) -> FakeApp {
        FakeApp::start(Options {
            spec,
            label: label.to_owned(),
            bus_address: self.address.clone(),
            truth_path: Some(self.dir.join(format!("{label}.truth"))),
            register: false,
            peer_dir: self.dir.clone(),
            session_bus: None,
            own_name: None,
        })
        .await
        .unwrap()
    }
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub async fn call<B>(
    c: &Connection,
    dest: &str,
    path: &str,
    iface: &str,
    member: &str,
    body: &B,
) -> zbus::Result<Message>
where
    B: Serialize + DynamicType,
{
    c.call_method(Some(dest), path, Some(iface), member, body)
        .await
}
