//! Session scenarios: a tiny terminal model feeds the extension the way the host would.

#![allow(clippy::cast_possible_truncation, clippy::format_collect)]

use super::*;
use crate::screen;

/// The host side: a clock, the terminal's current screen, and what the extension asked for.
struct Rig {
    s: Session,
    now: Ms,
    /// What the fake terminal shows and where its caret is.
    screen: String,
    caret: u32,
    /// The deadline of the armed timer, and when the pending screen read is answered.
    tick_due: Option<Ms>,
    read_due: Option<Ms>,
}

fn lines(n: std::ops::Range<usize>) -> String {
    n.map(|i| format!("line {i}\n")).collect()
}

impl Rig {
    fn new() -> Self {
        Self {
            s: Session::new(&TermConfig::default()),
            now: 1000,
            screen: String::new(),
            caret: 0,
            tick_due: None,
            read_due: None,
        }
    }

    fn feed(&mut self, input: TermInput) -> Vec<TermAction> {
        let mut out = Vec::new();
        self.s.step(self.now, input, &mut out);
        out
    }

    /// Answer every `FetchScreen` in `acts` with the terminal's screen and return the new actions.
    fn answer(&mut self, mut acts: Vec<TermAction>) -> Vec<TermAction> {
        let mut all = Vec::new();
        for _ in 0..4 {
            let fetch = acts.iter().any(|a| matches!(a, TermAction::FetchScreen));
            all.append(&mut acts);
            if !fetch {
                break;
            }
            self.now += 1;
            acts = self.feed(TermInput::Screen {
                text: self.screen.clone(),
                caret: Some(self.caret),
            });
        }
        all
    }

    /// Focus the terminal showing `screen` with the caret at `caret`.
    fn focus(&mut self, screen: &str, caret: u32) -> Vec<TermAction> {
        self.screen = screen.to_owned();
        self.caret = caret;
        let a = self.feed(TermInput::Focus);
        self.answer(a)
    }

    fn wait(&mut self, ms: Ms) -> Vec<TermAction> {
        self.now += ms;
        let a = self.feed(TermInput::Tick);
        self.answer(a)
    }

    /// The terminal changed to `screen`/`caret` in a way the ingress collapsed (T-1).
    fn repaint(&mut self, screen: &str, caret: u32) -> Vec<TermAction> {
        self.screen = screen.to_owned();
        self.caret = caret;
        let mut acts = self.feed(TermInput::Edit(TermEdit::Replaced));
        acts.extend(self.feed(TermInput::Caret(caret)));
        acts.extend(self.wait(31));
        acts
    }

    fn key(&mut self, k: TermKey) -> Vec<TermAction> {
        self.now += 5;
        self.feed(TermInput::Key(k))
    }

    /// A character typed and echoed by the shell, with the payload.
    fn echo_insert(&mut self, at: u32, s: &str) -> Vec<TermAction> {
        let n = s.chars().count() as u32;
        let mut text = self.screen.clone();
        let byte = text
            .char_indices()
            .nth(at as usize)
            .map_or(text.len(), |(b, _)| b);
        text.insert_str(byte, s);
        self.screen = text;
        self.caret = at + n;
        self.now += 10;
        let mut acts = self.feed(TermInput::Edit(TermEdit::Insert {
            offset: at,
            len: n,
            text: Some(s.to_owned()),
        }));
        acts.extend(self.feed(TermInput::Caret(self.caret)));
        acts.extend(self.wait(31));
        acts
    }
}

fn says(acts: &[TermAction]) -> Vec<(String, Urgency)> {
    acts.iter()
        .filter_map(|a| match a {
            TermAction::Say { text, urgency, .. } => Some((crate::english(text), *urgency)),
            TermAction::FetchScreen | TermAction::ArmTimer(_) | TermAction::ClaimKey => None,
        })
        .collect()
}

fn texts(acts: &[TermAction]) -> Vec<String> {
    says(acts).into_iter().map(|(t, _)| t).collect()
}

fn claimed(acts: &[TermAction]) -> bool {
    acts.iter().any(|a| matches!(a, TermAction::ClaimKey))
}

// ---- focus --------------------------------------------------------------------------------

#[test]
fn focus_reads_the_screen_and_speaks_the_cursor_line_queued() {
    let mut r = Rig::new();
    let acts = r.focus("out\nme@box:~$ ls", 16);
    assert_eq!(
        says(&acts),
        vec![("me@box:~$ ls".to_owned(), Urgency::Queued)]
    );
    assert_eq!(r.s.counters().fetches, 1);
}

#[test]
fn an_empty_cursor_line_is_blank() {
    let mut r = Rig::new();
    let acts = r.focus("out\n\n", 4);
    assert_eq!(texts(&acts), vec!["blank".to_owned()]);
}

#[test]
fn a_failed_baseline_goes_cold_and_the_next_event_reads_again_silently() {
    let mut r = Rig::new();
    r.screen = "$ ".to_owned();
    let a = r.feed(TermInput::Focus);
    assert!(a.iter().any(|x| matches!(x, TermAction::FetchScreen)));
    let a = r.feed(TermInput::ScreenFailed);
    assert!(says(&a).is_empty());
    assert_eq!(r.s.counters().fetch_failed, 1);
    // A text event wakes it: a silent baseline.
    r.caret = 2;
    let a = r.feed(TermInput::Edit(TermEdit::Replaced));
    assert!(a.iter().any(|x| matches!(x, TermAction::FetchScreen)));
    let a = r.answer(a);
    assert!(says(&a).is_empty());
}

#[test]
fn a_reply_nobody_waits_for_is_ignored() {
    let mut r = Rig::new();
    let a = r.feed(TermInput::Screen {
        text: "x".into(),
        caret: Some(0),
    });
    assert!(a.is_empty());
}

// ---- typing -------------------------------------------------------------------------------

#[test]
fn a_typed_character_is_spoken_by_the_key_and_the_shell_echo_is_silent() {
    let mut r = Rig::new();
    r.focus("me@box:~$ ", 10);
    let k = r.key(TermKey::Typed('l'));
    assert_eq!(says(&k), vec![("l".to_owned(), Urgency::Key)]);
    assert!(claimed(&k));
    let e = r.echo_insert(10, "l");
    assert!(says(&e).is_empty(), "{e:?}");
    let c = r.s.counters();
    assert_eq!((c.typed_echoed, c.typed_explained), (1, 1));
    assert_eq!(
        c.settled_local, 1,
        "the payload kept the live screen true: no read"
    );
}

#[test]
fn a_burst_of_typed_characters_is_explained_by_one_echo() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    for c in ['l', 's'] {
        r.key(TermKey::Typed(c));
    }
    let e = r.echo_insert(2, "ls");
    assert!(says(&e).is_empty(), "{e:?}");
    assert_eq!(r.s.counters().typed_explained, 2);
}

#[test]
fn text_the_keys_do_not_explain_is_spoken_once() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    // A paste: no key typed it.
    let e = r.echo_insert(2, "echo hi");
    assert_eq!(texts(&e), vec!["echo hi".to_owned()]);
}

#[test]
fn an_echo_that_never_comes_does_not_explain_a_later_insert() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    r.key(TermKey::Typed('x'));
    r.wait(2000);
    let mut e = r.echo_insert(2, "x");
    e.extend(r.wait(100));
    assert_eq!(texts(&e), vec!["x".to_owned()]);
}

