use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

/// The form titles are indexed and matched in: decomposed, accents stripped,
/// lowercased, underscores as spaces, whitespace collapsed and trimmed.
pub fn normalize(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.nfkd() {
        if is_combining_mark(c) {
            continue;
        }
        if c.is_whitespace() || c == '_' {
            space = !out.is_empty();
            continue;
        }
        // No title contains one, and left in, a control byte becomes part of
        // a key or of a lookup bound: a `\0` in a query collided with the
        // separator `titles.fst` puts between a key and its entry index.
        if c.is_control() {
            continue;
        }

        if space {
            out.push(' ');
            space = false;
        }
        out.extend(c.to_lowercase());
    }
    out
}

/// Normalizes a query typed as a prefix. A trailing space is kept, so
/// "new " matches "new york" and not "newton".
pub fn normalize_prefix(query: &str) -> String {
    let mut prefix = normalize(query);
    if !prefix.is_empty() && query.ends_with(|c: char| c.is_whitespace() || c == '_') {
        prefix.push(' ');
    }
    prefix
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_case_accents_and_spacing() {
        assert_eq!(normalize("  Henri_Poincaré  "), "henri poincare");
        assert_eq!(normalize("Mass–energy  equivalence"), "mass–energy equivalence");
        assert_eq!(normalize("ÉCOLE"), "ecole");
        assert_eq!(normalize("ﬁsh"), "fish");
        assert_eq!(normalize(""), "");
    }

    /// No title carries one, and left in, a control byte becomes part of an
    /// index key or of a lookup bound.
    #[test]
    fn drops_control_characters_but_still_treats_a_newline_as_a_space() {
        assert_eq!(normalize("Pacman\0"), "pacman");
        assert_eq!(normalize("Pac\u{1b}[2Jman"), "pac[2jman");
        assert_eq!(normalize("Einstein\nSyndrome"), "einstein syndrome");
    }

    #[test]
    fn prefix_keeps_one_trailing_space() {
        assert_eq!(normalize_prefix("New "), "new ");
        assert_eq!(normalize_prefix("New"), "new");
        assert_eq!(normalize_prefix("   "), "");
    }
}
