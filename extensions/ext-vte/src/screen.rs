//! The terminal screen as the extension keeps it: text, line index, line hashes, and the
//! line-level diff of two screens (ADR-0001 T-2).
//!
//! VTE exposes only the visible screen, rows joined by `'\n'`, with trailing blanks trimmed.
//! Offsets are Unicode scalars. Everything here is bounded: a screen is cut at
//! [`MAX_SCREEN_BYTES`], a diff gives up (and says so) beyond [`DP_CELLS_MAX`] table cells.

/// A screen larger than this is cut (a hostile application must not make us copy megabytes).
pub const MAX_SCREEN_BYTES: usize = 256 * 1024;
/// Lines kept per screen.
pub const MAX_LINES: usize = 4096;
/// Largest middle section (old lines x new lines) the exact line diff handles.
pub const DP_CELLS_MAX: usize = 160_000;

fn hash_line(s: &str) -> u64 {
    // FNV-1a over the trimmed line: cheap, and collisions only cost a missed or extra line.
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.trim_end().bytes() {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// One screen snapshot.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Screen {
    text: String,
    /// Byte offset of the start of each line.
    starts: Vec<usize>,
    /// Scalar offset of the start of each line.
    char_starts: Vec<u32>,
    hashes: Vec<u64>,
    chars: u32,
}

impl Screen {
    pub fn new(text: &str) -> Self {
        let mut t = String::with_capacity(text.len().min(MAX_SCREEN_BYTES));
        for c in text.chars() {
            if t.len() + c.len_utf8() > MAX_SCREEN_BYTES {
                break;
            }
            if c != '\r' {
                t.push(c);
            }
        }
        let mut s = Screen {
            text: t,
            ..Screen::default()
        };
        s.index();
        s
    }

    fn index(&mut self) {
        self.starts.clear();
        self.char_starts.clear();
        self.hashes.clear();
        let mut chars: u32 = 0;
        let mut line_start = 0usize;
        let mut line_chars = 0u32;
        let bytes = self.text.as_bytes();
        let mut i = 0usize;
        // Lines are split at '\n'; text after the last '\n' is a line when it is not empty.
        self.starts.push(0);
        self.char_starts.push(0);
        while i < bytes.len() {
            let b = bytes[i];
            if b == b'\n' {
                let end = i;
                self.hashes
                    .push(hash_line(self.text.get(line_start..end).unwrap_or("")));
                chars = chars.saturating_add(line_chars + 1);
                line_chars = 0;
                line_start = i + 1;
                if self.starts.len() < MAX_LINES {
                    self.starts.push(line_start);
                    self.char_starts.push(chars);
                } else {
                    // Beyond the cap the remainder is one long last line.
                    self.hashes.pop();
                    break;
                }
            } else if b & 0xc0 != 0x80 {
                line_chars += 1;
            }
            i += 1;
        }
        if self.starts.len() == self.hashes.len() + 1 {
            let tail = self.text.get(line_start..).unwrap_or("");
            if tail.is_empty() && self.starts.len() > 1 {
                // The text ended with '\n': no phantom last line.
                self.starts.pop();
                self.char_starts.pop();
            } else {
                self.hashes.push(hash_line(tail));
            }
        }
        if self.text.is_empty() {
            self.starts.clear();
            self.char_starts.clear();
            self.hashes.clear();
        }
        self.chars = u32::try_from(self.text.chars().count()).unwrap_or(u32::MAX);
    }

    /// The whole text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Length in scalars.
    pub fn char_len(&self) -> u32 {
        self.chars
    }

    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    pub fn line_count(&self) -> usize {
        self.starts.len()
    }

    /// Line `i` without its newline.
    pub fn line(&self, i: usize) -> &str {
        let Some(&s) = self.starts.get(i) else {
            return "";
        };
        let e = match self.starts.get(i + 1) {
            Some(&n) => n.saturating_sub(1),
            None => self.text.len() - usize::from(self.text.ends_with('\n')),
        };
        self.text.get(s..e.max(s)).unwrap_or("")
    }

    /// Line index and column (in scalars) of scalar offset `off`; past the end it is the end of
    /// the last line.
    pub fn locate(&self, off: u32) -> (usize, u32) {
        if self.starts.is_empty() {
            return (0, 0);
        }
        if off >= self.chars && self.text.ends_with('\n') {
            // The caret sits on the empty row after the last line of text.
            return (self.starts.len(), 0);
        }
        let idx = self
            .char_starts
            .partition_point(|&s| s <= off)
            .saturating_sub(1);
        let col = off.saturating_sub(self.char_starts.get(idx).copied().unwrap_or(0));
        if idx + 1 == self.starts.len() {
            // Past the end of the text: the end of the last line.
            let len = u32::try_from(self.line(idx).chars().count()).unwrap_or(u32::MAX);
            return (idx, col.min(len));
        }
        (idx, col)
    }

    /// Insert `s` (`len` scalars) at scalar offset `at`. False when it does not fit.
    pub fn apply_insert(&mut self, at: u32, s: &str) -> bool {
        if at > self.chars || self.text.len() + s.len() > MAX_SCREEN_BYTES {
            return false;
        }
        let Some(byte) = self.byte_of(at) else {
            return false;
        };
        let cleaned: String = s.chars().filter(|c| *c != '\r').collect();
        self.text.insert_str(byte, &cleaned);
        self.index();
        true
    }

    /// Remove `len` scalars at scalar offset `at`. False when the range is outside the text.
    pub fn apply_delete(&mut self, at: u32, len: u32) -> bool {
        let Some(end) = at.checked_add(len) else {
            return false;
        };
        if end > self.chars {
            return false;
        }
        let (Some(b0), Some(b1)) = (self.byte_of(at), self.byte_of(end)) else {
            return false;
        };
        self.text.replace_range(b0..b1, "");
        self.index();
        true
    }

    fn byte_of(&self, char_off: u32) -> Option<usize> {
        if char_off == self.chars {
            return Some(self.text.len());
        }
        self.text
            .char_indices()
            .nth(char_off as usize)
            .map(|(b, _)| b)
    }
}

/// The result of comparing two screens line by line.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Diff {
    /// New-screen lines that took the place of an old line (same position in the edit script).
    pub changed: Vec<usize>,
    /// New-screen lines that no old line corresponds to (scrolled in, printed).
    pub inserted: Vec<usize>,
    /// Old lines that are gone without a replacement.
    pub removed: usize,
    /// New lines that equal an old line (possibly shifted by a scroll).
    pub matched: usize,
    /// Index of a matched, non-blank new line, the last one (`None` when nothing matched).
    pub last_matched: Option<usize>,
    /// The tables were too large: every line of the unequal middle is `inserted`.
    pub bulk: bool,
    /// For every new line, the old line it equals or replaces (`None` for an inserted line).
    pub old_of: Vec<Option<usize>>,
}

