// SPDX-License-Identifier: GPL-3.0-or-later
//! ED: the editing matrix (docs/reports/unified-editing.md, user request 2026-10-17).
//!
//! The same scripted key sequences run in every editor the user meets, with the real reader
//! against the REAL application in a private headless session: GTK3 and GTK4 entries and
//! text views (`tests/edit/gtkedit.py`), gedit, GNOME Text Editor, Firefox and Chromium
//! (an input, a textarea, a contenteditable), and a gnome-terminal readline line. The model of
//! `model.rs` says what each step must speak; where a target deliberately differs the table
//! says so and why.
//!
//! One scenario id per target (`ED-G3E`, `ED-G4T`, ...); `GORCA_ED_OUT=<dir>` keeps a transcript
//! per target (`<id>.tsv`), `GORCA_ED_PROBE=1` records without asserting (the first run on a new
//! toolkit). Needs `tests/edit/run_edit.sh` and `--reader-env GORCA_ED=1` (the web targets also
//! `GORCA_M7=1`, the terminal `GORCA_M6=1 GORCA_M6_REAL=1`).

use std::fmt::Write as _;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::apps::ReaderSpec;
use super::m1::{K, Typist, flag, m1_env};
use super::m6::RealTerm;
use super::mock::{Flavor, Kind};
use super::runner::{HostEnv, Rig};
use super::scenarios::{RunOut, Size};

/// The scenario ids of this module.
pub const IDS: &[&str] = &[
    "ED-G3E", "ED-G3T", "ED-G4E", "ED-G4T", "ED-GEDIT", "ED-TEDIT", "ED-FFIN", "ED-FFTA",
    "ED-FFCE", "ED-CHIN", "ED-CHTA", "ED-CHCE", "ED-TERM",
];

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Target {
    Gtk3Entry,
    Gtk3Text,
    Gtk4Entry,
    Gtk4Text,
    Gedit,
    TextEditor,
    /// A web editable: Firefox or Chromium (`GORCA_M7_BROWSER`), the kind of field.
    Web(WebField),
    Terminal,
}

#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WebField {
    Input,
    Textarea,
    Editable,
}

impl Target {
    pub fn parse(id: &str) -> Option<Self> {
        Some(match id {
            "ED-G3E" => Self::Gtk3Entry,
            "ED-G3T" => Self::Gtk3Text,
            "ED-G4E" => Self::Gtk4Entry,
            "ED-G4T" => Self::Gtk4Text,
            "ED-GEDIT" => Self::Gedit,
            "ED-TEDIT" => Self::TextEditor,
            "ED-FFIN" | "ED-CHIN" => Self::Web(WebField::Input),
            "ED-FFTA" | "ED-CHTA" => Self::Web(WebField::Textarea),
            "ED-FFCE" | "ED-CHCE" => Self::Web(WebField::Editable),
            "ED-TERM" => Self::Terminal,
            _ => return None,
        })
    }

    /// Does the target hold several lines (so the line, paragraph and page keys mean something)?
    pub fn multiline(self) -> bool {
        match self {
            Self::Gtk3Entry | Self::Gtk4Entry | Self::Terminal => false,
            Self::Web(f) => f != WebField::Input,
            Self::Gtk3Text | Self::Gtk4Text | Self::Gedit | Self::TextEditor => true,
        }
    }

    /// Does Ctrl+Shift+arrows / Shift+arrows select (a terminal's readline does not)?
    pub fn selects(self) -> bool {
        self != Self::Terminal
    }

    fn is_app(self) -> bool {
        matches!(self, Self::Gedit | Self::TextEditor)
    }
}

// ---- keys -------------------------------------------------------------------------------------

const CONTROL: u32 = 4;
const SHIFT: u32 = 1;
const ALT: u32 = 8;

/// A key as the reader (through the emulated Mutter) and the application (through the
/// compositor's virtual keyboard) see it.
#[derive(Copy, Clone, Debug)]
pub struct Key {
    pub name: &'static str,
    /// evdev code (the X keycode is this plus 8).
    pub evdev: u16,
    pub sym: u32,
    pub uni: u32,
    pub mods: u32,
}

