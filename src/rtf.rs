//! A small RTF reader: pull the plain text (and, when present, the font name,
//! size, and color) back out of an OmniGraffle label blob.
//!
//! This is the inverse of the `rtf` writer in [`crate::graffle`], but it has to
//! cope with the richer RTF real OmniGraffle writes (`cocoatextscaling`,
//! `expandedcolortbl`, `\pard\pardeftab`, …), not just our own minimal output.
//! It is deliberately a forgiving scanner rather than a full RTF parser:
//! unknown control words are dropped, header/destination groups (`fonttbl`,
//! `colortbl`, `\*\…`) are skipped, and malformed input never panics — at worst
//! it yields empty text.

/// Text recovered from an RTF blob.
pub(crate) struct RtfText {
    pub(crate) text: String,
    /// First font-table entry, e.g. `Helvetica`.
    pub(crate) font: Option<String>,
    /// Point size from `\fsN` (RTF stores half-points).
    pub(crate) size: Option<f64>,
    /// `#rrggbb` resolved from `\cfN` against the color table (index 0 = auto).
    pub(crate) color: Option<String>,
}

/// Which kind of group we're currently inside — decides what to do with text.
#[derive(Clone, Copy, PartialEq)]
enum Group {
    /// Body text we keep.
    Body,
    /// `{\fonttbl …}` — harvest the font name, drop the rest.
    FontTbl,
    /// `{\colortbl …}` — harvest color entries, drop the rest.
    ColorTbl,
    /// A destination we don't care about (`\*\…`, `stylesheet`, …) — drop all.
    Skip,
}