impl Diff {
    /// Every unmatched new line, in order.
    pub fn unmatched(&self) -> Vec<usize> {
        let mut v: Vec<usize> = self
            .changed
            .iter()
            .chain(self.inserted.iter())
            .copied()
            .collect();
        v.sort_unstable();
        v
    }

    pub fn is_same(&self) -> bool {
        self.changed.is_empty() && self.inserted.is_empty() && self.removed == 0
    }
}

/// Reusable work buffer of [`diff`].
#[derive(Debug, Default)]
pub struct DiffBuf {
    table: Vec<u16>,
}

/// Compare `old` and `new`: equal head and tail first, then a longest-common-subsequence over
/// the lines in between.
pub fn diff(old: &Screen, new: &Screen, buf: &mut DiffBuf) -> Diff {
    diff_with(old, new, None, buf)
}

/// [`diff`] with the new line `force_new` (if any) never equal to an old one: a prompt that
/// looks like the old prompt but is the one printed after the output above it.
pub fn diff_with(old: &Screen, new: &Screen, force_new: Option<usize>, buf: &mut DiffBuf) -> Diff {
    let forced: Vec<u64>;
    let b: &[u64] = match force_new {
        Some(f) if f < new.hashes.len() => {
            let mut v = new.hashes.clone();
            v[f] ^= 0x9e37_79b9_7f4a_7c15;
            forced = v;
            &forced
        }
        Some(_) | None => &new.hashes,
    };
    let a = &old.hashes;
    let mut d = Diff {
        old_of: vec![None; b.len()],
        ..Diff::default()
    };
    let mut head = 0usize;
    let m = a.len().min(b.len());
    while head < m && a[head] == b[head] {
        head += 1;
    }
    let mut tail = 0usize;
    while tail < m - head && a[a.len() - 1 - tail] == b[b.len() - 1 - tail] {
        tail += 1;
    }
    let mid_a = &a[head..a.len() - tail];
    let mid_b = &b[head..b.len() - tail];
    d.matched = head + tail;
    for i in 0..head {
        d.old_of[i] = Some(i);
    }
    for t in 0..tail {
        d.old_of[b.len() - 1 - t] = Some(a.len() - 1 - t);
    }
    // The last matched line that has text: the tail block first, then the head block.
    let blank = |i: usize| new.line(i).trim().is_empty();
    d.last_matched = (b.len() - tail..b.len())
        .rev()
        .find(|&i| !blank(i))
        .or_else(|| (0..head).rev().find(|&i| !blank(i)));
    if mid_a.is_empty() || mid_b.is_empty() {
        d.inserted.extend(head..head + mid_b.len());
        d.removed = mid_a.len();
        return d;
    }
    if mid_a.len().saturating_mul(mid_b.len()) > DP_CELLS_MAX {
        d.bulk = true;
        d.inserted.extend(head..head + mid_b.len());
        d.removed = mid_a.len();
        return d;
    }
    lcs_hunks(mid_a, mid_b, head, buf, new, &mut d);
    d
}

