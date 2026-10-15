//! Prompt and password-prompt recognition for a terminal line.
//!
//! There is no structural information in the AT-SPI tree (no OSC 133), so the prompt end is a
//! heuristic: a user-configurable regular expression first, then the first of a list of markers
//! (`"$ "`, `"# "`, ...). A password prompt is a line that ends in a colon or question mark and
//! names a secret. Both run on short strings and never fail: an invalid expression is ignored.

use regex::Regex;

use gorca_ext_api::terminal::TermConfig;

/// Words that name a secret, as regular-expression alternatives (matched case-insensitively and
/// with Unicode case folding). Add a language here, not a branch in the code. Sources: the
/// `Linux-PAM`, `sudo` and `shadow` message catalogues (`msgunfmt /usr/share/locale/<l>/LC_MESSAGES/`)
/// and the OpenSSH/GnuPG English prompts. `pass` also covers German "Passwort", French "mot de
/// passe" and Italian "password".
const SECRET_WORDS: &[&str] = &[
    // English and the Latin-script catalogues that contain "pass".
    "pass(word|phrase|code)?",
    "passwd",
    r"\bpin\b",
    "secret",
    r"\btoken\b",
    r"\botp\b",
    "verification code",
    "credentials?",
    // Spanish, Portuguese.
    "contrase\u{f1}a",
    "senha",
    // Russian and Ukrainian (escaped; sudo prints "[sudo] <password> <for> user:"): password,
    // passphrase, code phrase, confirmation code, secret, PIN code.
    "\u{43f}\u{430}\u{440}\u{43e}\u{43b}[\u{44c}\u{44f}\u{435}\u{44e}\u{438}\u{439}\u{44b}]*",
    "\u{43f}\u{430}\u{440}\u{43e}\u{43b}\u{44c}\u{43d}\u{430}\u{44f} \u{444}\u{440}\u{430}\u{437}\u{430}",
    "\u{43a}\u{43e}\u{434}\u{43e}\u{432}\u{430}\u{44f} \u{444}\u{440}\u{430}\u{437}\u{430}",
    "\u{43a}\u{43e}\u{434} \u{43f}\u{43e}\u{434}\u{442}\u{432}\u{435}\u{440}\u{436}\u{434}\u{435}\u{43d}\u{438}\u{44f}",
    "\u{441}\u{435}\u{43a}\u{440}\u{435}\u{442}\u{43d}[\u{430}-\u{44f}]*",
    "\u{43f}\u{438}\u{43d}-?\u{43a}\u{43e}\u{434}",
];

/// What may follow the word before the closing colon or question mark (including the full-width
/// colon and the CJK/French spacing variants).
const SECRET_TAIL: &str = r"[^:?\u{ff1a}\u{ff1f}]{0,60}[:?\u{ff1a}\u{ff1f}]\s*$";

/// The built-in password prompt expression, built from [`SECRET_WORDS`].
fn secret_default() -> String {
    format!("(?i)({}){}", SECRET_WORDS.join("|"), SECRET_TAIL)
}

/// Longest line the recognisers look at (scalars); a longer line is cut at its start.
const MAX_LINE: usize = 512;

#[derive(Debug)]
pub struct Prompt {
    markers: Vec<String>,
    line_re: Option<Regex>,
    /// `None` only if even the built-in expression failed to compile (a test proves it does not).
    secret_re: Option<Regex>,
}

impl Prompt {
    pub fn new(cfg: &TermConfig) -> Self {
        let line_re = if cfg.prompt_regex.is_empty() {
            None
        } else {
            Regex::new(&cfg.prompt_regex).ok()
        };
        let secret_re = if cfg.secret_regex.is_empty() {
            None
        } else {
            Regex::new(&cfg.secret_regex).ok()
        }
        .or_else(|| Regex::new(&secret_default()).ok());
        Self {
            markers: cfg.prompt_markers.clone(),
            line_re,
            secret_re,
        }
    }

    /// The column (in scalars) at which the command starts on `line`, when the line has a prompt.
    pub fn end(&self, line: &str) -> Option<u32> {
        let line = clip(line);
        if let Some(re) = &self.line_re
            && let Some(m) = re.find(line)
            && m.end() > 0
        {
            return Some(cols(line.get(..m.end()).unwrap_or("")));
        }
        let mut best: Option<usize> = None;
        for mk in &self.markers {
            if mk.is_empty() {
                continue;
            }
            if let Some(i) = line.find(mk.as_str()) {
                let end = i + mk.len();
                best = Some(best.map_or(end, |b| b.min(end)));
            }
        }
        if best.is_none() {
            // VTE trims trailing blanks: a prompt with nothing typed after it ends in the marker
            // without its space.
            let bare = line.trim_end();
            let alone = self.markers.iter().any(|mk| {
                let m = mk.trim_end();
                !m.is_empty() && bare.ends_with(m)
            });
            if alone {
                return Some(cols(bare));
            }
        }
        best.map(|b| cols(line.get(..b).unwrap_or("")))
    }

    /// Does `before_caret` (the cursor line up to the caret, nothing after it) ask for a secret?
    pub fn is_secret_prompt(&self, before_caret: &str) -> bool {
        self.secret_re
            .as_ref()
            .is_some_and(|re| re.is_match(clip(before_caret)))
    }
}

