//! Languages with a catalog, and the process-wide UI language.
//!
//! One daemon speaks one language at a time: the Reader publishes `speech.language` here when the
//! effective settings change (`set_language`), and every presenter reads it through [`language`].
//! A test pins a language for its own thread with [`scoped`], which does not touch other tests.

use std::cell::Cell;
use std::sync::atomic::{AtomicU8, Ordering};

/// A language with its own catalog and symbol table. Anything else speaks English.
#[derive(Copy, Clone, Debug, Default, Eq, PartialEq, Hash)]
pub enum Lang {
    #[default]
    En,
    Ru,
}

impl Lang {
    /// Every language with a catalog.
    pub const ALL: [Lang; 2] = [Lang::En, Lang::Ru];

    /// The language for a locale-ish code (`ru`, `ru_RU.UTF-8`, `ru-RU`); English when the
    /// language has no catalog.
    pub fn from_code(code: &str) -> Self {
        let base = code
            .trim()
            .split(['.', '@', '_', '-'])
            .next()
            .unwrap_or("")
            .to_ascii_lowercase();
        match base.as_str() {
            "ru" => Lang::Ru,
            _ => Lang::En,
        }
    }

    /// The ISO 639-1 code.
    pub const fn code(self) -> &'static str {
        match self {
            Lang::En => "en",
            Lang::Ru => "ru",
        }
    }

    pub(crate) const fn index(self) -> usize {
        match self {
            Lang::En => 0,
            Lang::Ru => 1,
        }
    }

    /// The index of the plural form for the number `n`: English `one|other`, Russian
    /// `one|few|many`.
    pub fn plural_index(self, n: i64) -> usize {
        let n = n.unsigned_abs();
        match self {
            Lang::En => usize::from(n != 1),
            Lang::Ru => {
                let (m10, m100) = (n % 10, n % 100);
                if m10 == 1 && m100 != 11 {
                    0
                } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
                    1
                } else {
                    2
                }
            }
        }
    }
}

static CURRENT: AtomicU8 = AtomicU8::new(0);

thread_local! {
    static PINNED: Cell<Option<Lang>> = const { Cell::new(None) };
}

/// Publish the UI language (the Reader does, when the effective settings change).
pub fn set_language(lang: Lang) {
    CURRENT.store(u8::from(lang == Lang::Ru), Ordering::Relaxed);
}

/// The language messages are formatted in on this thread.
pub fn language() -> Lang {
    if let Some(l) = PINNED.with(Cell::get) {
        return l;
    }
    match CURRENT.load(Ordering::Relaxed) {
        1 => Lang::Ru,
        _ => Lang::En,
    }
}

/// While the guard lives, this thread formats in `lang` whatever the process language is.
#[must_use = "the language is pinned only while the guard lives"]
pub fn scoped(lang: Lang) -> ScopedLang {
    ScopedLang {
        prev: PINNED.with(|p| p.replace(Some(lang))),
    }
}

/// Pins a language for the current thread, see [`scoped`].
#[derive(Debug)]
pub struct ScopedLang {
    prev: Option<Lang>,
}

impl Drop for ScopedLang {
    fn drop(&mut self) {
        PINNED.with(|p| p.set(self.prev));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes() {
        assert_eq!(Lang::from_code("ru_RU.UTF-8"), Lang::Ru);
        assert_eq!(Lang::from_code(" ru-RU "), Lang::Ru);
        assert_eq!(Lang::from_code("de"), Lang::En);
        assert_eq!(Lang::from_code(""), Lang::En);
    }

    #[test]
    fn a_scoped_language_nests_and_restores() {
        assert_eq!(language(), Lang::En);
        {
            let _a = scoped(Lang::Ru);
            assert_eq!(language(), Lang::Ru);
            {
                let _b = scoped(Lang::En);
                assert_eq!(language(), Lang::En);
            }
            assert_eq!(language(), Lang::Ru);
        }
        assert_eq!(language(), Lang::En);
    }
}
