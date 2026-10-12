/// Minimal glob: `*` matches any run of characters (including none), `?` exactly one.
/// Iterative, O(n*m) worst case without recursion, so hostile manifests cannot blow the stack.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        match p.get(pi) {
            Some('*') => {
                star = Some((pi, ti));
                pi += 1;
            }
            Some(&c) if c == '?' || c == t[ti] => {
                pi += 1;
                ti += 1;
            }
            Some(_) | None => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn literals_and_wildcards() {
        assert!(glob_match("firefox", "firefox"));
        assert!(!glob_match("firefox", "firefox-esr"));
        assert!(glob_match("firefox*", "firefox-esr"));
        assert!(glob_match("*term*", "gnome-terminal-server"));
        assert!(glob_match("gtk?", "gtk4"));
        assert!(!glob_match("gtk?", "gtk"));
        assert!(glob_match("*", ""));
        assert!(glob_match("", ""));
        assert!(!glob_match("", "x"));
    }

    #[test]
    fn pathological_patterns_terminate() {
        let pattern = "a*".repeat(200) + "b";
        let text = "a".repeat(400);
        assert!(!glob_match(&pattern, &text));
    }
}
