//! A file's source and its comment-stripped text, byte for byte aligned
//! (W7-19a's Express text, shared with the Python frameworks in W7-19b,
//! with Spring in W7-19c, with Laravel in W7-19d, with Rails in W7-19e,
//! with ASP.NET in W7-19f, with Vapor in W7-19g, with Rust's frameworks in
//! W7-19h and with Go in W7-19i).
//!
//! Upstream strips comments one space per UTF-16 code unit and measures
//! columns, match lengths and windows in code units; the strippers here
//! blank a comment one space per byte, so an offset into the stripped text
//! indexes the source too, and whatever upstream measures is measured on
//! the source at the same offsets.

use std::cell::OnceCell;

use crate::strip_comments::{
    strip_go_comments_keeping_offsets, strip_java_comments_keeping_offsets,
    strip_js_comments_keeping_offsets, strip_php_comments_keeping_offsets,
    strip_python_comments_keeping_offsets, strip_ruby_comments_keeping_offsets,
    strip_rust_comments_keeping_offsets,
};

/// A file's source and its comment-stripped text, byte for byte aligned.
pub(crate) struct OffsetText<'a> {
    pub(crate) content: &'a str,
    pub(crate) safe: String,
    /// The offsets of the stripped text's `\n`s, built on first use.
    newlines: OnceCell<Vec<usize>>,
}

impl<'a> OffsetText<'a> {
    /// `content` and `safe`, its comment-stripped text blanked byte for
    /// byte.
    pub(crate) fn new(content: &'a str, safe: String) -> Self {
        debug_assert_eq!(content.len(), safe.len());
        Self {
            content,
            safe,
            newlines: OnceCell::new(),
        }
    }

    /// JavaScript or TypeScript source ([`strip_js_comments_keeping_offsets`]).
    pub(crate) fn js(content: &'a str) -> Self {
        Self::new(content, strip_js_comments_keeping_offsets(content))
    }

    /// Python source ([`strip_python_comments_keeping_offsets`]).
    pub(crate) fn python(content: &'a str) -> Self {
        Self::new(content, strip_python_comments_keeping_offsets(content))
    }

    /// Java or Kotlin source ([`strip_java_comments_keeping_offsets`]).
    pub(crate) fn java(content: &'a str) -> Self {
        Self::new(content, strip_java_comments_keeping_offsets(content))
    }

    /// C# source: upstream strips it as it strips Java
    /// ([`strip_java_comments_keeping_offsets`]).
    pub(crate) fn csharp(content: &'a str) -> Self {
        Self::java(content)
    }

    /// Swift source: upstream strips it as it strips Java
    /// ([`strip_java_comments_keeping_offsets`]).
    pub(crate) fn swift(content: &'a str) -> Self {
        Self::java(content)
    }

    /// PHP source ([`strip_php_comments_keeping_offsets`]).
    pub(crate) fn php(content: &'a str) -> Self {
        Self::new(content, strip_php_comments_keeping_offsets(content))
    }

    /// Ruby source ([`strip_ruby_comments_keeping_offsets`]).
    pub(crate) fn ruby(content: &'a str) -> Self {
        Self::new(content, strip_ruby_comments_keeping_offsets(content))
    }

    /// Rust source ([`strip_rust_comments_keeping_offsets`]).
    pub(crate) fn rust(content: &'a str) -> Self {
        Self::new(content, strip_rust_comments_keeping_offsets(content))
    }

    /// Go source ([`strip_go_comments_keeping_offsets`]).
    pub(crate) fn go(content: &'a str) -> Self {
        Self::new(content, strip_go_comments_keeping_offsets(content))
    }

    /// `start..end` of the stripped text as upstream's holds it: a blanked
    /// character is a space per UTF-16 code unit there, a byte here.
    pub(crate) fn upstream(&self, start: usize, end: usize) -> String {
        let mut out = String::with_capacity(end - start);
        for (offset, ch) in self.content[start..end].char_indices() {
            match self.piece(start + offset, ch) {
                Some(blank) => out.extend(std::iter::repeat_n(blank, ch.len_utf16())),
                None => out.push(ch),
            }
        }
        out
    }