const fn key(name: &'static str, evdev: u16, sym: u32, uni: u32) -> Key {
    Key {
        name,
        evdev,
        sym,
        uni,
        mods: 0,
    }
}

const LEFT: Key = key("Left", 105, 0xff51, 0);
const RIGHT: Key = key("Right", 106, 0xff53, 0);
const UP: Key = key("Up", 103, 0xff52, 0);
const DOWN: Key = key("Down", 108, 0xff54, 0);
const HOME: Key = key("Home", 102, 0xff50, 0);
const END: Key = key("End", 107, 0xff57, 0);
const PGUP: Key = key("PageUp", 104, 0xff55, 0);
const PGDN: Key = key("PageDown", 109, 0xff56, 0);
const BKSP: Key = key("BackSpace", 14, 0xff08, 8);
const DEL: Key = key("Delete", 111, 0xffff, 0);
const ENTER: Key = key("Enter", 28, 0xff0d, 13);
const ESC: Key = key("Escape", 1, 0xff1b, 27);
const KEY_A: Key = key("a", 30, 0x61, 1);
const KEY_F: Key = key("f", 33, 0x66, 6);
const KEY_H: Key = key("h", 35, 0x68, 8);
const KEY_S: Key = key("s", 31, 0x73, 19);
const KEY_T: Key = key("t", 20, 0x74, 20);
const KEY_Z: Key = key("z", 44, 0x7a, 26);

impl Key {
    const fn ctrl(mut self) -> Self {
        self.mods |= CONTROL;
        self.uni = 0;
        self
    }

    const fn shift(mut self) -> Self {
        self.mods |= SHIFT;
        self
    }

    const fn alt(mut self) -> Self {
        self.mods |= ALT;
        self.uni = 0;
        self
    }

    fn k(&self) -> K {
        K {
            code: self.evdev + 8,
            sym: self.sym,
        }
    }

    /// evdev codes of the whole chord, modifiers first.
    fn chord(&self) -> Vec<u32> {
        let mut v = Vec::with_capacity(4);
        for (bit, code) in [(CONTROL, 29u32), (SHIFT, 42), (ALT, 56)] {
            if self.mods & bit != 0 {
                v.push(code);
            }
        }
        v.push(u32::from(self.evdev));
        v
    }
}

/// The key that types `c` (US layout): letters, digits and a few marks.
pub fn char_key(c: char) -> Option<Key> {
    const ROWS: [(&str, u16); 4] = [
        ("qwertyuiop", 16),
        ("asdfghjkl", 30),
        ("zxcvbnm", 44),
        ("1234567890", 2),
    ];
    let lower = c.to_ascii_lowercase();
    for (row, base) in ROWS {
        if let Some(i) = row.find(lower) {
            let mut k = key("char", base + u16::try_from(i).ok()?, c as u32, c as u32);
            if c.is_ascii_uppercase() {
                k.mods = SHIFT;
            }
            return Some(k);
        }
    }
    Some(match c {
        ' ' => key("space", 57, 0x20, 0x20),
        ',' => key("char", 51, 0x2c, 0x2c),
        '.' => key("char", 52, 0x2e, 0x2e),
        '-' => key("char", 12, 0x2d, 0x2d),
        '/' => key("char", 53, 0x2f, 0x2f),
        ';' => key("char", 39, 0x3b, 0x3b),
        '?' => Key {
            mods: SHIFT,
            ..key("char", 53, 0x3f, 0x3f)
        },
        '!' => Key {
            mods: SHIFT,
            ..key("char", 2, 0x21, 0x21)
        },
        ':' => Key {
            mods: SHIFT,
            ..key("char", 39, 0x3a, 0x3a)
        },
        _ => return None,
    })
}

// ---- the script -------------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub enum Act {
    Key(Key),
    Type(&'static str),
    Wait(u64),
}

#[derive(Clone, Debug)]
pub struct Step {
    pub name: &'static str,
    pub acts: Vec<Act>,
}

fn st(name: &'static str, k: Key) -> Step {
    Step {
        name,
        acts: vec![Act::Key(k)],
    }
}