/// Longest common subsequence of two hash slices; every maximal run of unmatched lines becomes
/// a hunk whose first `min(del, ins)` new lines are `changed` (paired with the deleted lines in
/// order) and the rest `inserted`. `base` is the index of `a[0]` and `b[0]` in the full screens
/// (the equal head has the same length on both).
fn lcs_hunks(a: &[u64], b: &[u64], base: usize, buf: &mut DiffBuf, new: &Screen, d: &mut Diff) {
    let (n, m) = (a.len(), b.len());
    let w = m + 1;
    buf.table.clear();
    buf.table.resize((n + 1) * w, 0);
    // table[i][j] = LCS length of a[i..] and b[j..].
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            let v = if a[i] == b[j] {
                buf.table[(i + 1) * w + j + 1].saturating_add(1)
            } else {
                buf.table[(i + 1) * w + j].max(buf.table[i * w + j + 1])
            };
            buf.table[i * w + j] = v;
        }
    }
    let (mut i, mut j) = (0usize, 0usize);
    let mut run_del: Vec<usize> = Vec::new();
    let mut run_ins: Vec<usize> = Vec::new();
    let flush = |del: &mut Vec<usize>, ins: &mut Vec<usize>, d: &mut Diff| {
        let pair = del.len().min(ins.len());
        for (k, idx) in ins.iter().enumerate() {
            if k < pair {
                d.changed.push(*idx);
                d.old_of[*idx] = Some(del[k]);
            } else {
                d.inserted.push(*idx);
            }
        }
        d.removed += del.len() - pair;
        del.clear();
        ins.clear();
    };
    while i < n || j < m {
        if i < n && j < m && a[i] == b[j] {
            flush(&mut run_del, &mut run_ins, d);
            d.matched += 1;
            d.old_of[base + j] = Some(base + i);
            if !new.line(base + j).trim().is_empty() {
                d.last_matched = Some(d.last_matched.map_or(base + j, |p| p.max(base + j)));
            }
            i += 1;
            j += 1;
        } else if j < m && (i >= n || buf.table[i * w + j + 1] >= buf.table[(i + 1) * w + j]) {
            run_ins.push(base + j);
            j += 1;
        } else {
            run_del.push(base + i);
            i += 1;
        }
    }
    flush(&mut run_del, &mut run_ins, d);
}

#[cfg(test)]
#[allow(clippy::format_collect)]
mod tests {
    use super::*;