#[test]
fn a_typed_character_wrapping_to_the_next_row_is_still_explained() {
    let mut r = Rig::new();
    let first = format!("$ {}", "a".repeat(78));
    r.focus(&first, 80);
    r.key(TermKey::Typed('b'));
    // VTE joins rows with a newline; the new character lands on the next row.
    let e = r.echo_insert(80, "\nb");
    assert!(says(&e).is_empty(), "{e:?}");
}

#[test]
fn space_and_punctuation_are_typed_characters_too() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    let k = r.key(TermKey::Typed(' '));
    assert_eq!(texts(&k), vec![" ".to_owned()]);
    let k = r.key(TermKey::Typed('|'));
    assert_eq!(texts(&k), vec!["|".to_owned()]);
}

// ---- erasing and moving ---------------------------------------------------------------------

#[test]
fn backspace_speaks_the_character_it_removes_and_the_redraw_is_silent() {
    let mut r = Rig::new();
    r.focus("$ ls", 4);
    let k = r.key(TermKey::Backspace);
    assert_eq!(says(&k), vec![("s".to_owned(), Urgency::Key)]);
    assert!(claimed(&k));
    // The shell redraws: one character fewer.
    r.screen = "$ l".to_owned();
    r.caret = 3;
    r.feed(TermInput::Edit(TermEdit::Delete { offset: 3, len: 1 }));
    r.feed(TermInput::Caret(3));
    let a = r.wait(31);
    assert!(says(&a).is_empty(), "{a:?}");
}

#[test]
fn backspace_at_the_prompt_boundary_says_nothing() {
    let mut r = Rig::new();
    r.focus("me@box:~$ ", 10);
    let k = r.key(TermKey::Backspace);
    assert!(says(&k).is_empty());
    assert!(claimed(&k));
}

#[test]
fn backspace_cancels_a_character_whose_echo_has_not_come() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    r.key(TermKey::Typed('q'));
    let k = r.key(TermKey::Backspace);
    assert_eq!(texts(&k), vec!["q".to_owned()]);
    // Typed and erased before the screen changed: no change at all, no stale pending character.
    let mut e = r.echo_insert(2, "z");
    e.extend(r.wait(100));
    assert_eq!(texts(&e), vec!["z".to_owned()]);
}

#[test]
fn delete_speaks_the_character_under_the_caret() {
    let mut r = Rig::new();
    r.focus("$ abc", 3);
    let k = r.key(TermKey::Delete);
    assert_eq!(texts(&k), vec!["b".to_owned()]);
}

#[test]
fn an_arrow_key_speaks_the_character_the_caret_lands_on_with_no_read() {
    let mut r = Rig::new();
    r.focus("$ abc", 5);
    r.key(TermKey::CharLeft);
    let a = r.feed(TermInput::Caret(4));
    assert_eq!(says(&a), vec![("c".to_owned(), Urgency::Key)]);
    assert!(a.iter().all(|x| !matches!(x, TermAction::FetchScreen)));
    // The key was consumed: a second caret event (a program moving the cursor) is silent.
    let a = r.feed(TermInput::Caret(3));
    assert!(says(&a).is_empty());
}

#[test]
fn home_and_end_speak_the_character_at_the_caret_or_blank_past_the_end() {
    let mut r = Rig::new();
    r.focus("me@box:~$ abc", 13);
    r.key(TermKey::LineStart);
    let a = r.feed(TermInput::Caret(10));
    assert_eq!(texts(&a), vec!["a".to_owned()]);
    r.key(TermKey::LineEnd);
    let a = r.feed(TermInput::Caret(13));
    assert_eq!(texts(&a), vec!["blank".to_owned()]);
}

#[test]
fn a_word_motion_speaks_the_word() {
    let mut r = Rig::new();
    r.focus("$ git commit now", 16);
    r.key(TermKey::WordLeft);
    let a = r.feed(TermInput::Caret(13));
    assert_eq!(texts(&a), vec!["now".to_owned()]);
    r.key(TermKey::WordLeft);
    let a = r.feed(TermInput::Caret(6));
    assert_eq!(texts(&a), vec!["commit".to_owned()]);
}

#[test]
fn a_word_motion_speaks_the_word_passed_without_its_punctuation() {
    // readline's forward-word stops at the end of "usr" in a path; the shared presenter speaks
    // the word, not the whole whitespace-delimited token (the unified editing model).
    let mut r = Rig::new();
    r.focus("$ ls /usr/local/bin", 5);
    r.key(TermKey::WordRight);
    let a = r.feed(TermInput::Caret(9));
    assert_eq!(texts(&a), vec!["usr".to_owned()]);
    r.key(TermKey::WordRight);
    let a = r.feed(TermInput::Caret(15));
    assert_eq!(texts(&a), vec!["local".to_owned()]);
    r.key(TermKey::WordLeft);
    let a = r.feed(TermInput::Caret(10));
    assert_eq!(texts(&a), vec!["local".to_owned()]);
}

#[test]
fn a_word_motion_that_ends_at_the_end_of_the_line_speaks_the_last_word() {
    let mut r = Rig::new();
    r.focus("$ echo hello, world", 7);
    r.key(TermKey::WordRight);
    let a = r.feed(TermInput::Caret(12));
    assert_eq!(texts(&a), vec!["hello".to_owned()]);
    r.key(TermKey::WordRight);
    let a = r.feed(TermInput::Caret(19));
    assert_eq!(texts(&a), vec!["world".to_owned()]);
}

#[test]
fn a_motion_key_that_moves_nothing_speaks_the_unit_at_the_caret_after_the_edge_time() {
    let mut r = Rig::new();
    r.focus("$ abc def", 2);
    // Left at the start of the command (readline does not move): no caret event.
    r.key(TermKey::CharLeft);
    assert!(texts(&r.wait(50)).is_empty(), "not yet");
    assert_eq!(texts(&r.wait(60)), vec!["a".to_owned()]);
    // Ctrl+Left at the start speaks the word at the caret, Home the character.
    r.key(TermKey::WordLeft);
    assert_eq!(texts(&r.wait(110)), vec!["abc".to_owned()]);
    r.key(TermKey::LineStart);
    assert_eq!(texts(&r.wait(110)), vec!["a".to_owned()]);
    // A real move is answered once and the edge timer stays quiet.
    r.key(TermKey::CharRight);
    assert_eq!(texts(&r.feed(TermInput::Caret(3))), vec!["b".to_owned()]);
    assert!(texts(&r.wait(200)).is_empty());
}

#[test]
fn a_slow_caret_event_is_the_only_answer_to_a_key_that_can_move() {
    // Over a slow link the caret event comes 100-250 ms after the key. The key could move the
    // caret, so nothing is said for the old position: exactly one utterance, the new unit.
    let mut r = Rig::new();
    r.focus("$ abc def", 2);
    r.key(TermKey::CharRight);
    assert!(
        texts(&r.wait(120)).is_empty(),
        "the edge timer must not read the old caret"
    );
    assert_eq!(texts(&r.feed(TermInput::Caret(3))), vec!["b".to_owned()]);
    assert!(texts(&r.wait(300)).is_empty(), "spoken once");
    // The caret event never comes (a lost move): the long timer speaks the unit at the caret.
    r.key(TermKey::CharRight);
    assert!(texts(&r.wait(120)).is_empty());
    assert_eq!(texts(&r.wait(130)), vec!["b".to_owned()]);
}