fn typed(name: &'static str, text: &'static str) -> Step {
    Step {
        name,
        acts: vec![Act::Type(text)],
    }
}

/// The text typed first, and the second paragraph of the multi-line phase (long enough to wrap
/// in a narrow view).
pub const LINE: &str = "hello, brave world";
pub const PARA: &str = "alpha bravo charlie delta echo foxtrot golf hotel india juliet kilo lima mike november oscar papa quebec";

/// The matrix script of a target: single-line steps first (every target), then the multi-line
/// and selection phases where the target has them.
pub fn script(t: Target) -> Vec<Step> {
    let mut s = vec![
        typed("type", LINE),
        st("Left", LEFT),
        st("Right", RIGHT),
        st("Home", HOME),
        st("Left at start", LEFT),
        st("End", END),
        st("Right at end", RIGHT),
        st("Ctrl+Left 1", LEFT.ctrl()),
        st("Ctrl+Left 2", LEFT.ctrl()),
        st("Ctrl+Left 3", LEFT.ctrl()),
        st("Ctrl+Left 4 at start", LEFT.ctrl()),
        st("Ctrl+Right 1", RIGHT.ctrl()),
        st("Ctrl+Right 2", RIGHT.ctrl()),
        st("Ctrl+Right 3", RIGHT.ctrl()),
        st("Ctrl+Right 4 at end", RIGHT.ctrl()),
        st("BackSpace", BKSP),
        st("Delete at end", DEL),
        st("Home 2", HOME),
        st("Delete", DEL),
    ];
    if t.selects() {
        s.extend([
            st("Shift+Right", RIGHT.shift()),
            st("Shift+Right 2", RIGHT.shift()),
            st("Shift+Left", LEFT.shift()),
            st("Shift+Left 2", LEFT.shift()),
            st("Ctrl+Shift+Right", RIGHT.ctrl().shift()),
            st("Ctrl+Shift+Left", LEFT.ctrl().shift()),
            st("Shift+End", END.shift()),
            st("Shift+Home", HOME.shift()),
            st("Ctrl+A", KEY_A.ctrl()),
            st("Right collapses", RIGHT),
            st("End 2", END),
            st("Ctrl+BackSpace", BKSP.ctrl()),
            st("Ctrl+Delete at end", DEL.ctrl()),
            st("Home 3", HOME),
            st("Ctrl+Delete", DEL.ctrl()),
        ]);
    }
    if std::env::var("GORCA_ED_PHASE").as_deref() == Ok("multi") {
        s.clear();
    }
    if t.multiline() {
        s.extend([
            st("select all", KEY_A.ctrl()),
            st("clear", DEL),
            typed("type line 1", "one two"),
            st("Enter", ENTER),
            typed("type paragraph", PARA),
            st("Enter 2", ENTER),
            typed("type last", "last line"),
            st("Up", UP),
            st("Up 2", UP),
            st("Up 3", UP),
            st("Down", DOWN),
            st("Ctrl+Up", UP.ctrl()),
            st("Ctrl+Up 2", UP.ctrl()),
            st("Ctrl+Down", DOWN.ctrl()),
            st("Ctrl+Home", HOME.ctrl()),
            st("Ctrl+End", END.ctrl()),
            st("PageUp", PGUP),
            st("PageDown", PGDN),
            st("Ctrl+Home 2", HOME.ctrl()),
            st("Shift+Down", DOWN.shift()),
            st("Shift+Down 2", DOWN.shift()),
            st("Shift+Up", UP.shift()),
            st("Ctrl+Shift+Down", DOWN.ctrl().shift()),
            st("Ctrl+Shift+End", END.ctrl().shift()),
            st("BackSpace selection", BKSP),
            st("Ctrl+Z", KEY_Z.ctrl()),
        ]);
    }
    if t.is_app() && std::env::var("GORCA_ED_PHASE").as_deref() == Ok("app") {
        s.clear();
        s.push(typed("seed", "alpha bravo charlie"));
    }
    if t.is_app() && std::env::var("GORCA_ED_PHASE").as_deref() != Ok("single") {
        s.extend(app_steps(t));
    }
    s
}

