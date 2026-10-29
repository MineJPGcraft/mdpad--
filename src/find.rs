//! Plain-text search over the document source.
//!
//! Deliberately free of egui and of the document model: it takes the raw source
//! string, returns **byte** ranges into it, and owns nothing but the query and a
//! cursor over the current result set. That keeps the arithmetic — which is the
//! part that breaks on multi-byte text — testable without a window.
//!
//! Byte ranges are what the rest of the editor speaks: [`Document::replace_span`]
//! and the block spans are all byte offsets, so a match can be handed straight to
//! either without a second conversion that could disagree with the first.
//!
//! [`Document::replace_span`]: crate::doc::model::Document::replace_span

use std::ops::Range;

/// Case comparison for a search.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaseMode {
    /// Compare bytes exactly.
    Sensitive,
    /// Compare ASCII letters case-insensitively.
    Insensitive,
}

impl CaseMode {
    fn sensitive(self) -> bool {
        matches!(self, CaseMode::Sensitive)
    }
}

impl Default for CaseMode {
    /// Case-insensitive, which is what a search box does before it is told
    /// otherwise.
    fn default() -> Self {
        CaseMode::Insensitive
    }
}

/// Every non-overlapping occurrence of `needle` in `haystack`, in order.
///
/// Matches never overlap: `aa` occurs twice in `aaaa`, not three times. That
/// matches what a user sees when they hit "next" repeatedly, and it is what makes
/// [`replace_all`] safe to call with the result.
///
/// Case-insensitive search folds **ASCII** only. Unicode folding is not applied
/// because it can change a character's UTF-8 length (`İ` lowercases to two
/// chars), which would shift every later offset and corrupt the document on
/// replace. CJK — where case does not exist — is unaffected either way.
pub fn find_all(haystack: &str, needle: &str, case: CaseMode) -> Vec<Range<usize>> {
    if needle.is_empty() {
        return Vec::new();
    }
    if case.sensitive() {
        return collect(haystack, needle);
    }
    if haystack.is_ascii() && needle.is_ascii() {
        let folded = haystack.to_ascii_lowercase();
        return collect(&folded, &needle.to_ascii_lowercase());
    }
    collect(haystack, needle)
}

fn collect(haystack: &str, needle: &str) -> Vec<Range<usize>> {
    haystack
        .match_indices(needle)
        .map(|(at, found)| at..at + found.len())
        .collect()
}

/// Rewrites `haystack`, replacing each range in `matches` with `replacement`.
///
/// Ranges that are out of order, outside the string, or not on a character
/// boundary are skipped rather than trusted: this function is the one place where
/// a stale offset copied from an earlier frame could otherwise slice through a
/// UTF-8 sequence and panic.
pub fn replace_all(haystack: &str, matches: &[Range<usize>], replacement: &str) -> String {
    let mut out = String::with_capacity(haystack.len());
    let mut cursor = 0;
    for m in matches {
        if m.start < cursor
            || m.end < m.start
            || m.end > haystack.len()
            || !haystack.is_char_boundary(m.start)
            || !haystack.is_char_boundary(m.end)
        {
            continue;
        }
        out.push_str(&haystack[cursor..m.start]);
        out.push_str(replacement);
        cursor = m.end;
    }
    out.push_str(&haystack[cursor..]);
    out
}

/// A query plus a cursor over its matches.
///
/// `current` is `None` before the first jump, so the first "next" lands on the
/// first match instead of skipping past it.
#[derive(Clone, Debug, Default)]
pub struct Search {
    /// What to look for.
    pub query: String,
    /// What to put in its place.
    pub replacement: String,
    /// Whether to compare case.
    pub case: CaseMode,
    matches: Vec<Range<usize>>,
    current: Option<usize>,
}

impl Search {
    /// Recomputes the match list against `haystack`.
    ///
    /// Called whenever the query or the document changes. The cursor is kept by
    /// **index**, not by offset: after a replacement the offsets all shift, but
    /// the index still names the next match the user has not seen yet, which is
    /// exactly where editing should resume. An index that no longer exists (the
    /// text shrank, or the query changed) drops to "no cursor" so the next jump
    /// starts from the top.
    pub fn refresh(&mut self, haystack: &str) {
        self.matches = find_all(haystack, &self.query, self.case);
        self.current = match self.current {
            Some(i) if i < self.matches.len() => Some(i),
            _ => None,
        };
    }

    /// Drops the cursor, so the next jump starts from the beginning.
    pub fn reset_cursor(&mut self) {
        self.current = None;
    }

    /// The match the cursor is on, if any.
    pub fn current_match(&self) -> Option<Range<usize>> {
        self.current.and_then(|i| self.matches.get(i)).cloned()
    }