pub(crate) fn strip(rtf: &str) -> RtfText {
    let chars: Vec<char> = rtf.chars().collect();
    let mut i = 0;

    let mut stack: Vec<Group> = vec![Group::Body];
    let mut text = String::new();
    let mut font: Option<String> = None;
    let mut font_buf = String::new();
    let mut size: Option<f64> = None;
    let mut cf: Option<usize> = None;
    let mut pending_high_surrogate: Option<u16> = None;
    // Color table; the leading `;` in `{\colortbl;…}` pushes index 0 (auto).
    let mut colortbl: Vec<(u8, u8, u8)> = Vec::new();
    let mut cur = (0u8, 0u8, 0u8);

    while i < chars.len() {
        let c = chars[i];
        match c {
            // A new group inherits its parent's kind until a destination control
            // word (below) reclassifies it.
            '{' => {
                stack.push(*stack.last().unwrap_or(&Group::Body));
                i += 1;
            }
            '}' => {
                // Keep a floor of one group so the stack is never empty —
                // malformed input with extra `}` can't then panic a later
                // `last_mut().unwrap()`.
                if stack.len() > 1 {
                    stack.pop();
                }
                i += 1;
            }
            '\\' => {
                if let Some(&next) = chars.get(i + 1) {
                    if next.is_ascii_alphabetic() {
                        // Control word: letters, optional signed number, optional
                        // single trailing space (a delimiter, not text).
                        let start = i + 1;
                        let mut j = start;
                        while j < chars.len() && chars[j].is_ascii_alphabetic() {
                            j += 1;
                        }
                        let word: String = chars[start..j].iter().collect();

                        let neg = chars.get(j) == Some(&'-');
                        let num_start = if neg { j + 1 } else { j };
                        let mut k = num_start;
                        while k < chars.len() && chars[k].is_ascii_digit() {
                            k += 1;
                        }
                        let param: Option<i64> = if k > num_start {
                            chars[num_start..k]
                                .iter()
                                .collect::<String>()
                                .parse::<i64>()
                                .ok()
                                .map(|v| if neg { -v } else { v })
                        } else {
                            None
                        };
                        let mut next_i = k;
                        if chars.get(next_i) == Some(&' ') {
                            next_i += 1;
                        }

                        let top = stack.last().copied().unwrap_or(Group::Body);
                        match word.as_str() {
                            "fonttbl" => *stack.last_mut().expect("stack has root group") = Group::FontTbl,
                            "colortbl" => *stack.last_mut().expect("stack has root group") = Group::ColorTbl,
                            "stylesheet" | "expandedcolortbl" | "info" | "generator" => {
                                *stack.last_mut().expect("stack has root group") = Group::Skip;
                            }
                            "red" => cur.0 = clamp_u8(param),
                            "green" => cur.1 = clamp_u8(param),
                            "blue" => cur.2 = clamp_u8(param),
                            "fs" if top == Group::Body => size = param.map(|p| p as f64 / 2.0),
                            "cf" if top == Group::Body => cf = param.map(|p| p.max(0) as usize),
                            "line" | "par" | "newline" if top == Group::Body => {
                                flush_pending_surrogate(&mut text, &mut pending_high_surrogate);
                                text.push('\n');
                            }
                            "tab" if top == Group::Body => {
                                flush_pending_surrogate(&mut text, &mut pending_high_surrogate);
                                text.push('\t');
                            }
                            "u" if top == Group::Body => {
                                if let Some(unit) = param.map(rtf_unicode_unit) {
                                    push_utf16_unit(&mut text, &mut pending_high_surrogate, unit);
                                }
                                // Skip the single fallback char that follows \uN.
                                if chars.get(next_i).is_some_and(|c| *c != '\\' && *c != '{' && *c != '}') {
                                    next_i += 1;
                                }
                            }
                            _ => {}
                        }
                        i = next_i;
                    } else {
                        // Control symbol: \\ \{ \} are literals; \* opens an
                        // ignored destination; \<newline> is a hard break.
                        let in_body = stack.last() == Some(&Group::Body);
                        match next {
                            '\\' | '{' | '}' if in_body => {
                                flush_pending_surrogate(&mut text, &mut pending_high_surrogate);
                                text.push(next);
                            }
                            '\n' | '\r' if in_body => {
                                flush_pending_surrogate(&mut text, &mut pending_high_surrogate);
                                text.push('\n');
                            }
                            '~' if in_body => {
                                flush_pending_surrogate(&mut text, &mut pending_high_surrogate);
                                text.push('\u{00A0}');
                            }
                            '*' => *stack.last_mut().expect("stack has root group") = Group::Skip,
                            _ => {}
                        }
                        i += 2;
                    }
                } else {
                    i += 1;
                }
            }
            // Raw newlines in the RTF source are formatting, not body text.
            '\n' | '\r' => i += 1,
            _ => {
                match stack.last().copied().unwrap_or(Group::Body) {
                    Group::Body => {
                        flush_pending_surrogate(&mut text, &mut pending_high_surrogate);
                        text.push(c);
                    }
                    Group::FontTbl => {
                        if c == ';' {
                            if font.is_none() && !font_buf.trim().is_empty() {
                                font = Some(font_buf.trim().to_string());
                            }
                            font_buf.clear();
                        } else {
                            font_buf.push(c);
                        }
                    }
                    Group::ColorTbl => {
                        if c == ';' {
                            colortbl.push(cur);
                            cur = (0, 0, 0);
                        }
                    }
                    Group::Skip => {}
                }
                i += 1;
            }
        }
    }

    flush_pending_surrogate(&mut text, &mut pending_high_surrogate);

    // `\cf0` is the "auto" color — treat it (and anything out of range) as none.
    let color = cf
        .filter(|&idx| idx >= 1)
        .and_then(|idx| colortbl.get(idx).copied())
        .map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}"));

    RtfText {
        text: text.trim().to_string(),
        font,
        size,
        color,
    }
}

fn clamp_u8(param: Option<i64>) -> u8 {
    param.unwrap_or(0).clamp(0, 255) as u8
}

fn rtf_unicode_unit(param: i64) -> u16 {
    i16::try_from(param)
        .map(|unit| unit as u16)
        .unwrap_or_else(|_| param.clamp(0, i64::from(u16::MAX)) as u16)
}