    fn scr(s: &str) -> Screen {
        Screen::new(s)
    }

    #[test]
    fn lines_and_locate_follow_scalars() {
        let s = scr("ab\nçd\n\nlast");
        assert_eq!(s.line_count(), 4);
        assert_eq!(s.line(1), "çd");
        assert_eq!(s.line(2), "");
        assert_eq!(s.line(3), "last");
        assert_eq!(s.locate(0), (0, 0));
        assert_eq!(s.locate(4), (1, 1));
        assert_eq!(s.locate(6), (2, 0));
        assert_eq!(s.locate(7), (3, 0));
        assert_eq!(s.locate(1000), (3, 4));
    }

    #[test]
    fn a_trailing_newline_makes_no_phantom_line() {
        let s = scr("a\nb\n");
        assert_eq!(s.line_count(), 2);
        assert_eq!(scr("").line_count(), 0);
        assert_eq!(scr("\n").line_count(), 1);
    }

    #[test]
    fn edits_keep_the_index_right() {
        let mut s = scr("hello\nworld\n");
        assert!(s.apply_insert(5, ", you"));
        assert_eq!(s.line(0), "hello, you");
        assert!(s.apply_delete(5, 5));
        assert_eq!(s.line(0), "hello");
        assert!(!s.apply_delete(10, 10));
        assert!(!s.apply_insert(99, "x"));
        assert_eq!(s.line(1), "world");
    }

    #[test]
    fn trailing_blanks_do_not_change_a_line_hash() {
        assert_eq!(hash_line("abc"), hash_line("abc   "));
        assert_ne!(hash_line("abc"), hash_line("abd"));
    }

    fn d(old: &str, new: &str) -> Diff {
        diff(&scr(old), &scr(new), &mut DiffBuf::default())
    }

    #[test]
    fn equal_screens_are_the_same() {
        let x = d("a\nb\n$ ", "a\nb\n$ ");
        assert!(x.is_same());
        assert_eq!(x.matched, 3);
    }

    #[test]
    fn a_scroll_inserts_the_bottom_lines_only() {
        let x = d("l1\nl2\nl3\nl4", "l3\nl4\nl5\nl6");
        assert_eq!(x.inserted, vec![2, 3]);
        assert!(x.changed.is_empty());
        assert_eq!(x.removed, 2);
        assert_eq!(x.matched, 2);
    }

    #[test]
    fn an_edited_prompt_line_is_changed_in_place() {
        let x = d("out\n$ l", "out\n$ ls");
        assert_eq!(x.changed, vec![1]);
        assert!(x.inserted.is_empty());
    }

    #[test]
    fn new_output_below_a_screen_that_is_not_full() {
        let x = d("$ ls", "$ ls\na b c\n$ ");
        assert_eq!(x.inserted, vec![1, 2]);
        assert_eq!(x.matched, 1);
    }

    #[test]
    fn a_reverse_scroll_inserts_at_the_top() {
        let x = d("l3\nl4\nl5", "l1\nl2\nl3");
        assert_eq!(x.inserted, vec![0, 1]);
    }

    #[test]
    fn scattered_changes_are_changed_lines() {
        let x = d(
            "h\ncpu 1\nstatic\nmem 2\nfoot",
            "h\ncpu 9\nstatic\nmem 7\nfoot",
        );
        assert_eq!(x.changed, vec![1, 3]);
        assert!(x.inserted.is_empty());
    }

    #[test]
    fn a_huge_unequal_middle_is_bulk_not_a_table() {
        let a: String = (0..500).map(|i| format!("a{i}\n")).collect();
        let b: String = (0..500).map(|i| format!("b{i}\n")).collect();
        let x = d(&a, &b);
        assert!(x.bulk);
        assert_eq!(x.inserted.len(), 500);
    }

    #[test]
    fn a_text_larger_than_the_cap_is_cut_on_a_scalar() {
        let big = "é".repeat(MAX_SCREEN_BYTES);
        let s = scr(&big);
        assert!(s.text().len() <= MAX_SCREEN_BYTES);
        assert!(s.char_len() > 0);
    }
}