/// gedit and GNOME Text Editor: find, replace, tabs and the save dialog (baseline scope).
fn app_steps(t: Target) -> Vec<Step> {
    let mut v = vec![
        st("Ctrl+Home 3", HOME.ctrl()),
        st("Ctrl+F", KEY_F.ctrl()),
        typed("find: type", "bravo"),
        st("find: Enter", ENTER),
        st("find: Escape", ESC),
        st("Ctrl+H", KEY_H.ctrl()),
        st("replace: Escape", ESC),
        st("Ctrl+T", KEY_T.ctrl()),
        typed("tab 2: type", "second"),
        st("Ctrl+Alt+PageUp", PGUP.ctrl().alt()),
        st("Ctrl+Alt+PageDown", PGDN.ctrl().alt()),
        st("Ctrl+S", KEY_S.ctrl()),
        st("save: Escape", ESC),
    ];
    if t == Target::TextEditor {
        v.insert(8, st("Ctrl+Alt+PageUp check", PGUP.ctrl().alt()));
    }
    v
}

// ---- drivers ----------------------------------------------------------------------------------

/// What the matrix needs of a running target.
pub trait Drv {
    fn press(&mut self, key: &Key) -> Result<()>;
    fn mark(&self) -> usize;
    fn heard(&self, mark: usize) -> Vec<String>;
}

fn sleep(ms: u64) {
    std::thread::sleep(Duration::from_millis(ms));
}

/// Wait until nothing has been spoken for `quiet_ms` (at most `cap_ms`).
fn settle(d: &dyn Drv, mark: usize, quiet_ms: u64, cap_ms: u64) {
    let end = Instant::now() + Duration::from_millis(cap_ms);
    let mut last = d.heard(mark).len();
    let mut since = Instant::now();
    while Instant::now() < end {
        sleep(40);
        let n = d.heard(mark).len();
        if n != last {
            last = n;
            since = Instant::now();
        } else if since.elapsed() >= Duration::from_millis(quiet_ms) {
            break;
        }
    }
}

/// The result of one step.
#[derive(Clone, Debug)]
pub struct Row {
    pub name: &'static str,
    pub got: Vec<String>,
}

fn run_script(d: &mut dyn Drv, steps: &[Step]) -> Result<Vec<Row>> {
    let mut rows = Vec::with_capacity(steps.len());
    for s in steps {
        let mark = d.mark();
        for a in &s.acts {
            match a {
                Act::Key(k) => d.press(k)?,
                Act::Type(text) => {
                    for c in text.chars() {
                        let k = char_key(c).with_context(|| format!("no key for {c:?}"))?;
                        d.press(&k)?;
                        sleep(70);
                    }
                }
                Act::Wait(ms) => sleep(*ms),
            }
        }
        sleep(150);
        settle(d, mark, 350, 2500);
        rows.push(Row {
            name: s.name,
            got: d.heard(mark),
        });
    }
    Ok(rows)
}

/// A native target: the reader's rig, a keyboard on the compositor, the application.
struct Nat {
    // Drop order: applications, keyboard, then the reader.
    apps: Vec<(Child, i32)>,
    rd: Option<Child>,
    term: Option<RealTerm>,
    rig: Rig,
    ty: Typist,
}

impl Drop for Nat {
    fn drop(&mut self) {
        if let Some(rd) = self.rd.as_mut() {
            drop(rd.stdin.take());
        }
        for (_, pgid) in &self.apps {
            let group = format!("-{pgid}");
            let _ = Command::new("kill").args(["-TERM", "--", &group]).status();
        }
        sleep(600);
        for (c, pgid) in &mut self.apps {
            let group = format!("-{pgid}");
            let _ = Command::new("kill").args(["-KILL", "--", &group]).status();
            let _ = c.wait();
        }
        if let Some(mut rd) = self.rd.take() {
            let _ = rd.kill();
            let _ = rd.wait();
        }
    }
}

