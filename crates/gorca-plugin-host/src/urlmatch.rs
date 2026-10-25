// SPDX-License-Identifier: GPL-3.0-or-later
//! Structured URL matching for plugin activation (`[match] urls`, ADR-0026 section 2.2).
//!
//! A plugin that matches a document can read its page content, so the match must not be a flat
//! string glob (`https://web.telegram.org*` would match `https://web.telegram.org.evil.com/`).
//! Both the pattern and the URL are split into scheme, host, port, path, query and fragment and
//! each part is matched on its own:
//!
//! * the scheme must be equal; a pattern accepts `http` and `https` only (`file` only through
//!   [`UrlPattern::parse_dev`], the private-session test knob);
//! * the host is lower-cased and IDN labels are converted to punycode on both sides; a host
//!   wildcard is a leading `*.` that matches one or more whole DNS labels and never the bare
//!   suffix; a `*` anywhere else in the host is a parse error;
//! * the port defaults to the scheme's (443, 80) on both sides;
//! * userinfo (`user@`) is refused in a pattern, and a URL that has any never matches;
//! * `*` in the path matches any run that contains neither `?` nor `#`; a pattern without `?` or
//!   `#` ignores the URL's query and fragment, a pattern with one matches that part (its `*`
//!   never crosses into the next part).

use std::fmt;

/// Why a pattern was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UrlPatternError {
    /// Not `scheme://authority/...`.
    #[error("not a URL pattern: {0}")]
    Shape(String),
    /// Scheme other than http/https.
    #[error("scheme `{0}` is not allowed in a web pattern")]
    Scheme(String),
    /// Userinfo in the authority.
    #[error("userinfo in a URL pattern")]
    Userinfo,
    /// A wildcard that is not a whole leading label.
    #[error("host wildcard must be a leading `*.` label: {0}")]
    HostWildcard(String),
    /// Bad port.
    #[error("bad port in URL pattern")]
    Port,
    /// Empty or invalid host.
    #[error("bad host in URL pattern")]
    Host,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum HostPat {
    Exact(String),
    /// `*.suffix`: one or more whole labels in front of `suffix`.
    Suffix(String),
}

/// A validated activation pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlPattern {
    scheme: String,
    host: HostPat,
    port: u16,
    path: String,
    query: Option<String>,
    fragment: Option<String>,
}

struct Parts<'a> {
    scheme: String,
    authority: &'a str,
    path: &'a str,
    query: Option<&'a str>,
    fragment: Option<&'a str>,
}

fn split(s: &str) -> Option<Parts<'_>> {
    let (scheme, rest) = s.split_once("://")?;
    if scheme.is_empty() || !scheme.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'+' || b == b'-' || b == b'.') {
        return None;
    }
    let (rest, fragment) = match rest.split_once('#') {
        Some((a, f)) => (a, Some(f)),
        None => (rest, None),
    };
    let (rest, query) = match rest.split_once('?') {
        Some((a, q)) => (a, Some(q)),
        None => (rest, None),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    Some(Parts {
        scheme: scheme.to_ascii_lowercase(),
        authority,
        path,
        query,
        fragment,
    })
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "https" => Some(443),
        "http" => Some(80),
        _ => None,
    }
}

/// Split `host[:port]`; an IPv6 literal keeps its brackets.
fn host_port(a: &str) -> Option<(&str, Option<&str>)> {
    if let Some(rest) = a.strip_prefix('[') {
        let end = rest.find(']')?;
        let host = &a[..end + 2];
        let after = &rest[end + 1..];
        return match after {
            "" => Some((host, None)),
            _ => after.strip_prefix(':').map(|p| (host, Some(p))),
        };
    }
    match a.rsplit_once(':') {
        Some((h, p)) => Some((h, Some(p))),
        None => Some((a, None)),
    }
}

