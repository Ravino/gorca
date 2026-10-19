// SPDX-License-Identifier: GPL-3.0-or-later
//! Runs the pointer resolver (ADR-0025 `BackendOp::Locate`) against a real application on a (private) a11y
//! bus, for the end-to-end tests (`contrib/gnome-shell-extension/testclients/test_resolver.py`).
//!
//! `locate_cli A11Y_ADDRESS PID NAME` finds the object named NAME in the application whose process id is
//! PID and prints one line for `pointer_cli`: `clickwin PID|TITLE|OX|OY|W|H|toplevel/device|X|Y|L|active`.
#![allow(clippy::print_stdout, clippy::print_stderr, clippy::disallowed_methods)]

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use gorca_output_api::{ChildSpace, PointTarget};
use gorca_rt::Deadline;
use zbus::zvariant::OwnedValue;

async fn call<B>(
    conn: &zbus::Connection,
    dest: &str,
    path: &str,
    iface: &str,
    member: &str,
    body: &B,
) -> Option<zbus::Message>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    let d = Deadline(Instant::now() + Duration::from_secs(3));
    gorca_dbus::call_method(conn, d, dest, path, iface, member, body)
        .await
        .ok()
}

async fn children(conn: &zbus::Connection, bus: &str, path: &str) -> Vec<(String, String)> {
    let Some(m) = call(
        conn,
        bus,
        path,
        "org.a11y.atspi.Accessible",
        "GetChildren",
        &(),
    )
    .await
    else {
        return Vec::new();
    };
    m.body()
        .deserialize::<Vec<(String, zbus::zvariant::OwnedObjectPath)>>()
        .map(|v| {
            v.into_iter()
                .map(|(b, p)| (b, p.as_str().to_owned()))
                .collect()
        })
        .unwrap_or_default()
}

async fn name(conn: &zbus::Connection, bus: &str, path: &str) -> Option<String> {
    let m = call(
        conn,
        bus,
        path,
        "org.freedesktop.DBus.Properties",
        "Get",
        &("org.a11y.atspi.Accessible", "Name"),
    )
    .await?;
    let v: OwnedValue = m.body().deserialize().ok()?;
    v.downcast_ref::<&str>().ok().map(str::to_owned)
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let [_, address, pid, wanted] = args.as_slice() else {
        eprintln!("usage: locate_cli A11Y_ADDRESS PID NAME");
        std::process::exit(2);
    };
    let pid: u32 = pid.parse().unwrap_or(0);
    let conn = match zbus::connection::Builder::address(address.as_str()) {
        Ok(b) => match b.build().await {
            Ok(c) => c,
            Err(e) => {
                eprintln!("connect failed: {e}");
                std::process::exit(2);
            }
        },
        Err(e) => {
            eprintln!("bad address: {e}");
            std::process::exit(2);
        }
    };
    // The registry root lists the applications; pick the one whose connection belongs to PID.
    let apps = children(
        &conn,
        "org.a11y.atspi.Registry",
        "/org/a11y/atspi/accessible/root",
    )
    .await;
    for (bus, root) in apps {
        let Some(m) = call(
            &conn,
            "org.freedesktop.DBus",
            "/org/freedesktop/DBus",
            "org.freedesktop.DBus",
            "GetConnectionUnixProcessID",
            &(bus.as_str(),),
        )
        .await
        else {
            continue;
        };
        if m.body().deserialize::<u32>().ok() != Some(pid) {
            continue;
        }
        // Breadth first for the named object.
        let mut queue: VecDeque<String> = VecDeque::from([root]);
        let mut seen = 0;
        while let Some(path) = queue.pop_front() {
            seen += 1;
            if seen > 20_000 {
                break;
            }
            if name(&conn, &bus, &path).await.as_deref() == Some(wanted.as_str()) {
                let d = Deadline(Instant::now() + Duration::from_secs(3));
                match gorca_connector_atspi::locate_object(conn.clone(), &bus, &path, None, pid, d)
                    .await
                {
                    Ok(PointTarget::Window { top, x, y }) => {
                        let cs = match top.child_space {
                            ChildSpace::Toplevel => "toplevel",
                            ChildSpace::DevicePixels => "device",
                        };
                        println!(
                            "clickwin {}|{}|{}|{}|{}|{}|{}|{}|{}|L|{}",
                            top.pid,
                            top.title,
                            top.atspi_origin.0,
                            top.atspi_origin.1,
                            top.atspi_size.0,
                            top.atspi_size.1,
                            cs,
                            x,
                            y,
                            if top.active { "active" } else { "inactive" }
                        );
                        return;
                    }
                    Ok(PointTarget::Stage { .. }) => println!("stage target (unexpected)"),
                    Err(e) => println!("locate failed: {e}"),
                }
                return;
            }
            for (b, p) in children(&conn, &bus, &path).await {
                if b == bus {
                    queue.push_back(p);
                }
            }
        }
        println!("object {wanted:?} not found in pid {pid}");
        return;
    }
    println!("no application with pid {pid}");
}