    /// How many matches the last [`Self::refresh`] found.
    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// Whether the last [`Self::refresh`] found nothing.
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }

    /// 1-based position of the cursor, for a "3 / 12" label.
    pub fn position(&self) -> Option<usize> {
        self.current.map(|i| i + 1)
    }

    /// Moves the cursor one match forward or back, wrapping around.
    ///
    /// Returns the match to focus. A forward step from "no cursor" selects the
    /// first match; a backward step from there selects the last, which is what a
    /// user pressing Shift+Enter expects.
    pub fn step(&mut self, forward: bool) -> Option<Range<usize>> {
        if self.matches.is_empty() {
            self.current = None;
            return None;
        }
        let len = self.matches.len();
        self.current = Some(match self.current {
            None if forward => 0,
            None => len - 1,
            Some(i) if forward => (i + 1) % len,
            Some(i) => (i + len - 1) % len,
        });
        self.current_match()
    }

    /// The match list, for callers that rewrite the whole document at once.
    pub fn matches(&self) -> &[Range<usize>] {
        &self.matches
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ranges(text: &str, needle: &str) -> Vec<(usize, usize)> {
        find_all(text, needle, CaseMode::Sensitive)
            .into_iter()
            .map(|r| (r.start, r.end))
            .collect()
    }

    #[test]
    fn finds_every_occurrence_in_order() {
        assert_eq!(ranges("one two one", "one"), vec![(0, 3), (8, 11)]);
    }

    #[test]
    fn matches_never_overlap() {
        // Three overlapping windows exist textually; two non-overlapping matches.
        assert_eq!(ranges("aaaa", "aa"), vec![(0, 2), (2, 4)]);
    }

    #[test]
    fn empty_needle_matches_nothing() {
        assert!(find_all("anything", "", CaseMode::Insensitive).is_empty());
    }

    #[test]
    fn needle_longer_than_haystack_matches_nothing() {
        assert!(find_all("ab", "abcdef", CaseMode::Sensitive).is_empty());
    }

    #[test]
    fn ascii_case_folding_keeps_offsets() {
        let found = find_all("hello HELLO", "hello", CaseMode::Insensitive);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0], 0..5);
        assert_eq!(found[1], 6..11);
        // `HELLO` is not `hello` when case matters.
        assert_eq!(find_all("hello HELLO", "hello", CaseMode::Sensitive).len(), 1);
    }

    #[test]
    fn cjk_offsets_are_byte_offsets() {
        // Each ideograph is three bytes, so the second occurrence starts at 9.
        let text = "中文测试中文";
        let found = find_all(text, "中文", CaseMode::Insensitive);
        assert_eq!(found, vec![0..6, 12..18]);
        // Slicing at the reported offsets must not panic.
        assert_eq!(&text[found[1].clone()], "中文");
    }

    #[test]
    fn non_ascii_text_still_searches_insensitively_as_exact_match() {
        // Folding is ASCII-only by design; an exact non-ASCII needle still works.
        // `Grüße` occupies bytes 0..7, so the space is at 7 and the match at 8.
        let found = find_all("Grüße grüße", "grüße", CaseMode::Insensitive);
        assert_eq!(found, vec![8..15]);
        assert_eq!(&"Grüße grüße"[found[0].clone()], "grüße");
    }

    #[test]
    fn replace_all_rewrites_only_the_matches() {
        let text = "cat scat cat";
        let matches = find_all(text, "cat", CaseMode::Sensitive);
        assert_eq!(replace_all(text, &matches, "dog"), "dog sdog dog");
    }

    #[test]
    fn replace_all_with_multibyte_and_multiline_text() {
        let text = "第一行 中文\n第二行 中文\n";
        let matches = find_all(text, "中文", CaseMode::Sensitive);
        assert_eq!(replace_all(text, &matches, "English"), "第一行 English\n第二行 English\n");
    }

    #[test]
    fn replace_all_skips_stale_and_misaligned_ranges() {
        let text = "中文";
        // 1..2 splits a three-byte sequence; 0..99 runs past the end; the last
        // one is inverted. None may panic, and none may be applied.
        let inverted = std::ops::Range { start: 4, end: 2 };
        let bad = vec![1..2, 0..99, inverted];
        assert_eq!(replace_all(text, &bad, "x"), "中文");
    }

    #[test]
    fn replace_all_accepts_an_empty_replacement() {
        let text = "a-b-c";
        let matches = find_all(text, "-", CaseMode::Sensitive);
        assert_eq!(replace_all(text, &matches, ""), "abc");
    }

    #[test]
    fn first_step_forward_selects_the_first_match() {
        let mut search = Search {
            query: "a".to_owned(),
            ..Search::default()
        };
        search.refresh("a b a");
        assert_eq!(search.position(), None);
        assert_eq!(search.step(true), Some(0..1));
        assert_eq!(search.position(), Some(1));
        assert_eq!(search.step(true), Some(4..5));
        // Wraps back to the start.
        assert_eq!(search.step(true), Some(0..1));
    }

    #[test]
    fn first_step_backward_selects_the_last_match() {
        let mut search = Search {
            query: "a".to_owned(),
            ..Search::default()
        };
        search.refresh("a b a");
        assert_eq!(search.step(false), Some(4..5));
    }

    #[test]
    fn refresh_keeps_the_cursor_by_index() {
        let mut search = Search {
            query: "x".to_owned(),
            ..Search::default()
        };
        search.refresh("x 1 x 2 x");
        search.step(true); // first x
        search.step(true); // second x
        assert_eq!(search.current_match(), Some(4..5));
        // Text before the cursor grows, so every match shifts right, but the
        // cursor still names the second occurrence. The prefix must not itself
        // contain the query, or the count would move too.
        search.refresh(">> x 1 x 2 x");
        assert_eq!(search.current_match(), Some(7..8));
        assert_eq!(search.position(), Some(2));
    }

    #[test]
    fn refresh_drops_a_cursor_the_text_no_longer_has() {
        let mut search = Search {
            query: "x".to_owned(),
            ..Search::default()
        };
        search.refresh("x x x");
        search.step(true);
        search.step(true);
        search.step(true); // third
        search.refresh("x");
        assert_eq!(search.current_match(), None);
        assert_eq!(search.len(), 1);
    }

    #[test]
    fn changing_the_query_drops_a_stale_cursor() {
        let mut search = Search {
            query: "abc".to_owned(),
            ..Search::default()
        };
        search.refresh("abc abc");
        search.step(true);
        search.step(true);
        search.query = "zzz".to_owned();
        search.refresh("abc abc");
        assert!(search.is_empty());
        assert_eq!(search.current_match(), None);
        assert_eq!(search.position(), None);
    }
}
