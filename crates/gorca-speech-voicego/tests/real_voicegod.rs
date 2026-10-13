//! Manual check against a real `voicegod` (not part of `cargo test`; run with `--ignored`).
//!
//! Start a private voicegod (null sink) and point the test at its socket:
//! `GORCA_VOICEGOD_SOCKET=/path/v.sock cargo test -p gorca-speech-voicego --test real_voicegod
//! -- --ignored --nocapture`. Never point it at the desktop's own speech service.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use gorca_model::UttId;
use gorca_output_api::{SpeechClient, SpeechCmd, SpeechEvent, Verbalized, VoiceRef};
use gorca_rt::Deadline;
use gorca_speech_voicego::{VoicegoClient, VoicegoOptions};

fn speak(id: u64, text: &str) -> SpeechCmd {
    SpeechCmd::Speak {
        id: UttId(id),
        text: Verbalized {
            text: Arc::from(text),
            lang: None,
        },
        voice: VoiceRef::Default,
        marks: false,
    }
}

async fn wait_for(c: &mut VoicegoClient, want: SpeechEvent, within: Duration) -> Option<Instant> {
    let end = tokio::time::Instant::now() + within;
    loop {
        match tokio::time::timeout_at(end, c.events().recv()).await {
            Ok(Some(ev)) if ev == want => return Some(Instant::now()),
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => return None,
        }
    }
}

#[tokio::test]
#[ignore = "needs a private voicegod: set GORCA_VOICEGOD_SOCKET"]
async fn replace_latency_and_lost_speech() {
    let sock = std::env::var("GORCA_VOICEGOD_SOCKET").expect("GORCA_VOICEGOD_SOCKET");
    let dl = Deadline(Instant::now() + Duration::from_secs(2));
    let mut c = VoicegoClient::connect(&VoicegoOptions::with_path(sock), dl)
        .await
        .unwrap();

    // Plain speak: submit -> 701 BEGIN.
    let mut plain = Vec::new();
    for i in 0..20u64 {
        let t = Instant::now();
        c.submit(speak(1000 + i, "hello world")).unwrap();
        let at = wait_for(
            &mut c,
            SpeechEvent::Started(UttId(1000 + i)),
            Duration::from_secs(2),
        )
        .await
        .expect("no Started");
        plain.push(at - t);
        wait_for(
            &mut c,
            SpeechEvent::Ended(UttId(1000 + i)),
            Duration::from_secs(2),
        )
        .await;
    }

    // Replace: cancel the running long utterance and speak a new one, 50 times.
    let long = "this is a long sentence that keeps the engine busy for a while ".repeat(8);
    let mut lat = Vec::new();
    let mut lost = 0;
    let mut prev = 1u64;
    c.submit(speak(prev, &long)).unwrap();
    wait_for(
        &mut c,
        SpeechEvent::Started(UttId(prev)),
        Duration::from_secs(2),
    )
    .await
    .expect("first start");
    for i in 2..52u64 {
        let t = Instant::now();
        c.submit(SpeechCmd::Replace {
            stop: UttId(prev),
            then: Box::new(speak(i, &long)),
        })
        .unwrap();
        match wait_for(
            &mut c,
            SpeechEvent::Started(UttId(i)),
            Duration::from_secs(2),
        )
        .await
        {
            Some(at) => lat.push(at - t),
            None => lost += 1,
        }
        prev = i;
        tokio::time::sleep(Duration::from_millis(40)).await;
    }
    lat.sort();
    plain.sort();
    let p = |v: &[Duration], num: usize, den: usize| v[(v.len() - 1) * num / den];
    eprintln!(
        "plain speak -> BEGIN: p50 {:?} max {:?}",
        p(&plain, 1, 2),
        plain.last().unwrap()
    );
    eprintln!(
        "replace -> BEGIN of new: n={} p50 {:?} p99 {:?} max {:?}; lost {lost}",
        lat.len(),
        p(&lat, 1, 2),
        p(&lat, 99, 100),
        lat.last().unwrap()
    );
    assert_eq!(lost, 0, "speech was lost after a replace");
}
