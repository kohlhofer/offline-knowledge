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

    #[test]
    fn prefix_keeps_one_trailing_space() {
        assert_eq!(normalize_prefix("New "), "new ");
        assert_eq!(normalize_prefix("New"), "new");
        assert_eq!(normalize_prefix("   "), "");
    }
}
