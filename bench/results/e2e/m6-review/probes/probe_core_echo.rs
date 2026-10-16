// Reviewer probe (M6 review). Append inside crates/gorca-core/src/terminal/tests.rs as
// `mod probe_echo { use super::*; <this file> }` and run `cargo test -p gorca-core --lib probe_`.
#[test]
fn probe_typed_characters_off_is_honoured_in_a_terminal() {
    let mut g = rig();
    g.focus("me@box:~$ ", 10);
    g.r.cmd.overlay.typed_characters = Some(false);
    g.r.cmd.republish();
    let fx = g.typed('x');
    let s = spoken(&fx);
    println!("spoken with echo.typed_characters=false: {s:?}");
    assert!(s.is_empty(), "the echo setting is ignored by the terminal arm: {s:?}");
}