fn clip(s: &str) -> &str {
    if s.len() <= MAX_LINE {
        return s;
    }
    let mut i = s.len() - MAX_LINE;
    while !s.is_char_boundary(i) {
        i += 1;
    }
    s.get(i..).unwrap_or(s)
}

fn cols(s: &str) -> u32 {
    u32::try_from(s.chars().count()).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p() -> Prompt {
        Prompt::new(&TermConfig::default())
    }

    #[test]
    fn the_built_in_secret_expression_compiles() {
        assert!(Regex::new(&secret_default()).is_ok());
    }

    #[test]
    fn the_first_marker_ends_the_prompt() {
        let p = p();
        assert_eq!(p.end("user@host:~$ ls -la"), Some(13));
        assert_eq!(p.end("root@host:/# "), Some(13));
        assert_eq!(p.end("no prompt here"), None);
        // An empty prompt whose trailing blank was trimmed.
        assert_eq!(p.end("me@box:~$"), Some(9));
        assert_eq!(p.end("root@host:/#"), Some(12));
        assert_eq!(p.end("\u{276f} cargo build"), Some(2));
        // A marker inside the command does not move the prompt end.
        assert_eq!(p.end("u@h:~$ echo $ x"), Some(7));
    }

    #[test]
    fn a_regex_wins_over_the_markers() {
        let cfg = TermConfig {
            prompt_regex: r"^\[\d+\] \w+ > ".to_owned(),
            ..TermConfig::default()
        };
        let p = Prompt::new(&cfg);
        assert_eq!(p.end("[12] bob > ls $ x"), Some(11));
        // A line the expression does not match falls back to the markers.
        assert_eq!(p.end("u@h:~$ ls"), Some(7));
    }

    #[test]
    fn an_invalid_regex_is_ignored() {
        let cfg = TermConfig {
            prompt_regex: "([".to_owned(),
            secret_regex: "(".to_owned(),
            ..TermConfig::default()
        };
        let p = Prompt::new(&cfg);
        assert_eq!(p.end("u@h:~$ ls"), Some(7));
        assert!(p.is_secret_prompt("[sudo] password for me: "));
    }

    #[test]
    fn password_prompts_are_recognised() {
        let p = p();
        for yes in [
            "[sudo] password for evgeniy: ",
            "Password:",
            "Enter passphrase for key '/home/u/.ssh/id_ed25519': ",
            "Current password: ",
            "Enter PIN:",
            "Verification code: ",
            "mysql password?",
            // Localised (sudo, PAM, ssh, passwd) prompts.
            "[sudo] \u{43f}\u{430}\u{440}\u{43e}\u{43b}\u{44c} \u{434}\u{43b}\u{44f} evgeniy: ",
            "\u{41f}\u{430}\u{440}\u{43e}\u{43b}\u{44c}:",
            "\u{41f}\u{430}\u{440}\u{43e}\u{43b}\u{44c} \u{434}\u{43b}\u{44f} user@host: ",
            "\u{422}\u{435}\u{43a}\u{443}\u{449}\u{438}\u{439} \u{43f}\u{430}\u{440}\u{43e}\u{43b}\u{44c}: ",
            "\u{41d}\u{43e}\u{432}\u{44b}\u{439} \u{43f}\u{430}\u{440}\u{43e}\u{43b}\u{44c}: ",
            "\u{412}\u{432}\u{435}\u{434}\u{438}\u{442}\u{435} \u{43f}\u{430}\u{440}\u{43e}\u{43b}\u{44c}\u{43d}\u{443}\u{44e} \u{444}\u{440}\u{430}\u{437}\u{443} \u{434}\u{43b}\u{44f} \u{43a}\u{43b}\u{44e}\u{447}\u{430}: ",
            "\u{41f}\u{410}\u{420}\u{41e}\u{41b}\u{42c}:",
            "\u{41f}\u{43e}\u{442}\u{43e}\u{447}\u{43d}\u{438}\u{439} \u{43f}\u{430}\u{440}\u{43e}\u{43b}\u{44c}: ",
            "Mot de passe actuel : ",
            "Passwort: ",
            "Contrase\u{f1}a actual: ",
            "Password\u{ff1a}",
        ] {
            assert!(p.is_secret_prompt(yes), "{yes}");
        }
        for no in [
            "user@host:~$ ",
            "user@host:~$ cat password.txt",
            "passwords are great",
            "Password updated.",
            "ls: ",
            "\u{41f}\u{430}\u{440}\u{43e}\u{43b}\u{44c} \u{438}\u{437}\u{43c}\u{435}\u{43d}\u{451}\u{43d}.",
            "user@host:~$ cat \u{43f}\u{430}\u{440}\u{43e}\u{43b}\u{44c}.txt",
        ] {
            assert!(!p.is_secret_prompt(no), "{no}");
        }
    }

    #[test]
    fn a_custom_secret_expression_replaces_the_default() {
        let cfg = TermConfig {
            secret_regex: r"(?i)^code> $".to_owned(),
            ..TermConfig::default()
        };
        let p = Prompt::new(&cfg);
        assert!(p.is_secret_prompt("code> "));
        assert!(!p.is_secret_prompt("Password: "));
    }

    #[test]
    fn a_long_line_is_cut_at_its_start() {
        let long = format!("{}Password: ", "x".repeat(10_000));
        assert!(p().is_secret_prompt(&long));
    }
}