#[test]
fn a_key_that_cannot_move_speaks_at_the_short_edge_time() {
    // Left at the start of the command and Right behind the last character move nothing.
    let mut r = Rig::new();
    r.focus("$ abc", 2);
    r.key(TermKey::CharLeft);
    assert_eq!(texts(&r.wait(110)), vec!["a".to_owned()]);
    let mut r = Rig::new();
    r.focus("$ abc", 5);
    r.key(TermKey::CharRight);
    assert_eq!(texts(&r.wait(110)), vec!["blank".to_owned()]);
}

#[test]
fn a_caret_without_a_key_is_silent() {
    let mut r = Rig::new();
    r.focus("$ abc", 5);
    assert!(says(&r.feed(TermInput::Caret(3))).is_empty());
}

#[test]
fn an_old_key_no_longer_explains_a_caret_move() {
    let mut r = Rig::new();
    r.focus("$ abc", 5);
    r.key(TermKey::CharLeft);
    r.now += 3000;
    assert!(says(&r.feed(TermInput::Caret(4))).is_empty());
}

// ---- history, completion, kill -----------------------------------------------------------------

#[test]
fn history_recall_reads_the_command_without_the_prompt() {
    let mut r = Rig::new();
    r.focus("out\nme@box:~$ ", 14);
    let k = r.key(TermKey::LineUp);
    assert!(claimed(&k));
    let a = r.repaint("out\nme@box:~$ git status", 24);
    assert_eq!(says(&a), vec![("git status".to_owned(), Urgency::Key)]);
    assert_eq!(r.s.counters().history_read, 1);
}

#[test]
fn history_recall_to_an_empty_line_says_blank() {
    let mut r = Rig::new();
    r.focus("out\nme@box:~$ ls", 16);
    r.key(TermKey::LineDown);
    let a = r.repaint("out\nme@box:~$ ", 14);
    assert_eq!(texts(&a), vec!["blank".to_owned()]);
}

#[test]
fn tab_completion_reads_the_completed_word() {
    let mut r = Rig::new();
    r.focus("$ gi", 4);
    r.key(TermKey::Tab);
    let a = r.echo_insert(4, "t ");
    assert_eq!(says(&a), vec![("git".to_owned(), Urgency::Key)]);
}

#[test]
fn tab_that_lists_candidates_reads_them_as_output() {
    let mut r = Rig::new();
    r.focus("$ ls f", 6);
    r.key(TermKey::Tab);
    let a = r.repaint("$ ls f\nfoo  fob\n$ ls f", 6 + 1 + 8 + 1 + 6);
    // The old prompt line is unchanged, the candidates and the repeated prompt are new.
    let t = texts(&a);
    assert_eq!(t.len(), 1, "{t:?}");
    assert!(t[0].starts_with("foo  fob"), "{t:?}");
}

#[test]
fn a_kill_of_several_characters_is_spoken_and_a_single_erase_by_key_is_not_doubled() {
    let mut r = Rig::new();
    r.focus("$ git status", 12);
    r.key(TermKey::Other);
    let a = r.repaint("$ git ", 6);
    assert_eq!(texts(&a), vec!["status".to_owned()]);
}

// ---- program output ---------------------------------------------------------------------------

#[test]
fn command_output_is_read_once_in_order_with_the_next_prompt() {
    let mut r = Rig::new();
    r.focus("$ ls", 4);
    r.key(TermKey::Enter);
    let a = r.repaint("$ ls\nalpha  beta\ngamma\n$ ", 26);
    assert_eq!(
        says(&a),
        vec![("alpha  beta\ngamma\n$".to_owned(), Urgency::Output)]
    );
    let c = r.s.counters();
    assert_eq!((c.output_spoken, c.output_lines_spoken), (1, 3));
}

#[test]
fn enter_alone_is_claimed_and_silent() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    let k = r.key(TermKey::Enter);
    assert!(claimed(&k));
    assert!(says(&k).is_empty());
}

#[test]
fn output_that_scrolls_the_screen_speaks_only_the_new_lines() {
    let mut r = Rig::new();
    let old = lines(0..24);
    r.focus(&old, 0);
    let new = lines(3..27);
    let a = r.repaint(&new, 0);
    assert_eq!(texts(&a), vec!["line 24\nline 25\nline 26".to_owned()]);
}

#[test]
fn identical_scrolling_output_like_yes_says_nothing() {
    let mut r = Rig::new();
    let y: String = "y\n".repeat(24);
    r.focus(&y, 0);
    for _ in 0..10 {
        assert!(says(&r.repaint(&y, 0)).is_empty());
    }
}

#[test]
fn a_flood_speaks_only_the_newest_lines_and_never_queues() {
    let mut r = Rig::new();
    r.focus(&lines(0..24), 0);
    let mut spoken = 0;
    let mut longest = 0;
    for i in 1..60 {
        let a = r.repaint(&lines(i * 24..i * 24 + 24), 0);
        for (t, _) in says(&a) {
            spoken += 1;
            longest = longest.max(t.lines().count());
        }
    }
    // The first reading is a whole screen; once it is a flood only the tail is said, and the
    // pacing keeps most readings silent.
    assert!(spoken >= 1);
    assert!(longest <= 24, "{longest}");
    let c = r.s.counters();
    assert!(c.output_lines_skipped > 500, "{c:?}");
    assert!(r.s.outq.len() <= 4, "{}", r.s.outq.len());
}

#[test]
fn pacing_holds_output_back_until_the_estimate_of_the_speech_has_passed() {
    let mut r = Rig::new();
    let mut screen = String::from("$ run\n");
    r.focus(&screen, 6);
    r.key(TermKey::Enter);
    screen.push_str("first line of output\n");
    let a = r.repaint(&screen, 0);
    assert_eq!(texts(&a), vec!["first line of output".to_owned()]);
    screen.push_str("second\n");
    let a = r.repaint(&screen, 0);
    assert!(says(&a).is_empty(), "paced: {a:?}");
    assert!(
        a.iter().any(|x| matches!(x, TermAction::ArmTimer(_))),
        "{a:?}"
    );
    let a = r.wait(2600);
    assert_eq!(texts(&a), vec!["second".to_owned()]);
}

#[test]
fn ctrl_c_drops_what_waits_and_the_next_prompt_is_spoken() {
    let mut r = Rig::new();
    r.focus(&lines(0..24), 0);
    for i in 1..6 {
        r.repaint(&lines(i * 24..i * 24 + 24), 0);
    }
    assert!(!r.s.outq.is_empty() || r.s.out_gate > r.now);
    let k = r.key(TermKey::Interrupt);
    assert!(claimed(&k));
    assert!(r.s.outq.is_empty());
    r.now += 3000;
    let a = r.repaint(&format!("{}^C\nme@box:~$ ", lines(300..322)), 0);
    assert_eq!(texts(&a), vec!["^C\nme@box:~$".to_owned()]);
}

