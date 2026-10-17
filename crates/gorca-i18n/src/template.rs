//! The message template language of the catalogs.
//!
//! ```text
//! key = plain text
//! key = {n} {n#item|items}            plural forms of the number `n`
//! key = {name} stuck, tap {name}      named placeholders
//! key = {{braces}} are doubled
//! ```
//!
//! The forms of `{n#a|b|c}` follow the language's plural rule ([`Lang::plural_index`]): English has
//! two (`one|other`), Russian three (`one|few|many`: 1 элемент, 2 элемента, 5 элементов). A message
//! with fewer forms than the rule picks uses its last one.

use std::fmt::Write as _;

use crate::lang::Lang;

/// A placeholder value.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Arg<'a> {
    Str(&'a str),
    Num(i64),
}

impl<'a> From<&'a str> for Arg<'a> {
    fn from(s: &'a str) -> Self {
        Arg::Str(s)
    }
}

impl<'a> From<&'a String> for Arg<'a> {
    fn from(s: &'a String) -> Self {
        Arg::Str(s)
    }
}

macro_rules! arg_from_int {
    ($($t:ty),*) => {$(
        impl From<$t> for Arg<'_> {
            fn from(n: $t) -> Self {
                Arg::Num(i64::from(n))
            }
        }
    )*};
}
arg_from_int!(i8, i16, i32, i64, u8, u16, u32);

impl From<usize> for Arg<'_> {
    fn from(n: usize) -> Self {
        Arg::Num(i64::try_from(n).unwrap_or(i64::MAX))
    }
}

impl From<u64> for Arg<'_> {
    fn from(n: u64) -> Self {
        Arg::Num(i64::try_from(n).unwrap_or(i64::MAX))
    }
}

impl Arg<'_> {
    fn number(self) -> Option<i64> {
        match self {
            Arg::Num(n) => Some(n),
            Arg::Str(s) => s.trim().parse().ok(),
        }
    }
}

/// A template that does not parse.
#[derive(Debug, Clone, Eq, PartialEq, thiserror::Error)]
pub enum TemplateError {
    #[error("`{{` is not closed")]
    Unclosed,
    #[error("a stray `}}` (write `}}}}` for a brace)")]
    Stray,
    #[error("a placeholder needs a name made of letters, digits and `_`")]
    BadName,
    #[error("a placeholder inside a plural form")]
    NestedPlaceholder,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum Piece {
    Lit(Box<str>),
    Var(Box<str>),
    Plural(Box<str>, Box<[Box<str>]>),
}

/// A parsed message.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Template {
    pieces: Box<[Piece]>,
    /// The whole text when there is no placeholder.
    plain: Option<Box<str>>,
}