/// Lower-case a host and convert non-ASCII labels to punycode. `None` for an empty label or an
/// unencodable label. One trailing dot (the absolute form) is dropped.
fn normalise_host(h: &str) -> Option<String> {
    if h.starts_with('[') {
        return Some(h.to_ascii_lowercase());
    }
    let h = h.strip_suffix('.').unwrap_or(h);
    if h.is_empty() {
        return None;
    }
    let mut out: Vec<String> = Vec::new();
    for label in h.split('.') {
        if label.is_empty() {
            return None;
        }
        let lower: String = label.chars().flat_map(char::to_lowercase).collect();
        if lower.is_ascii() {
            out.push(lower);
        } else {
            out.push(format!("xn--{}", punycode_encode(&lower)?));
        }
    }
    Some(out.join("."))
}

/// RFC 3492 encoder (the basic code points first, then the deltas).
fn punycode_encode(input: &str) -> Option<String> {
    const BASE: u32 = 36;
    const TMIN: u32 = 1;
    const TMAX: u32 = 26;
    const SKEW: u32 = 38;
    const DAMP: u32 = 700;
    let digit = |d: u32| -> char {
        if d < 26 {
            char::from(b'a' + u8::try_from(d).unwrap_or(0))
        } else {
            char::from(b'0' + u8::try_from(d - 26).unwrap_or(0))
        }
    };
    let adapt = |mut delta: u32, num: u32, first: bool| -> u32 {
        delta = if first { delta / DAMP } else { delta / 2 };
        delta += delta / num;
        let mut k = 0;
        while delta > ((BASE - TMIN) * TMAX) / 2 {
            delta /= BASE - TMIN;
            k += BASE;
        }
        k + (BASE - TMIN + 1) * delta / (delta + SKEW)
    };
    let cps: Vec<u32> = input.chars().map(u32::from).collect();
    let mut out: String = input.chars().filter(char::is_ascii).collect();
    let b = u32::try_from(out.len()).ok()?;
    let mut h = b;
    if b > 0 {
        out.push('-');
    }
    let (mut n, mut delta, mut bias) = (128u32, 0u32, 72u32);
    let total = u32::try_from(cps.len()).ok()?;
    while h < total {
        let m = cps.iter().copied().filter(|c| *c >= n).min()?;
        delta = delta.checked_add((m - n).checked_mul(h + 1)?)?;
        n = m;
        for &c in &cps {
            if c < n {
                delta = delta.checked_add(1)?;
            }
            if c == n {
                let mut q = delta;
                let mut k = BASE;
                loop {
                    let t = if k <= bias {
                        TMIN
                    } else if k >= bias + TMAX {
                        TMAX
                    } else {
                        k - bias
                    };
                    if q < t {
                        break;
                    }
                    out.push(digit(t + (q - t) % (BASE - t)));
                    q = (q - t) / (BASE - t);
                    k += BASE;
                }
                out.push(digit(q));
                bias = adapt(delta, h + 1, h == b);
                delta = 0;
                h += 1;
            }
        }
        delta = delta.checked_add(1)?;
        n = n.checked_add(1)?;
    }
    Some(out)
}