#[test]
fn a_page_of_less_scrolling_back_reads_the_lines_that_came_in_at_the_top() {
    let mut r = Rig::new();
    r.focus(&lines(10..34), 0);
    let a = r.repaint(&lines(7..31), 0);
    assert_eq!(texts(&a), vec!["line 7\nline 8\nline 9".to_owned()]);
}

#[test]
fn a_full_screen_repaint_is_silent_and_counted() {
    let mut r = Rig::new();
    let top = |a: u32, b: u32, c: u32| {
        format!("CPU {a}\nstatic one\nMEM {b}\nstatic two\nSWP {c}\nstatic three\n")
    };
    r.focus(&top(1, 2, 3), 0);
    let a = r.repaint(&top(5, 6, 7), 0);
    assert!(says(&a).is_empty(), "{a:?}");
    assert_eq!(r.s.counters().repaints, 1);
}

#[test]
fn a_progress_line_updating_in_place_is_read() {
    let mut r = Rig::new();
    r.focus("get file\n10%\n$ ", 14);
    let a = r.repaint("get file\n55%\n$ ", 14);
    assert_eq!(texts(&a), vec!["55%".to_owned()]);
}

// ---- password prompts ----------------------------------------------------------------------------

#[test]
fn a_password_prompt_makes_typed_characters_sensitive_and_unremembered() {
    let mut r = Rig::new();
    let prompt = "$ sudo ls\n[sudo] password for me: ";
    r.focus(prompt, prompt.chars().count() as u32);
    let k = r.key(TermKey::Typed('h'));
    let [
        TermAction::ClaimKey,
        TermAction::Say {
            sensitive, text, ..
        },
        ..,
    ] = k.as_slice()
    else {
        panic!("{k:?}");
    };
    assert!(*sensitive);
    assert_eq!(text, "h");
    assert!(r.s.pending.is_empty(), "a secret is not kept");
    // Enter ends the secret, and the screen moves on.
    r.key(TermKey::Enter);
    assert!(!r.s.secret);
    r.repaint(&format!("{prompt}\nme@box:~$ "), 45);
    assert!(!r.s.secret);
    let k = r.key(TermKey::Typed('x'));
    assert!(matches!(
        k[1],
        TermAction::Say {
            sensitive: false,
            ..
        }
    ));
}

#[test]
fn the_prompt_itself_is_spoken_and_not_marked_sensitive() {
    let mut r = Rig::new();
    r.focus("$ sudo ls", 9);
    r.key(TermKey::Enter);
    let a = r.repaint("$ sudo ls\n[sudo] password for me: ", 34);
    let said: Vec<_> = a
        .iter()
        .filter(|x| matches!(x, TermAction::Say { .. }))
        .collect();
    let [
        TermAction::Say {
            text, sensitive, ..
        },
    ] = said.as_slice()
    else {
        panic!("{a:?}");
    };
    assert_eq!(text, "[sudo] password for me:");
    assert!(!*sensitive);
    assert!(r.s.secret);
    assert_eq!(r.s.counters().secret_prompts, 1);
}

#[test]
fn a_character_typed_just_before_the_settle_still_finds_the_prompt() {
    let mut r = Rig::new();
    r.focus("$ sudo ls", 9);
    r.key(TermKey::Enter);
    // The prompt arrived as an edit the live screen applied, but no settle yet.
    r.screen = "$ sudo ls\n[sudo] password for me: ".to_owned();
    r.caret = 34;
    let _ = r.feed(TermInput::Edit(TermEdit::Replaced));
    let a = r.wait(31);
    assert!(r.s.secret, "{a:?}");
}

#[test]
fn blur_wipes_the_secret_and_the_screens() {
    let mut r = Rig::new();
    r.focus("Password: ", 10);
    assert!(r.s.secret);
    r.feed(TermInput::Blur);
    assert!(!r.s.secret);
    assert!(r.s.base.is_empty() && r.s.live.is_empty());
    assert!(r.s.pending.is_empty());
}

#[test]
fn backspace_in_a_secret_says_nothing() {
    let mut r = Rig::new();
    r.focus("Password: ", 10);
    r.key(TermKey::Typed('a'));
    let k = r.key(TermKey::Backspace);
    assert!(says(&k).is_empty());
}

// ---- timers and failures ---------------------------------------------------------------------

#[test]
fn one_timer_per_deadline_and_a_tick_before_it_does_nothing() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    let a = r.feed(TermInput::Edit(TermEdit::Replaced));
    let timers: Vec<_> = a
        .iter()
        .filter(|x| matches!(x, TermAction::ArmTimer(_)))
        .collect();
    assert_eq!(timers.len(), 1);
    let a = r.feed(TermInput::Edit(TermEdit::Replaced));
    // Same quiet window moved by zero ms: the deadline is unchanged, no second timer.
    assert!(
        a.iter().all(|x| !matches!(x, TermAction::ArmTimer(_))),
        "{a:?}"
    );
    r.now += 10;
    let a = r.feed(TermInput::Tick);
    assert!(
        a.iter().all(|x| !matches!(x, TermAction::FetchScreen)),
        "{a:?}"
    );
}

#[test]
fn a_continuous_flood_is_read_at_the_cap() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    r.feed(TermInput::Edit(TermEdit::Replaced));
    let mut fetched_at = None;
    for _ in 0..40 {
        r.now += 10;
        let a = r.feed(TermInput::Edit(TermEdit::Replaced));
        if a.iter().any(|x| matches!(x, TermAction::FetchScreen)) {
            fetched_at = Some(r.now);
        }
        let a = r.feed(TermInput::Tick);
        if a.iter().any(|x| matches!(x, TermAction::FetchScreen)) {
            fetched_at = Some(r.now);
            break;
        }
    }
    let at = fetched_at.unwrap_or(0);
    assert!(
        (1200..=1230).contains(&at),
        "read at {at}, cap 200 ms after 1000"
    );
}

#[test]
fn a_lost_read_is_retried_twice_and_then_given_up() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    r.feed(TermInput::Edit(TermEdit::Replaced));
    let mut fetches = 0;
    for _ in 0..8 {
        r.now += 500;
        let a = r.feed(TermInput::Tick);
        fetches += a
            .iter()
            .filter(|x| matches!(x, TermAction::FetchScreen))
            .count();
    }
    assert_eq!(fetches, 3, "the first read and two retries");
    assert_eq!(r.s.phase, Phase::Idle);
    assert_eq!(r.s.counters().fetch_failed, 3);
}

#[test]
fn a_failed_read_that_the_host_reports_is_counted_and_retried() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    r.feed(TermInput::Edit(TermEdit::Replaced));
    r.now += 40;
    let a = r.feed(TermInput::Tick);
    assert!(a.iter().any(|x| matches!(x, TermAction::FetchScreen)));
    r.feed(TermInput::ScreenFailed);
    assert_eq!(r.s.counters().fetch_failed, 1);
    assert!(matches!(r.s.phase, Phase::Settling { .. }));
}

#[test]
fn a_fetched_screen_that_differs_from_the_live_one_is_counted() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    // An insert with a payload whose length lies: the live screen is then untrusted, so no
    // mismatch is counted but a read is made.
    let a = r.feed(TermInput::Edit(TermEdit::Insert {
        offset: 2,
        len: 5,
        text: Some("ab".into()),
    }));
    assert!(a.iter().any(|x| matches!(x, TermAction::ArmTimer(_))));
    r.screen = "$ ab".into();
    r.caret = 4;
    let a = r.wait(31);
    assert_eq!(r.s.counters().resync_mismatch, 0);
    assert_eq!(r.s.counters().fetches, 2, "{a:?}");
}