impl Nat {
    fn rd_line(&mut self, line: &str) {
        use std::io::Write;
        if let Some(t) = self.term.as_mut() {
            t.rd_line(line);
        } else if let Some(stdin) = self.rd.as_mut().and_then(|c| c.stdin.as_mut()) {
            let _ = writeln!(stdin, "{line}");
            let _ = stdin.flush();
        }
    }
}

impl Drv for Nat {
    fn press(&mut self, k: &Key) -> Result<()> {
        let mods = [
            (
                CONTROL,
                K {
                    code: 37,
                    sym: 0xffe3,
                },
            ),
            (
                SHIFT,
                K {
                    code: 50,
                    sym: 0xffe1,
                },
            ),
            (
                ALT,
                K {
                    code: 64,
                    sym: 0xffe9,
                },
            ),
        ];
        self.ty.t_ms += 12;
        let m = self.rig.mutter.as_mut().context("no mutter")?;
        let mut held = 0u32;
        for (bit, mk) in mods {
            if k.mods & bit != 0 {
                self.ty.t_ms += 4;
                m.key_ex(mk.code, mk.sym, held, false, self.ty.t_ms)?;
                held |= bit;
            }
        }
        self.ty.t_ms += 8;
        let kk = k.k();
        let down = m.key_full(kk.code, kk.sym, k.uni, k.mods, false, self.ty.t_ms)?;
        self.ty.t_ms += 3;
        m.key_full(kk.code, kk.sym, 0, k.mods, true, self.ty.t_ms)?;
        for (bit, mk) in mods.iter().rev() {
            if k.mods & bit != 0 {
                self.ty.t_ms += 3;
                held &= !bit;
                m.key_ex(mk.code, mk.sym, held, true, self.ty.t_ms)?;
            }
        }
        if !down.consumed {
            let list: Vec<String> = k.chord().iter().map(u32::to_string).collect();
            self.rd_line(&format!("key {}", list.join(" ")));
        }
        Ok(())
    }

    fn mark(&self) -> usize {
        self.rig.mock.mark()
    }

    fn heard(&self, mark: usize) -> Vec<String> {
        self.rig
            .mock
            .since(mark)
            .iter()
            .filter_map(|r| match &r.kind {
                Kind::Speak { text, .. } => Some(text.clone()),
                _ => None,
            })
            .collect()
    }
}

fn group_of(c: &Child) -> i32 {
    i32::try_from(c.id()).unwrap_or(0)
}

fn spawn_app(
    host: &HostEnv,
    prog: &str,
    args: &[String],
    extra_env: &[(&str, &str)],
    log: &str,
) -> Result<(Child, i32)> {
    let wayland = std::env::var("GORCA_VTE_WAYLAND").context("GORCA_VTE_WAYLAND not set")?;
    let root = std::env::var("GORCA_HARNESS_DIR").context("GORCA_HARNESS_DIR not set")?;
    if !host.a11y_addr.contains("/gorca-h.") {
        bail!("refusing to start an application outside a harness session");
    }
    let out = std::fs::File::create(PathBuf::from(&root).join(format!("{log}.log")))?;
    let err = out.try_clone()?;
    let mut cmd = Command::new(prog);
    cmd.args(args)
        .env_remove("DISPLAY")
        .env("GDK_BACKEND", "wayland")
        .env("WAYLAND_DISPLAY", wayland)
        .env("GSETTINGS_BACKEND", "memory")
        .env("LANG", "C.UTF-8")
        .env("NO_AT_BRIDGE", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::from(out))
        .stderr(Stdio::from(err))
        .process_group(0);
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    let child = cmd.spawn().with_context(|| format!("starting {prog}"))?;
    let g = group_of(&child);
    Ok((child, g))
}

