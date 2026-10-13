//! Reviewer audit probes for the M1 config task (not part of the branch).
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use gorca_core::{ConfigRequest, ControlMsg, LoadedSettings, SettingsMsg};
use gorca_daemon::{ConfigSource, ConfigTask, PlatformOverrides};
use gorca_input_api::LaneLinks;
use gorca_rt::{CancellationToken, mpsc};

fn tmpdir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("gorca-audit-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

async fn next_loaded(rx: &mut mpsc::Receiver<ControlMsg>, wait: Duration) -> Option<Arc<LoadedSettings>> {
    loop {
        match tokio::time::timeout(wait, rx.recv()).await {
            Ok(Some(ControlMsg::Settings(SettingsMsg::Loaded(l)))) => return Some(l),
            Ok(Some(_)) => {}
            _ => return None,
        }
    }
}

/// Save writes Y; the user edits to X; the user reverts to exactly Y. The file is Y, so the
/// active settings must be Y's again.
#[tokio::test]
async fn reverting_to_our_own_saved_text_is_reloaded() {
    let dir = tmpdir("revert");
    let path = dir.join("gorca.toml");
    std::fs::write(&path, "[speech]\nrate = 40\n").unwrap();
    let (ctl_tx, mut rx) = mpsc::channel(32);
    let (req_tx, req_rx) = mpsc::channel(8);
    let t = ConfigTask {
        source: ConfigSource {
            path: Some(path.clone()),
            overrides: PlatformOverrides::default(),
            no_probe: true,
        },
        links: LaneLinks::new(),
        control: ctl_tx,
        requests: req_rx,
    };
    let stop = CancellationToken::new();
    let h = tokio::spawn(t.run(stop.clone()));
    let l = next_loaded(&mut rx, Duration::from_secs(5)).await.unwrap();
    assert_eq!(l.settings.speech.rate, 40);
    req_tx
        .send(ConfigRequest::Save(gorca_config::Overlay {
            pitch: Some(33),
            ..gorca_config::Overlay::default()
        }))
        .await
        .unwrap();
    let l = next_loaded(&mut rx, Duration::from_secs(5)).await.unwrap();
    assert_eq!(l.settings.speech.pitch, 33);
    let saved = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, saved.replace("rate = 40", "rate = 70")).unwrap();
    let l = next_loaded(&mut rx, Duration::from_secs(5)).await.unwrap();
    assert_eq!(l.settings.speech.rate, 70);
    std::fs::write(&path, &saved).unwrap();
    let l = next_loaded(&mut rx, Duration::from_secs(2)).await;
    stop.cancel();
    let _ = h.await;
    let _ = std::fs::remove_dir_all(&dir);
    let l = l.expect("BUG: the revert to our own saved text was never reloaded; settings stay at rate 70 while the file says 40");
    assert_eq!(l.settings.speech.rate, 40);
}

/// A symlinked settings file (dotfile managers) must stay a symlink after an explicit save.
#[tokio::test]
async fn save_keeps_a_symlinked_settings_file() {
    let dir = tmpdir("link");
    let real = dir.join("dotfiles-gorca.toml");
    let path = dir.join("gorca.toml");
    std::fs::write(&real, "[speech]\nrate = 40\n").unwrap();
    std::os::unix::fs::symlink(&real, &path).unwrap();
    let (ctl_tx, mut rx) = mpsc::channel(32);
    let (req_tx, req_rx) = mpsc::channel(8);
    let t = ConfigTask {
        source: ConfigSource {
            path: Some(path.clone()),
            overrides: PlatformOverrides::default(),
            no_probe: true,
        },
        links: LaneLinks::new(),
        control: ctl_tx,
        requests: req_rx,
    };
    let stop = CancellationToken::new();
    let h = tokio::spawn(t.run(stop.clone()));
    let _ = next_loaded(&mut rx, Duration::from_secs(5)).await.unwrap();
    req_tx
        .send(ConfigRequest::Save(gorca_config::Overlay {
            pitch: Some(33),
            ..gorca_config::Overlay::default()
        }))
        .await
        .unwrap();
    let _ = next_loaded(&mut rx, Duration::from_secs(5)).await.unwrap();
    stop.cancel();
    let _ = h.await;
    let is_link = std::fs::symlink_metadata(&path).unwrap().file_type().is_symlink();
    let real_text = std::fs::read_to_string(&real).unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert!(is_link, "BUG: save replaced the symlink with a regular file; the dotfile target still says: {real_text}");
}