// ---- hostile input ------------------------------------------------------------------------------

#[test]
fn offsets_outside_the_screen_never_panic_and_force_a_read() {
    let mut r = Rig::new();
    r.focus("$ ls\nout", 4);
    for e in [
        TermEdit::Insert {
            offset: u32::MAX,
            len: 3,
            text: Some("abc".into()),
        },
        TermEdit::Delete {
            offset: u32::MAX,
            len: u32::MAX,
        },
        TermEdit::Delete {
            offset: 3,
            len: 9999,
        },
        TermEdit::Insert {
            offset: 0,
            len: u32::MAX,
            text: None,
        },
    ] {
        r.feed(TermInput::Edit(e));
        assert!(!r.s.live_ok);
        r.screen = "$ ls\nout".into();
        r.wait(31);
        assert!(r.s.live_ok);
    }
}

#[test]
fn a_giant_screen_is_cut_and_a_caret_past_the_end_is_clamped() {
    let mut r = Rig::new();
    let big = "x".repeat(3 * 1024 * 1024);
    r.focus(&big, u32::MAX);
    assert!(r.s.base.text().len() <= screen::MAX_SCREEN_BYTES);
    let k = r.key(TermKey::Backspace);
    assert!(claimed(&k));
}

#[test]
fn scrambled_input_never_panics_and_memory_stays_bounded() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    let mut x: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut next = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    for _ in 0..20_000 {
        let n = next();
        let input = match n % 11 {
            0 => TermInput::Key(TermKey::Typed(
                char::from_u32((next() % 0x2fff) as u32).unwrap_or('a'),
            )),
            1 => TermInput::Key(match next() % 8 {
                0 => TermKey::Enter,
                1 => TermKey::Backspace,
                2 => TermKey::Tab,
                3 => TermKey::LineUp,
                4 => TermKey::Interrupt,
                5 => TermKey::WordLeft,
                6 => TermKey::Delete,
                _ => TermKey::Other,
            }),
            2 => TermInput::Edit(TermEdit::Replaced),
            3 => TermInput::Edit(TermEdit::Insert {
                offset: (next() % 300) as u32,
                len: (next() % 5) as u32,
                text: Some("ab\n".repeat((next() % 3) as usize)),
            }),
            4 => TermInput::Edit(TermEdit::Delete {
                offset: (next() % 300) as u32,
                len: (next() % 40) as u32,
            }),
            5 => TermInput::Caret((next() % 400) as u32),
            6 => TermInput::Screen {
                text: lines(0..(next() % 60) as usize),
                caret: Some((next() % 500) as u32),
            },
            7 => TermInput::ScreenFailed,
            8 => TermInput::Focus,
            9 => TermInput::Blur,
            _ => TermInput::Tick,
        };
        r.now += next() % 60;
        let mut out = Vec::new();
        r.s.step(r.now, input, &mut out);
        assert!(r.s.pending.len() <= PENDING_MAX);
        assert!(r.s.outq.len() <= OUT_LINES_MAX + 64);
    }
}

#[test]
fn the_extension_wrapper_drives_the_same_session() {
    let mut e = crate::VteExtension::default();
    let mut out = Vec::new();
    e.step(0, TermInput::Key(TermKey::Typed('a')), &mut out);
    assert!(claimed(&out));
    assert_eq!(e.counters().typed_echoed, 1);
}

#[test]
fn output_still_flowing_after_ctrl_c_is_not_read_until_it_stops() {
    let mut r = Rig::new();
    r.focus(&lines(0..24), 0);
    r.key(TermKey::Interrupt);
    let mut said = Vec::new();
    // The program needs a moment to die: frames keep coming every 16 ms for a second.
    for i in 1..60 {
        r.screen = lines(i * 24..i * 24 + 24);
        let mut acts = r.feed(TermInput::Edit(TermEdit::Replaced));
        acts.extend(r.wait(16));
        said.extend(texts(&acts));
    }
    assert!(said.is_empty(), "flood leftovers were spoken: {said:?}");
    assert!(
        r.s.counters().fetches >= 3,
        "the screen was still being read"
    );
    // It stops, and the shell prints ^C and the prompt.
    r.screen = format!("{}^C\nme@box:~$ ", lines(2000..2022));
    let a = r.feed(TermInput::Edit(TermEdit::Replaced));
    let mut acts = a;
    acts.extend(r.wait(31));
    assert_eq!(texts(&acts), vec!["^C\nme@box:~$".to_owned()]);
}

#[test]
fn ctrl_c_at_an_idle_prompt_does_not_arm_the_tail_rule_for_later_output() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    r.key(TermKey::Interrupt);
    r.now += 6000;
    r.screen = "$ \na\nb\nc\nd\n$ ".to_owned();
    let mut acts = r.feed(TermInput::Edit(TermEdit::Replaced));
    acts.extend(r.wait(31));
    assert_eq!(texts(&acts), vec!["a\nb\nc\nd\n$".to_owned()]);
}

#[test]
fn a_listing_that_ends_in_a_prompt_equal_to_an_old_one_is_output_not_a_repaint() {
    let mut r = Rig::new();
    r.focus("r1\nr2\nr3\nme@box:~$ ^C\nme@box:~$", 33);
    r.key(TermKey::Enter);
    // The screen scrolled: the old last prompt is now the first row, the listing follows, and
    // the new prompt looks exactly like the old one.
    let a = r.repaint("me@box:~$\nf1\nf2\nf3\nme@box:~$", 28);
    assert_eq!(texts(&a), vec!["f1\nf2\nf3\nme@box:~$".to_owned()]);
    assert_eq!(r.s.counters().repaints, 0);
}

#[test]
fn a_keypress_drops_output_that_was_waiting_for_its_turn() {
    let mut r = Rig::new();
    let mut screen = String::from("$ run\n");
    r.focus(&screen, 6);
    r.key(TermKey::Enter);
    screen.push_str("first line of output\n");
    let a = r.repaint(&screen, 0);
    assert_eq!(texts(&a).len(), 1);
    screen.push_str("second\n");
    r.repaint(&screen, 0);
    assert!(!r.s.outq.is_empty(), "paced behind the first");
    let k = r.key(TermKey::Typed('x'));
    assert!(r.s.outq.is_empty(), "stale after a key press: {k:?}");
    assert_eq!(r.s.out_gate, 0);
}

impl Rig {
    /// Advance the clock `ms` milliseconds the way the host would: ticks at the armed deadline,
    /// a screen read answered two milliseconds after it was asked for. Returns every action.
    fn run(&mut self, ms: Ms) -> Vec<TermAction> {
        let mut all = Vec::new();
        let end = self.now + ms;
        while self.now < end {
            self.now += 1;
            if self.read_due.is_some_and(|d| d <= self.now) {
                self.read_due = None;
                let a = self.feed(TermInput::Screen {
                    text: self.screen.clone(),
                    caret: Some(self.caret),
                });
                self.collect(a, &mut all);
            }
            if self.tick_due.is_some_and(|d| d <= self.now) {
                self.tick_due = None;
                let a = self.feed(TermInput::Tick);
                self.collect(a, &mut all);
            }
        }
        all
    }

