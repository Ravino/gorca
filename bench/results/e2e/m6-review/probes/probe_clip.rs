//! Reviewer probe (M6 review): the connector's `screen()` keeps the LAST 2048 bytes of a large
//! screen (`clip_tail(&t, MAX_TEXT_BYTES)`) but reports `base: 0` and the caret as an offset into
//! the FULL text. Feed the extension exactly that and compare with the unclipped truth.
//! Copy to extensions/ext-vte/tests/ and run `cargo test -p ext-vte --test probe_clip -- --nocapture`.
use ext_vte::VteExtension;
use gorca_ext_api::terminal::{TermAction, TermConfig, TermInput, TerminalHooks};

const MAX_TEXT_BYTES: usize = 2048; // crates/gorca-connector-atspi/src/decode.rs:18

fn clip_tail(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut start = s.len() - max;
    while !s.is_char_boundary(start) {
        start += 1;
    }
    &s[start..]
}

fn focus_line(text: &str, caret: u32) -> Vec<String> {
    let mut e = VteExtension::new(&TermConfig::default());
    let mut out: Vec<TermAction> = Vec::new();
    e.step(0, TermInput::Focus, &mut out);
    out.clear();
    e.step(5, TermInput::Screen { text: text.to_owned(), caret: Some(caret) }, &mut out);
    out.iter()
        .filter_map(|a| match a {
            TermAction::Say { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn probe_large_screen_caret_line() {
    // A 40x100 screen (e.g. nano in a maximised window): row r is "row NN " padded with dots.
    let rows: Vec<String> = (0..40).map(|r| format!("row {r:02} {}", ".".repeat(92))).collect();
    let mut full = rows.join("\n");
    full.push('\n');
    // Cursor on row 10, column 4.
    let caret: u32 = (10 * 101 + 4) as u32;
    let truth = focus_line(&full, caret);
    let clipped = clip_tail(&full, MAX_TEXT_BYTES);
    let got = focus_line(clipped, caret);
    println!("full bytes {} clipped bytes {}", full.len(), clipped.len());
    println!("truth: {:?}", truth.iter().map(|s| &s[..s.len().min(12)]).collect::<Vec<_>>());
    println!("clipped: {:?}", got.iter().map(|s| &s[..s.len().min(12)]).collect::<Vec<_>>());
    assert_eq!(truth, got, "the caret line read on focus differs once the screen is clipped");
}