fn start_native(host: &HostEnv, reader: &ReaderSpec, flavor: Flavor, t: Target) -> Result<Nat> {
    let mut o = super::scenarios::base_opts(reader, flavor);
    o.keys = true;
    let rig = Rig::start(&m1_env(host), &o)?;
    if !rig.keys_ok {
        bail!("the reader did not register with the emulated keyboard monitor (no SetKeyGrabs)");
    }
    rig.mock
        .configure(Duration::from_millis(1), Duration::from_millis(5));
    let mut n = Nat {
        apps: Vec::new(),
        rd: None,
        term: None,
        rig,
        ty: Typist::new(),
    };
    if t == Target::Terminal {
        n.term = Some(RealTerm::start(host)?);
        sleep(4000);
    } else {
        let cbus = std::env::var("GORCA_VTE_CBUS").context("GORCA_VTE_CBUS not set")?;
        let tools = std::env::var("GORCA_VTE_TOOLS").context("GORCA_VTE_TOOLS not set")?;
        let root = std::env::var("GORCA_HARNESS_DIR").context("GORCA_HARNESS_DIR not set")?;
        let log = |f: &str| std::fs::File::create(PathBuf::from(&root).join(f));
        let rd = Command::new("/usr/bin/python3")
            .arg(PathBuf::from(&tools).join("../web/m7/rd.py"))
            .env("GORCA_M7_CBUS", cbus)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(log("ed-rd.log")?))
            .stderr(Stdio::from(log("ed-rd.err")?))
            .process_group(0)
            .spawn()
            .context("starting rd.py")?;
        n.rd = Some(rd);
        sleep(1500);
        // A headless seat has no keyboard until a virtual one is used.
        n.rd_line("key 42");
        sleep(500);
        let ed = std::env::var("GORCA_ED_TOOLS").context("GORCA_ED_TOOLS not set")?;
        let gtk = |ver: &str, widget: &str| {
            vec![
                PathBuf::from(&ed).join("gtkedit.py").display().to_string(),
                ver.to_owned(),
                widget.to_owned(),
                "--width".to_owned(),
                "420".to_owned(),
            ]
        };
        let app = match t {
            Target::Gtk3Entry => {
                spawn_app(host, "/usr/bin/python3", &gtk("3", "entry"), &[], "ed-app")?
            }
            Target::Gtk3Text => spawn_app(
                host,
                "/usr/bin/python3",
                &gtk("3", "textview"),
                &[],
                "ed-app",
            )?,
            Target::Gtk4Entry => {
                spawn_app(host, "/usr/bin/python3", &gtk("4", "entry"), &[], "ed-app")?
            }
            Target::Gtk4Text => spawn_app(
                host,
                "/usr/bin/python3",
                &gtk("4", "textview"),
                &[],
                "ed-app",
            )?,
            Target::Gedit => spawn_app(
                host,
                "/usr/bin/gedit",
                &["--standalone".to_owned()],
                &[],
                "ed-app",
            )?,
            Target::TextEditor => {
                // Text Editor restores the drafts of the previous run from its data dir: every
                // start gets a fresh one so repeats begin with an empty document.
                let root =
                    std::env::var("GORCA_HARNESS_DIR").context("GORCA_HARNESS_DIR not set")?;
                let nanos = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map_or(0, |d| d.as_nanos());
                let data = PathBuf::from(root).join(format!("xdg-data-{nanos}"));
                std::fs::create_dir_all(&data)?;
                let data = data.display().to_string();
                spawn_app(
                    host,
                    "/usr/bin/gnome-text-editor",
                    &["--standalone".to_owned()],
                    &[("XDG_DATA_HOME", data.as_str())],
                    "ed-app",
                )?
            }
            Target::Terminal | Target::Web(_) => bail!("not a native GUI target"),
        };
        n.apps.push(app);
        sleep(5000);
        n.rd_line("click");
        sleep(1200);
    }
    Ok(n)
}

// ---- web --------------------------------------------------------------------------------------

struct WebDrv {
    f: super::webtext::Ft,
}

impl Drv for WebDrv {
    fn press(&mut self, k: &Key) -> Result<()> {
        self.f.key(k.k(), k.uni, k.mods).map(|_| ())
    }

    fn mark(&self) -> usize {
        self.f.w.mark()
    }

    fn heard(&self, mark: usize) -> Vec<String> {
        self.f.heard(mark).into_iter().map(|(_, s)| s).collect()
    }
}

// ---- the run ----------------------------------------------------------------------------------