    /// [`Self::feed`] that also arms the simulated timer and read.
    fn send(&mut self, input: TermInput) -> Vec<TermAction> {
        let a = self.feed(input);
        let mut all = Vec::new();
        self.collect(a, &mut all);
        all
    }

    fn collect(&mut self, acts: Vec<TermAction>, all: &mut Vec<TermAction>) {
        for a in &acts {
            match a {
                TermAction::FetchScreen => self.read_due = Some(self.now + 2),
                TermAction::ArmTimer(t) => {
                    self.tick_due = Some(self.tick_due.map_or(*t, |d| d.min(*t)));
                }
                TermAction::Say { .. } | TermAction::ClaimKey => {}
            }
        }
        all.extend(acts);
    }

    /// The user types `s`, one key every `gap` ms, and the shell echoes each at once with the
    /// payload; nothing waits for a settle.
    fn type_fast(&mut self, s: &str, gap: Ms) -> Vec<TermAction> {
        let mut all = Vec::new();
        for ch in s.chars() {
            let a = self.key(TermKey::Typed(ch));
            self.collect(a, &mut all);
            self.now += 2;
            let at = self.caret;
            let mut text = self.screen.clone();
            let byte = text
                .char_indices()
                .nth(at as usize)
                .map_or(text.len(), |(b, _)| b);
            text.insert(byte, ch);
            self.screen = text;
            self.caret = at + 1;
            let a = self.feed(TermInput::Edit(TermEdit::Insert {
                offset: at,
                len: 1,
                text: Some(ch.to_string()),
            }));
            self.collect(a, &mut all);
            let a = self.feed(TermInput::Caret(self.caret));
            self.collect(a, &mut all);
            let a = self.run(gap - 2);
            all.extend(a);
        }
        all
    }
}

#[test]
fn fast_typing_after_output_is_explained_by_the_keys_not_read_again() {
    let mut r = Rig::new();
    r.focus("me@box:~$ ls", 12);
    r.key(TermKey::Enter);
    let a = r.repaint("me@box:~$ ls\nout\nme@box:~$", 26);
    assert_eq!(texts(&a).len(), 1);
    r.run(3000);
    let a = r.type_fast("x2wrmdfw", 28);
    let a2 = r.run(400);
    let said = texts(&a);
    let said2 = texts(&a2);
    let expect: Vec<String> = "x2wrmdfw".chars().map(|c| c.to_string()).collect();
    assert_eq!(said, expect, "one echo per key, nothing else");
    assert!(said2.is_empty(), "the shell echo is silent: {said2:?}");
}

#[test]
fn a_key_that_comes_after_its_echo_is_not_spoken_twice() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    // The screen changes first; the key follows 20 ms later.
    let e = r.echo_insert(2, "q");
    assert!(says(&e).is_empty(), "held for its key: {e:?}");
    r.now += 20;
    let k = r.key(TermKey::Typed('q'));
    assert_eq!(texts(&k), vec!["q".to_owned()]);
    let after = r.wait(200);
    assert!(says(&after).is_empty(), "{after:?}");
    assert_eq!(r.s.counters().typed_explained, 1);
}

#[test]
fn a_leftover_typed_character_does_not_block_the_ones_after_it() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    // 'z' is typed but its echo never changes the screen (a program that swallowed it).
    r.key(TermKey::Typed('z'));
    for (i, c) in "abc".chars().enumerate() {
        r.key(TermKey::Typed(c));
        r.echo_insert(2 + u32::try_from(i).unwrap_or(0), &c.to_string());
    }
    let after = r.wait(200);
    assert!(says(&after).is_empty(), "{after:?}");
}

#[test]
fn a_motion_key_pressed_while_the_last_echo_settles_is_read_at_the_new_caret() {
    let mut r = Rig::new();
    r.focus("$ ab", 4);
    let mut said = texts(&r.key(TermKey::Typed('c')));
    r.run(10);
    // The echo of 'c': the screen and the caret change.
    r.screen = "$ abc".to_owned();
    r.caret = 5;
    said.extend(texts(&r.send(TermInput::Edit(TermEdit::Insert {
        offset: 4,
        len: 1,
        text: Some("c".to_owned()),
    }))));
    said.extend(texts(&r.send(TermInput::Caret(5))));
    r.run(18);
    // Left, while the settle of that echo is still pending; the caret follows ten ms later.
    said.extend(texts(&r.key(TermKey::CharLeft)));
    r.run(10);
    r.caret = 4;
    said.extend(texts(&r.send(TermInput::Caret(4))));
    said.extend(texts(&r.run(300)));
    assert_eq!(
        said,
        vec!["c".to_owned(), "c".to_owned()],
        "key echo, then the character"
    );
}

#[test]
fn an_echo_that_a_later_erase_took_away_is_not_read_as_text_nobody_typed() {
    let mut r = Rig::new();
    r.focus("$ ", 2);
    // The echo of 'q' is seen before its key (held), then the line is erased again.
    let mut said = texts(&r.echo_insert(2, "q"));
    r.screen = "$".to_owned();
    r.caret = 2;
    said.extend(texts(
        &r.send(TermInput::Edit(TermEdit::Delete { offset: 2, len: 1 })),
    ));
    said.extend(texts(&r.send(TermInput::Caret(2))));
    said.extend(texts(&r.run(400)));
    assert!(said.is_empty(), "{said:?}");
}

#[test]
fn ctrl_c_in_a_screen_of_identical_lines_still_says_the_prompt() {
    let mut r = Rig::new();
    let y = "y\n".repeat(29);
    r.focus(&y, 58);
    for _ in 0..5 {
        r.run(30);
        r.send(TermInput::Edit(TermEdit::Replaced));
        r.run(250);
    }
    r.key(TermKey::Interrupt);
    r.screen = format!("{}^C\n$ \n", "y\n".repeat(28));
    r.caret = u32::try_from(r.screen.chars().count()).unwrap_or(0) - 1;
    let mut said = texts(&r.send(TermInput::Edit(TermEdit::Replaced)));
    said.extend(texts(&r.run(400)));
    assert_eq!(said, vec!["^C\n$".to_owned()]);
}

#[test]
fn two_motion_keys_whose_caret_events_were_folded_into_one_say_the_last_answer() {
    let mut r = Rig::new();
    r.focus("$ ab", 4);
    r.key(TermKey::CharLeft);
    r.now += 20;
    r.key(TermKey::CharRight);
    // The ingress kept only the newest caret: it is where the caret already was.
    let a = r.feed(TermInput::Caret(4));
    assert_eq!(texts(&a), vec!["blank".to_owned()]);
    assert!(r.s.motions.is_empty());
}

// ---- large screens and multibyte content (m6-review finding 1) --------------------------------

/// A full-window screen as VTE serves it: every row plus "\n", empty rows included. Row `r`
/// starts with `tag(r)` and is padded with `fill` to `cols` scalars; row `prompt_row` is `prompt`.
fn big_screen(rows: usize, cols: usize, fill: char, prompt_row: usize, prompt: &str) -> String {
    let mut s = String::new();
    for r in 0..rows {
        let line = if r == prompt_row {
            prompt.to_owned()
        } else {
            let tag = format!("row {r:02} ");
            let pad = cols.saturating_sub(tag.chars().count());
            format!("{tag}{}", fill.to_string().repeat(pad))
        };
        s.push_str(&line);
        s.push('\n');
    }
    s
}