    /// Upstream's UTF-16 length of the stripped text in `start..end`, which
    /// is the source's there.
    pub(crate) fn units(&self, start: usize, end: usize) -> i64 {
        self.content[start..end].encode_utf16().count() as i64
    }

    /// The offset `units` UTF-16 code units of the source past `start`
    /// (upstream's `start + units`), or the text's end when it is nearer; a
    /// character the bound cuts in two ends the text before it.
    pub(crate) fn offset_after_units(&self, start: usize, units: usize) -> usize {
        let mut taken = 0;
        for (offset, ch) in self.content[start..].char_indices() {
            taken += ch.len_utf16();
            if taken > units {
                return start + offset;
            }
        }
        self.content.len()
    }

    /// The 1-based line of `offset` (upstream `split('\n').length`).
    pub(crate) fn line_at(&self, offset: usize) -> i64 {
        let newlines = self.newlines.get_or_init(|| {
            self.safe
                .bytes()
                .enumerate()
                .filter(|&(_, byte)| byte == b'\n')
                .map(|(at, _)| at)
                .collect()
        });
        newlines.partition_point(|&at| at < offset) as i64 + 1
    }

    /// Upstream `safe.slice(Math.max(floor, end - units), end)` — `floor`
    /// is where the text upstream slices starts — as upstream's stripped
    /// text holds it: a blanked character is a space per code unit there.
    /// A unit cut from its pair is U+FFFD, which every pattern here reads as
    /// it reads the lone surrogate: no word character, no space.
    pub(crate) fn units_before(&self, floor: usize, end: usize, units: usize) -> String {
        let mut reversed: Vec<char> = Vec::new();
        let mut taken = 0;
        for (offset, ch) in self.content[floor..end].char_indices().rev() {
            if taken == units {
                break;
            }
            let width = ch.len_utf16();
            let piece = self.piece(floor + offset, ch);
            if taken + width > units {
                reversed.push(piece.map_or('\u{FFFD}', |_| ' '));
                break;
            }
            taken += width;
            match piece {
                Some(blank) => reversed.extend(std::iter::repeat_n(blank, width)),
                None => reversed.push(ch),
            }
        }
        reversed.into_iter().rev().collect()
    }

    /// Upstream `safe.slice(start, start + units)`, as [`Self::units_before`]
    /// reads it.
    pub(crate) fn units_after(&self, start: usize, units: usize) -> String {
        let mut out = String::new();
        let mut taken = 0;
        for (offset, ch) in self.content[start..].char_indices() {
            if taken == units {
                break;
            }
            let width = ch.len_utf16();
            let piece = self.piece(start + offset, ch);
            if taken + width > units {
                out.push(piece.map_or('\u{FFFD}', |_| ' '));
                break;
            }
            taken += width;
            match piece {
                Some(blank) => out.extend(std::iter::repeat_n(blank, width)),
                None => out.push(ch),
            }
        }
        out
    }

    /// What the stripped text holds for the source character `ch` at `at`:
    /// `Some(byte)` for an ASCII character (the comment stripper may have
    /// blanked it) and for a blanked one (a space per code unit), `None`
    /// for one it kept.
    pub(crate) fn piece(&self, at: usize, ch: char) -> Option<char> {
        let byte = self.safe.as_bytes()[at];
        if ch.is_ascii() || byte == b' ' {
            Some(byte as char)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bound counts UTF-16 code units: an emoji is two and four bytes,
    /// `é` one and two; a character the bound cuts in two is left out.
    #[test]
    fn an_offset_after_units_counts_code_units_from_its_start() {
        let content = "ab\u{1F600}\u{e9}c";
        let text = OffsetText::new(content, content.to_string());
        assert_eq!(text.offset_after_units(0, 0), 0);
        assert_eq!(text.offset_after_units(0, 2), 2);
        // Three units would cut the emoji.
        assert_eq!(text.offset_after_units(0, 3), 2);
        assert_eq!(text.offset_after_units(0, 4), 6);
        assert_eq!(text.offset_after_units(0, 5), 8);
        assert_eq!(text.offset_after_units(2, 3), 8);
        assert_eq!(text.offset_after_units(6, 1), 8);
        // Past the end: the end.
        assert_eq!(text.offset_after_units(0, 500), content.len());
        assert_eq!(text.offset_after_units(content.len(), 5), content.len());
    }
}