/// `*` matches any run of characters that are not `?` or `#`.
fn glob_part(pattern: &str, text: &str) -> bool {
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && p[pi] == b'*' {
            star = Some(pi);
            pi += 1;
            mark = ti;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == b'*' {
        pi += 1;
    }
    pi == p.len()
}

impl UrlPattern {
    /// Parse a manifest pattern (http and https only).
    pub fn parse(s: &str) -> Result<Self, UrlPatternError> {
        Self::parse_with(s, false)
    }

    /// Parse a pattern that may also name `file://` (the private-session test knob only).
    pub fn parse_dev(s: &str) -> Result<Self, UrlPatternError> {
        Self::parse_with(s, true)
    }

    fn parse_with(s: &str, allow_file: bool) -> Result<Self, UrlPatternError> {
        let p = split(s).ok_or_else(|| UrlPatternError::Shape(s.to_owned()))?;
        let file = p.scheme == "file";
        if !(matches!(p.scheme.as_str(), "http" | "https") || (allow_file && file)) {
            return Err(UrlPatternError::Scheme(p.scheme));
        }
        if p.authority.contains('@') {
            return Err(UrlPatternError::Userinfo);
        }
        if file {
            if !p.authority.is_empty() {
                return Err(UrlPatternError::Host);
            }
            return Ok(Self {
                scheme: p.scheme,
                host: HostPat::Exact(String::new()),
                port: 0,
                path: p.path.to_owned(),
                query: p.query.map(str::to_owned),
                fragment: p.fragment.map(str::to_owned),
            });
        }
        let (h, port) = host_port(p.authority).ok_or(UrlPatternError::Host)?;
        let host = if let Some(suffix) = h.strip_prefix("*.") {
            if suffix.contains('*') || suffix.is_empty() {
                return Err(UrlPatternError::HostWildcard(h.to_owned()));
            }
            HostPat::Suffix(normalise_host(suffix).ok_or(UrlPatternError::Host)?)
        } else {
            if h.contains('*') {
                return Err(UrlPatternError::HostWildcard(h.to_owned()));
            }
            HostPat::Exact(normalise_host(h).ok_or(UrlPatternError::Host)?)
        };
        let port = match port {
            None => default_port(&p.scheme).ok_or(UrlPatternError::Port)?,
            Some(x) => x.parse::<u16>().map_err(|_| UrlPatternError::Port)?,
        };
        Ok(Self {
            scheme: p.scheme,
            host,
            port,
            path: p.path.to_owned(),
            query: p.query.map(str::to_owned),
            fragment: p.fragment.map(str::to_owned),
        })
    }

    /// True when `url` (as a browser serialises it) satisfies the pattern.
    pub fn matches(&self, url: &str) -> bool {
        let Some(u) = split(url) else { return false };
        if u.scheme != self.scheme || u.authority.contains('@') {
            return false;
        }
        if self.scheme == "file" {
            if !u.authority.is_empty() {
                return false;
            }
        } else {
            let Some((h, port)) = host_port(u.authority) else {
                return false;
            };
            let Some(host) = normalise_host(h) else {
                return false;
            };
            let port = match port {
                None => default_port(&u.scheme),
                Some(x) => x.parse::<u16>().ok(),
            };
            if port != Some(self.port) {
                return false;
            }
            let host_ok = match &self.host {
                HostPat::Exact(e) => &host == e,
                HostPat::Suffix(s) => host
                    .strip_suffix(s.as_str())
                    .and_then(|front| front.strip_suffix('.'))
                    .is_some_and(|front| !front.is_empty()),
            };
            if !host_ok {
                return false;
            }
        }
        if !glob_part(&self.path, u.path) {
            return false;
        }
        if let Some(q) = &self.query
            && !u.query.is_some_and(|uq| glob_part(q, uq))
        {
            return false;
        }
        if let Some(f) = &self.fragment
            && !u.fragment.is_some_and(|uf| glob_part(f, uf))
        {
            return false;
        }
        true
    }
}

impl fmt::Display for UrlPattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let host = match &self.host {
            HostPat::Exact(h) => h.clone(),
            HostPat::Suffix(s) => format!("*.{s}"),
        };
        write!(f, "{}://{}:{}{}", self.scheme, host, self.port, self.path)?;
        if let Some(q) = &self.query {
            write!(f, "?{q}")?;
        }
        if let Some(fr) = &self.fragment {
            write!(f, "#{fr}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(s: &str) -> UrlPattern {
        UrlPattern::parse(s).expect("pattern")
    }

    #[test]
    fn the_evil_suffix_does_not_match() {
        let pat = p("https://web.telegram.org/*");
        assert!(pat.matches("https://web.telegram.org/a/"));
        assert!(!pat.matches("https://web.telegram.org.evil.com/a/"));
        assert!(!pat.matches("https://evilweb.telegram.org/a/"));
        // A wildcard glued to the host is refused outright.
        assert!(matches!(
            UrlPattern::parse("https://web.telegram.org*"),
            Err(UrlPatternError::HostWildcard(_))
        ));
        assert!(UrlPattern::parse("https://*/a").is_err());
        assert!(UrlPattern::parse("https://we*.org/a").is_err());
    }

    #[test]
    fn userinfo_is_refused_both_ways() {
        let pat = p("https://web.telegram.org/*");
        assert!(!pat.matches("https://web.telegram.org@evil.com/"));
        assert!(!pat.matches("https://user:pw@web.telegram.org/"));
        assert_eq!(
            UrlPattern::parse("https://user@web.telegram.org/"),
            Err(UrlPatternError::Userinfo)
        );
    }

    #[test]
    fn ports_default_per_scheme() {
        let pat = p("https://web.telegram.org/*");
        assert!(pat.matches("https://web.telegram.org:443/a"));
        assert!(!pat.matches("https://web.telegram.org:8443/a"));
        assert!(p("https://x.org:8443/*").matches("https://x.org:8443/a"));
        assert!(!p("https://x.org:8443/*").matches("https://x.org/a"));
        assert!(!p("http://x.org/*").matches("https://x.org/a"));
        assert!(UrlPattern::parse("https://x.org:99999/*").is_err());
    }

    #[test]
    fn case_and_trailing_dot() {
        let pat = p("https://Web.Telegram.ORG/a/*");
        assert!(pat.matches("HTTPS://WEB.TELEGRAM.ORG/a/x"));
        assert!(pat.matches("https://web.telegram.org./a/x"));
        // The path stays case sensitive.
        assert!(!pat.matches("https://web.telegram.org/A/x"));
    }

    #[test]
    fn idn_is_compared_as_punycode() {
        // bücher.example is xn--bcher-kva.example.
        let pat = p("https://bücher.example/*");
        assert!(pat.matches("https://xn--bcher-kva.example/x"));
        assert!(pat.matches("https://BÜCHER.example/x"));
        assert!(!pat.matches("https://bucher.example/x"));
        let by_puny = p("https://xn--bcher-kva.example/*");
        assert!(by_puny.matches("https://bücher.example/x"));
        assert_eq!(punycode_encode("bücher").as_deref(), Some("bcher-kva"));
        assert_eq!(punycode_encode("münchen").as_deref(), Some("mnchen-3ya"));
    }

    #[test]
    fn host_wildcards_are_whole_labels_and_never_the_bare_suffix() {
        let pat = p("https://*.telegram.org/*");
        assert!(pat.matches("https://web.telegram.org/a"));
        assert!(pat.matches("https://a.b.telegram.org/a"));
        assert!(!pat.matches("https://telegram.org/a"));
        assert!(!pat.matches("https://eviltelegram.org/a"));
        assert!(!pat.matches("https://web.telegram.org.evil.com/a"));
        assert!(UrlPattern::parse("https://*telegram.org/*").is_err());
        assert!(UrlPattern::parse("https://*.*.org/*").is_err());
    }

    #[test]
    fn star_never_crosses_into_query_or_fragment() {
        let pat = p("https://web.telegram.org/a/*");
        // A path-only pattern ignores query and fragment of the URL.
        assert!(pat.matches("https://web.telegram.org/a/x?tgaddr=1#frag"));
        // But a star in the path does not swallow them: this one wants a literal `?`.
        assert!(!p("https://x.org/a*b").matches("https://x.org/a?b"));
        assert!(!p("https://x.org/a*b").matches("https://x.org/a#b"));
        // An explicit query pattern is matched against the query alone.
        let q = p("https://x.org/s?q=*");
        assert!(q.matches("https://x.org/s?q=abc"));
        assert!(!q.matches("https://x.org/s"));
        let f = p("https://x.org/s#/chat/*");
        assert!(f.matches("https://x.org/s#/chat/12"));
        assert!(!f.matches("https://x.org/s"));
    }

    #[test]
    fn only_http_and_https_are_web_schemes() {
        for bad in ["ftp://x.org/*", "javascript://x.org/*", "chrome://settings/*", "file:///etc/*", "x.org/*"] {
            assert!(UrlPattern::parse(bad).is_err(), "{bad}");
        }
        let dev = UrlPattern::parse_dev("file:///a/tests/*").expect("dev");
        assert!(dev.matches("file:///a/tests/x.html"));
        assert!(!dev.matches("https://a/tests/x.html"));
    }
}
