//! Text hygiene shared by every frontend that shows ZIM content: the TUI
//! (terminal escape sequences), the HTML renderer and MCP's plain-text
//! output (control bytes in general).

/// Removes control characters other than newlines, so article text cannot
/// send escape sequences to a terminal or inject control bytes into HTML or
/// MCP output. Also removes bidi override characters (they're `Cf`, format
/// characters, not `Cc`, so `is_control()` misses them) — left in, one can
/// make `"ac.txt"` display as `"atxt.exe"` in a terminal, an HTML page or an
/// agent's own text output.
pub fn sanitize(text: &str) -> String {
    text.chars().filter(|&c| keep_char(c)).collect()
}

/// `sanitize`, plus collapsing runs of whitespace (including the newlines
/// `sanitize` deliberately keeps) to a single space: for a field a caller
/// renders as one line — a title, a heading — where an article's own
/// `<br>`-turned-newline must not be able to fake a second line.
pub fn sanitize_line(text: &str) -> String {
    sanitize(text).split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Whether `sanitize` keeps `c`. Exposed so `html::esc_into` can sanitize
/// and escape in one pass over a string's characters, instead of building
/// a sanitized `String` first and then a separately-escaped one from it.
pub(crate) fn keep_char(c: char) -> bool {
    (!c.is_control() || c == '\n') && !is_bidi_override(c)
}

fn is_bidi_override(c: char) -> bool {
    matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_esc_bel_and_c1_controls_but_keeps_newlines() {
        assert_eq!(sanitize("a\u{1b}[2Jb"), "a[2Jb", "ESC starts a terminal escape sequence");
        assert_eq!(sanitize("a\u{7}b"), "ab", "BEL");
        assert_eq!(sanitize("a\u{9b}b"), "ab", "C1 control (CSI, 0x9B)");
        assert_eq!(sanitize("a\u{0}\u{1f}b"), "ab", "C0 controls other than newline");
        assert_eq!(sanitize("line one\nline two"), "line one\nline two", "newlines are kept");
        assert_eq!(sanitize(""), "");
    }

    #[test]
    fn strips_bidi_overrides() {
        assert_eq!(sanitize("a\u{202e}txt.exe"), "atxt.exe", "RLO (U+202E) can make a filename display reversed");
        assert_eq!(sanitize("a\u{2066}b\u{2069}c"), "abc", "isolate characters (U+2066/U+2069)");
    }

    #[test]
    fn sanitize_line_collapses_embedded_newlines_to_a_single_space() {
        assert_eq!(sanitize_line("Einstein\nSyndrome"), "Einstein Syndrome", "a <br>-turned-newline must not fake a second line");
        assert_eq!(sanitize_line("a\n\n\nb"), "a b", "runs of whitespace collapse to one space");
        assert_eq!(sanitize_line("  padded  "), "padded", "leading/trailing whitespace is trimmed too");
        assert_eq!(sanitize_line("plain title"), "plain title", "ordinary text is unaffected");
    }
}