fn push_utf16_unit(text: &mut String, pending_high: &mut Option<u16>, unit: u16) {
    match unit {
        0xD800..=0xDBFF => {
            flush_pending_surrogate(text, pending_high);
            *pending_high = Some(unit);
        }
        0xDC00..=0xDFFF => match pending_high.take() {
            Some(high) => {
                if let Some(ch) = char::decode_utf16([high, unit]).next().and_then(Result::ok) {
                    text.push(ch);
                } else {
                    text.push(char::REPLACEMENT_CHARACTER);
                }
            }
            None => text.push(char::REPLACEMENT_CHARACTER),
        },
        _ => {
            flush_pending_surrogate(text, pending_high);
            if let Some(ch) = char::from_u32(u32::from(unit)) {
                text.push(ch);
            }
        }
    }
}

fn flush_pending_surrogate(text: &mut String, pending_high: &mut Option<u16>) {
    if pending_high.take().is_some() {
        text.push(char::REPLACEMENT_CHARACTER);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_minimal_blob_to_plaintext() {
        let r = strip(
            "{\\rtf1\\ansi\\ansicpg1252\\cocoartf2870\n{\\fonttbl\\f0\\fnil\\fcharset0 Helvetica;}\n\\f0\\fs28 \\cf0 GoLang}",
        );
        assert_eq!(r.text, "GoLang");
        assert_eq!(r.font.as_deref(), Some("Helvetica"));
        assert_eq!(r.size, Some(14.0));
        assert_eq!(r.color, None); // \cf0 == auto
    }

    #[test]
    fn extracts_font_size_and_color() {
        let r = strip(
            "{\\rtf1{\\fonttbl\\f0\\fnil\\fcharset0 AvenirNext-Regular;}{\\colortbl;\\red255\\green255\\blue255;\\red71\\green91\\blue98;}\\f0\\fs24 \\cf2 hi}",
        );
        assert_eq!(r.text, "hi");
        assert_eq!(r.font.as_deref(), Some("AvenirNext-Regular"));
        assert_eq!(r.size, Some(12.0));
        assert_eq!(r.color.as_deref(), Some("#475b62"));
    }

    #[test]
    fn recovers_line_breaks() {
        let line = strip("{\\rtf1\\f0\\fs24 \\cf0 a\\line b}");
        assert_eq!(line.text, "a\nb");
        let par = strip("{\\rtf1\\f0 a\\par b}");
        assert_eq!(par.text, "a\nb");
        // A backslash immediately before a real newline is a hard break.
        let hard = strip("{\\rtf1\\f0 a\\\nb}");
        assert_eq!(hard.text, "a\nb");
    }

    #[test]
    fn recovers_signed_unicode_escapes() {
        // RTF \uN stores a signed 16-bit code unit; values above 32767 are
        // written negative. U+E000 is -8192 as an i16.
        let r = strip("{\\rtf1\\f0 private \\u-8192? use}");
        assert_eq!(r.text, "private \u{e000} use");
    }

    #[test]
    fn recovers_unicode_surrogate_pairs() {
        // U+1F600 as signed UTF-16 code units: D83D DE00 -> -10179, -8704.
        let r = strip("{\\rtf1\\f0 smile \\u-10179?\\u-8704?}");
        assert_eq!(r.text, "smile \u{1f600}");
    }

    #[test]
    fn skips_expanded_colortbl_group() {
        let r = strip(
            "{\\rtf1{\\colortbl;\\red0\\green0\\blue0;}{\\*\\expandedcolortbl;;\\cssrgb\\c34702;}\\f0\\fs24 \\cf0 visible}",
        );
        assert_eq!(r.text, "visible");
    }

    #[test]
    fn escaped_braces_and_backslash_are_literal() {
        let r = strip("{\\rtf1\\f0 a\\{b\\}c\\\\d}");
        assert_eq!(r.text, "a{b}c\\d");
    }

    #[test]
    fn handles_empty_and_malformed_without_panic() {
        assert_eq!(strip("").text, "");
        assert_eq!(strip("{").text, "");
        assert_eq!(strip("no braces at all").text, "no braces at all");
        assert_eq!(strip("}}}{{{").text, "");
    }
}
