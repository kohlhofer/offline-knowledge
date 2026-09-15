//! Text hygiene shared by every frontend that shows ZIM content: the TUI
//! (terminal escape sequences), the HTML renderer and MCP's plain-text
//! output (control bytes in general).

/// Removes control characters other than newlines, so article text cannot
/// send escape sequences to a terminal or inject control bytes into HTML or
/// MCP output.
pub fn sanitize(text: &str) -> String {
    text.chars().filter(|c| !c.is_control() || *c == '\n').collect()
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
}