fn valid_name(n: &str) -> bool {
    !n.is_empty() && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

impl Template {
    /// Parse a template.
    pub fn parse(src: &str) -> Result<Self, TemplateError> {
        let mut pieces: Vec<Piece> = Vec::new();
        let mut lit = String::new();
        let mut it = src.chars().peekable();
        while let Some(c) = it.next() {
            match c {
                '{' if it.peek() == Some(&'{') => {
                    it.next();
                    lit.push('{');
                }
                '}' if it.peek() == Some(&'}') => {
                    it.next();
                    lit.push('}');
                }
                '}' => return Err(TemplateError::Stray),
                '{' => {
                    let mut body = String::new();
                    loop {
                        match it.next() {
                            Some('}') => break,
                            Some('{') => return Err(TemplateError::NestedPlaceholder),
                            Some(ch) => body.push(ch),
                            None => return Err(TemplateError::Unclosed),
                        }
                    }
                    if !lit.is_empty() {
                        pieces.push(Piece::Lit(std::mem::take(&mut lit).into()));
                    }
                    match body.split_once('#') {
                        None => {
                            if !valid_name(&body) {
                                return Err(TemplateError::BadName);
                            }
                            pieces.push(Piece::Var(body.into()));
                        }
                        Some((name, forms)) => {
                            if !valid_name(name) {
                                return Err(TemplateError::BadName);
                            }
                            let forms: Box<[Box<str>]> = forms.split('|').map(Box::from).collect();
                            pieces.push(Piece::Plural(name.into(), forms));
                        }
                    }
                }
                _ => lit.push(c),
            }
        }
        if pieces.is_empty() {
            return Ok(Template {
                pieces: Box::new([]),
                plain: Some(lit.into()),
            });
        }
        if !lit.is_empty() {
            pieces.push(Piece::Lit(lit.into()));
        }
        Ok(Template {
            pieces: pieces.into(),
            plain: None,
        })
    }

    /// The empty message.
    pub fn empty() -> Self {
        Template {
            pieces: Box::new([]),
            plain: Some(Box::from("")),
        }
    }

    /// The text of a message without placeholders.
    pub fn plain(&self) -> Option<&str> {
        self.plain.as_deref()
    }

    /// The placeholder names, in order of appearance (for catalog parity checks).
    pub fn names(&self) -> Vec<&str> {
        let mut v: Vec<&str> = self
            .pieces
            .iter()
            .filter_map(|p| match p {
                Piece::Var(n) | Piece::Plural(n, _) => Some(&**n),
                Piece::Lit(_) => None,
            })
            .collect();
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Append the message to `out`. A missing argument formats as nothing.
    pub fn format_into(&self, out: &mut String, lang: Lang, args: &[(&str, Arg<'_>)]) {
        if let Some(p) = &self.plain {
            out.push_str(p);
            return;
        }
        let find = |name: &str| args.iter().find(|(n, _)| *n == name).map(|(_, a)| *a);
        for p in &*self.pieces {
            match p {
                Piece::Lit(s) => out.push_str(s),
                Piece::Var(n) => match find(n) {
                    Some(Arg::Str(s)) => out.push_str(s),
                    Some(Arg::Num(v)) => {
                        let _ = write!(out, "{v}");
                    }
                    None => {}
                },
                Piece::Plural(n, forms) => {
                    let idx = find(n)
                        .and_then(Arg::number)
                        .map_or(0, |v| lang.plural_index(v));
                    if let Some(f) = forms.get(idx).or_else(|| forms.last()) {
                        out.push_str(f);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(src: &str, lang: Lang, args: &[(&str, Arg<'_>)]) -> String {
        let t = Template::parse(src).unwrap();
        let mut s = String::new();
        t.format_into(&mut s, lang, args);
        s
    }

    #[test]
    fn plain_text_is_kept_whole() {
        let t = Template::parse("list item, with: punctuation").unwrap();
        assert_eq!(t.plain(), Some("list item, with: punctuation"));
    }

    #[test]
    fn placeholders_and_escapes() {
        assert_eq!(
            f(
                "{a} stuck, tap {a} {{x}}",
                Lang::En,
                &[("a", Arg::Str("Insert"))]
            ),
            "Insert stuck, tap Insert {x}"
        );
        assert_eq!(f("{n} items", Lang::En, &[("n", 7.into())]), "7 items");
    }

    #[test]
    fn english_plurals() {
        for (n, want) in [(0, "0 items"), (1, "1 item"), (2, "2 items"), (11, "11 items")] {
            assert_eq!(
                f("{n} item{n#|s}", Lang::En, &[("n", n.into())]),
                want.to_owned()
            );
        }
    }

    #[test]
    fn russian_plurals_follow_one_few_many() {
        let t = "{n} {n#элемент|элемента|элементов}";
        for (n, want) in [
            (0, "0 элементов"),
            (1, "1 элемент"),
            (2, "2 элемента"),
            (4, "4 элемента"),
            (5, "5 элементов"),
            (11, "11 элементов"),
            (12, "12 элементов"),
            (14, "14 элементов"),
            (21, "21 элемент"),
            (22, "22 элемента"),
            (25, "25 элементов"),
            (101, "101 элемент"),
            (111, "111 элементов"),
            (112, "112 элементов"),
        ] {
            assert_eq!(f(t, Lang::Ru, &[("n", n.into())]), want, "{n}");
        }
    }

    #[test]
    fn a_numeric_string_argument_drives_a_plural() {
        assert_eq!(
            f("{n#один|два|много}", Lang::Ru, &[("n", Arg::Str("3"))]),
            "два"
        );
    }

    #[test]
    fn bad_templates_are_errors() {
        assert_eq!(Template::parse("a {b"), Err(TemplateError::Unclosed));
        assert_eq!(Template::parse("a } b"), Err(TemplateError::Stray));
        assert_eq!(Template::parse("a {} b"), Err(TemplateError::BadName));
        assert_eq!(Template::parse("{a{b}}"), Err(TemplateError::NestedPlaceholder));
    }

    #[test]
    fn names_are_listed_once_and_sorted() {
        let t = Template::parse("{b} {a} {b#x|y}").unwrap();
        assert_eq!(t.names(), ["a", "b"]);
    }

    #[test]
    fn a_missing_argument_is_empty() {
        assert_eq!(f("[{x}]", Lang::En, &[]), "[]");
    }
}