/// Scalar offset of (`row`, `col`) in a screen of equal-width rows.
fn offset_of(screen: &str, row: usize, col: usize) -> u32 {
    let before: usize = screen
        .split('\n')
        .take(row)
        .map(|l| l.chars().count() + 1)
        .sum();
    (before + col) as u32
}

#[test]
fn focus_on_a_200x60_screen_reads_the_caret_row() {
    let prompt = "me@box:~$ ls -la";
    let screen = big_screen(60, 200, '.', 30, prompt);
    assert!(screen.len() > 11_000);
    let caret = offset_of(&screen, 30, prompt.chars().count());
    let mut r = Rig::new();
    let a = r.focus(&screen, caret);
    assert_eq!(texts(&a), vec![prompt.to_owned()]);
}

#[test]
fn cyrillic_rows_count_scalars_not_bytes() {
    let prompt =
        "\u{43f}\u{43e}\u{43b}\u{44c}\u{437}@\u{445}\u{43e}\u{441}\u{442}:~$ \u{43b}\u{441}";
    let screen = big_screen(50, 120, '\u{44f}', 42, prompt);
    assert!(screen.len() > 10_000);
    let col = prompt.chars().count();
    let caret = offset_of(&screen, 42, col);
    let mut r = Rig::new();
    let a = r.focus(&screen, caret);
    assert_eq!(texts(&a), vec![prompt.to_owned()]);
    // Left reads the character before the caret, Right the one the caret lands on again.
    r.key(TermKey::CharLeft);
    let a = r.feed(TermInput::Caret(caret - 1));
    assert_eq!(texts(&a), vec!["\u{441}".to_owned()]);
    r.key(TermKey::CharLeft);
    let a = r.feed(TermInput::Caret(caret - 2));
    assert_eq!(texts(&a), vec!["\u{43b}".to_owned()]);
    r.key(TermKey::CharRight);
    let a = r.feed(TermInput::Caret(caret - 1));
    assert_eq!(texts(&a), vec!["\u{441}".to_owned()]);
}

#[test]
fn a_password_prompt_deep_in_a_big_cyrillic_screen_is_secret() {
    let prompt = "[sudo] \u{43f}\u{430}\u{440}\u{43e}\u{43b}\u{44c} \u{434}\u{43b}\u{44f} user: ";
    let screen = big_screen(60, 200, '\u{44f}', 55, prompt);
    let caret = offset_of(&screen, 55, prompt.chars().count());
    let mut r = Rig::new();
    r.focus(&screen, caret);
    let a = r.key(TermKey::Typed('q'));
    assert!(
        a.iter().any(|x| matches!(
            x,
            TermAction::Say {
                sensitive: true,
                ..
            }
        )),
        "{a:?}"
    );
    assert!(r.s.secret);
    assert!(r.s.pending.is_empty(), "a secret is not kept");
}

#[test]
fn a_failed_baseline_is_read_again_by_itself_with_a_growing_pause() {
    let mut r = Rig::new();
    r.screen = "$ ".to_owned();
    r.caret = 2;
    let a = r.feed(TermInput::Focus);
    assert!(a.iter().any(|x| matches!(x, TermAction::FetchScreen)));
    let a = r.feed(TermInput::ScreenFailed);
    assert!(
        a.iter()
            .any(|x| matches!(x, TermAction::ArmTimer(t) if *t == r.now + BASE_RETRY_MS)),
        "{a:?}"
    );
    // The retry fails too: the pause doubles; the third read succeeds and speaks the prompt.
    r.now += BASE_RETRY_MS;
    let a = r.feed(TermInput::Tick);
    assert!(a.iter().any(|x| matches!(x, TermAction::FetchScreen)));
    r.feed(TermInput::ScreenFailed);
    assert_eq!(r.s.base_retry, Some(r.now + 2 * BASE_RETRY_MS));
    r.now += 2 * BASE_RETRY_MS;
    let a = r.feed(TermInput::Tick);
    let a = r.answer(a);
    assert_eq!(texts(&a), vec!["$".to_owned()]);
    assert!(r.s.snapshot().is_some());
    assert_eq!(r.s.base_attempts, 0);
}

#[test]
fn baseline_retries_are_bounded() {
    let mut r = Rig::new();
    r.feed(TermInput::Focus);
    r.feed(TermInput::ScreenFailed);
    for _ in 0..BASE_RETRIES_MAX {
        r.now += BASE_RETRY_CAP_MS;
        r.feed(TermInput::Tick);
        r.feed(TermInput::ScreenFailed);
    }
    assert!(r.s.base_retry.is_none(), "gives up and waits for an event");
    assert!(r.s.snapshot().is_none());
}

#[test]
fn output_that_arrives_before_any_baseline_is_spoken_not_swallowed() {
    let mut r = Rig::new();
    r.feed(TermInput::Focus);
    r.feed(TermInput::ScreenFailed);
    // The retries are spent before the output shows up.
    r.s.base_retry = None;
    r.screen = "$ ls\nout\n$ ".to_owned();
    r.caret = 11;
    let a = r.feed(TermInput::Edit(TermEdit::Replaced));
    let a = r.answer(a);
    let spoken = texts(&a).join("|");
    assert!(spoken.contains("out"), "{spoken}");
}

// ---- history recall under many redraw shapes --------------------------------------------------

/// A readline terminal: `hist` rows above the prompt, a prompt, the command, wrapped at `w`
/// columns, on a screen of `h` rows. The prompt stays on the bottom row once the screen is full.
struct Tty {
    w: usize,
    h: usize,
    hist: Vec<String>,
}

const PROMPT: &str = "me@box:~$ ";

impl Tty {
    /// The screen text (every row plus a newline, blank rows included) and the caret offset.
    fn render(&self, cmd: &str) -> (String, u32) {
        let line: Vec<char> = format!("{PROMPT}{cmd}").chars().collect();
        let mut rows = self.hist.clone();
        for ch in line.chunks(self.w) {
            rows.push(ch.iter().collect());
        }
        let last = rows.len() - 1;
        let drop = rows.len().saturating_sub(self.h);
        let caret_row = (last - drop).min(self.h - 1);
        let mut shown: Vec<String> = rows.into_iter().skip(drop).collect();
        while shown.len() < self.h {
            shown.push(String::new());
        }
        let mut off = 0u32;
        let mut text = String::new();
        for (i, r) in shown.iter().enumerate() {
            let r = r.trim_end();
            let n = r.chars().count() as u32;
            if i < caret_row {
                off += n + 1;
            } else if i == caret_row {
                off += n;
            }
            text.push_str(r);
            text.push('\n');
        }
        (text, off)
    }
}