fn dump(id: &str, rows: &[Row]) {
    let Some(dir) = std::env::var_os("GORCA_ED_OUT") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let _ = std::fs::create_dir_all(&dir);
    let mut s = String::new();
    for r in rows {
        let _ = writeln!(s, "{}\t{}", r.name, r.got.join(" | ").replace('\n', "\\n"));
    }
    let _ = std::fs::write(dir.join(format!("{id}.tsv")), s);
}

pub fn run(
    id: &str,
    host: &HostEnv,
    reader: &ReaderSpec,
    flavor: Flavor,
    _size: Size,
    out: &mut RunOut,
) -> Result<()> {
    let Some(t) = Target::parse(id) else {
        bail!("unknown editing scenario {id}");
    };
    let web = matches!(t, Target::Web(_));
    let ready = flag(host, "GORCA_ED")
        && std::env::var_os("GORCA_VTE_WAYLAND").is_some()
        && (!web || flag(host, "GORCA_M7"))
        && (t != Target::Terminal || flag(host, "GORCA_M6"));
    if !ready {
        if flag(host, "GORCA_ED_STRICT") {
            bail!("{id}: needs --reader-env GORCA_ED=1 and tests/edit/run_edit.sh");
        }
        out.notes.push(format!(
            "{id} SKIPPED: tests/edit/run_edit.sh and --reader-env GORCA_ED=1 (web: GORCA_M7=1, terminal: GORCA_M6=1)"
        ));
        out.x("skipped", 1.0, "flag");
        return Ok(());
    }
    let chromium = id.starts_with("ED-CH");
    if chromium && !super::web::chromium_selected() {
        out.notes.push(format!(
            "{id} SKIPPED: GORCA_M7_BROWSER=chromium selects Chromium for the ED-CH targets"
        ));
        out.x("skipped", 1.0, "flag");
        return Ok(());
    }
    if web && !chromium && super::web::chromium_selected() {
        out.notes.push(format!(
            "{id} SKIPPED: GORCA_M7_BROWSER=chromium runs the ED-CH targets"
        ));
        out.x("skipped", 1.0, "flag");
        return Ok(());
    }
    let steps = script(t);
    let rows = match t {
        Target::Web(field) => {
            let (label, role) = match field {
                WebField::Input => ("Line", "entry"),
                WebField::Textarea => ("Notes", "entry"),
                WebField::Editable => ("Editor", "entry"),
            };
            let f = super::webtext::Ft::open_edit(host, reader, flavor, label, role)?;
            let mut d = WebDrv { f };
            run_script(&mut d, &steps)?
        }
        _ => {
            let mut n = start_native(host, reader, flavor, t)?;
            // The focus announcement and anything else the application says on its own.
            let mark = n.mark();
            settle(&n, mark, 800, 6000);
            if n.heard(0).is_empty() {
                bail!("{id}: the reader never spoke: the application did not come up");
            }
            run_script(&mut n, &steps)?
        }
    };
    dump(id, &rows);
    let probe = std::env::var_os("GORCA_ED_PROBE").is_some();
    let mut failures = 0u64;
    let mut gated = 0u64;
    for r in &rows {
        let line = r.got.join(" | ");
        match super::edit_model::check(t, chromium, r.name, &r.got) {
            Some(Err(why)) if !probe => {
                failures += 1;
                let known = super::edit_model::known_failing(t, r.name);
                // Web targets are reported, not gated: the typist of the browser rigs drops letters
                // (flake class); the native targets and the terminal gate.
                if !known && !matches!(t, Target::Web(_)) {
                    gated += 1;
                }
                let tag = if known { " [known failing]" } else { "" };
                out.notes
                    .push(format!("{id} {}: heard [{line}] {why}{tag}", r.name));
            }
            Some(Err(_) | Ok(())) | None => {
                out.notes.push(format!("{id} {}: [{line}]", r.name));
            }
        }
    }
    out.x("ed_steps", rows.len() as f64, "steps");
    out.x("ed_failures", failures as f64, "steps");
    // The gated metric (bin/e2e.rs, threshold 0): native editors and the terminal only.
    if !matches!(t, Target::Web(_)) {
        out.x("ed_gate_failures", gated as f64, "steps");
    }
    Ok(())
}