/// The events VTE sends for one redraw, in one of several shapes.
fn redraw_events(old: &str, new: &str, shape: u8, caret: u32) -> Vec<TermInput> {
    let mut ev = Vec::new();
    let o: Vec<char> = old.chars().collect();
    let n: Vec<char> = new.chars().collect();
    match shape {
        0 => ev.push(TermInput::Edit(TermEdit::Replaced)),
        1 => {
            ev.push(TermInput::Edit(TermEdit::Delete {
                offset: 0,
                len: o.len() as u32,
            }));
            ev.push(TermInput::Edit(TermEdit::Insert {
                offset: 0,
                len: n.len() as u32,
                text: Some(new.to_owned()),
            }));
        }
        _ => {
            let m = o.len().min(n.len());
            let mut head = 0;
            while head < m && o[head] == n[head] {
                head += 1;
            }
            let mut tl = 0;
            while tl < m - head && o[o.len() - 1 - tl] == n[n.len() - 1 - tl] {
                tl += 1;
            }
            let del = o.len() - head - tl;
            let ins: String = n[head..n.len() - tl].iter().collect();
            if del > 0 {
                ev.push(TermInput::Edit(TermEdit::Delete {
                    offset: head as u32,
                    len: del as u32,
                }));
            }
            if !ins.is_empty() {
                ev.push(TermInput::Edit(TermEdit::Insert {
                    offset: head as u32,
                    len: ins.chars().count() as u32,
                    text: Some(ins),
                }));
            }
        }
    }
    ev.push(TermInput::Caret(caret));
    ev
}

const RECALL_CMDS: [&str; 10] = [
    "ls",
    "git status",
    "git commit -m \"a fairly long message that wraps past the width\"",
    "echo \u{43f}\u{440}\u{438}\u{432}\u{435}\u{442} \u{43c}\u{438}\u{440}",
    "cat /etc/passwd /etc/group /etc/hosts /etc/fstab /etc/os-release /etc/issue /etc/motd",
    "x",
    "",
    "ls -l",
    "lsa",
    "ls -la /usr/share/doc/some/very/long/path/that/keeps/going/and/going/and/going/on/and/on",
];

/// One recall of `to` over `from` on a screen with `hist_len` rows of output, in redraw `shape`.
fn recall_says(hist_len: usize, shape: u8, from: &str, to: &str) -> Vec<String> {
    let tty = Tty {
        w: 40,
        h: 10,
        hist: (0..hist_len).map(|k| format!("out {k}")).collect(),
    };
    let (s0, c0) = tty.render(from);
    let (s1, c1) = tty.render(to);
    let mut r = Rig::new();
    r.focus(&s0, c0);
    r.key(TermKey::LineUp);
    r.screen = s1.clone();
    r.caret = c1;
    let mut acts = Vec::new();
    for e in redraw_events(&s0, &s1, shape, c1) {
        r.now += 1;
        acts.extend(r.feed(e));
    }
    acts.extend(r.wait(31));
    acts.extend(r.wait(100));
    texts(&acts)
}

#[test]
fn history_recall_is_spoken_once_in_every_redraw_shape() {
    let mut fails = Vec::new();
    for hist_len in [0usize, 3, 9, 30] {
        for shape in 0..3u8 {
            for (i, from) in RECALL_CMDS.iter().enumerate() {
                for to in RECALL_CMDS
                    .iter()
                    .skip(i + 1)
                    .chain(RECALL_CMDS.iter().take(i))
                {
                    if from == to {
                        continue;
                    }
                    let t = recall_says(hist_len, shape, from, to);
                    let want = if to.is_empty() { "blank" } else { to };
                    if t != vec![want.to_owned()] {
                        fails.push(format!("h{hist_len} s{shape} {from:?} -> {to:?}: {t:?}"));
                    }
                }
            }
        }
    }
    assert!(
        fails.is_empty(),
        "{} failures:\n{}",
        fails.len(),
        fails.join("\n")
    );
}

#[test]
fn a_history_key_that_changes_nothing_still_reads_the_line() {
    // The same command twice in the history: readline redraws nothing.
    let mut r = Rig::new();
    r.focus("out\nme@box:~$ ls", 16);
    r.key(TermKey::LineUp);
    let a = r.wait(300);
    assert_eq!(says(&a), vec![("ls".to_owned(), Urgency::Key)]);
    // Once only.
    assert!(texts(&r.wait(1000)).is_empty());
}

#[test]
fn a_recall_is_not_read_twice_when_its_redraw_comes_late() {
    let mut r = Rig::new();
    r.focus("out\nme@box:~$ ", 14);
    r.key(TermKey::LineUp);
    let mut said = texts(&r.wait(100));
    said.extend(texts(&r.repaint("out\nme@box:~$ git status", 24)));
    said.extend(texts(&r.wait(1000)));
    assert_eq!(said, vec!["git status".to_owned()]);
}

#[test]
fn two_quick_history_keys_read_each_recall_once() {
    // The first redraw is read before the second key's redraw arrives.
    let mut r = Rig::new();
    r.focus("out\nme@box:~$ ", 14);
    r.key(TermKey::LineUp);
    r.screen = "out\nme@box:~$ ls\n".to_owned();
    r.caret = 16;
    let mut acts = r.feed(TermInput::Edit(TermEdit::Replaced));
    acts.extend(r.feed(TermInput::Caret(16)));
    // The settle fires and the read is in flight when the second key is pressed.
    r.now += 31;
    acts.extend(r.feed(TermInput::Tick));
    r.key(TermKey::LineUp);
    r.now += 1;
    acts.extend(r.feed(TermInput::Screen {
        text: "out\nme@box:~$ ls\n".to_owned(),
        caret: Some(16),
    }));
    r.screen = "out\nme@box:~$ git status\n".to_owned();
    r.caret = 24;
    acts.extend(r.feed(TermInput::Edit(TermEdit::Replaced)));
    acts.extend(r.feed(TermInput::Caret(24)));
    acts.extend(r.wait(31));
    acts.extend(r.wait(1000));
    assert_eq!(
        texts(&acts),
        vec!["ls".to_owned(), "git status".to_owned()],
        "{acts:?}"
    );
}

#[test]
fn a_pager_scrolling_under_down_is_not_followed_by_a_line_read() {
    let mut r = Rig::new();
    let page = |a: usize| format!("{}:", lines(a..a + 23));
    r.focus(&page(1000), page(1000).chars().count() as u32);
    r.key(TermKey::LineDown);
    r.screen = page(1001);
    r.caret = r.screen.chars().count() as u32;
    let mut acts = r.feed(TermInput::Edit(TermEdit::Replaced));
    acts.extend(r.wait(31));
    acts.extend(r.wait(1000));
    let said = texts(&acts);
    assert!(!said.iter().any(|t| t == ":"), "{said:?}");
}

#[test]
fn recall_to_a_blank_line_below_older_prompts_says_blank_not_the_output() {
    // The older prompt line above has the same prompt text; the empty recall lost its blank.
    let mut r = Rig::new();
    let above = "me@box:~$ git status\nOn branch main\n";
    r.focus(&format!("{above}me@box:~$ git status"), 56);
    r.key(TermKey::LineDown);
    let a = r.repaint(&format!("{above}me@box:~$"), 49);
    assert_eq!(texts(&a), vec!["blank".to_owned()]);
    // And a recall to a command, with the same older prompts above.
    r.key(TermKey::LineUp);
    let a = r.repaint(&format!("{above}me@box:~$ ls"), 52);
    assert_eq!(texts(&a), vec!["ls".to_owned()]);
}
