//! The outline overlay: every section of the article, filtered as you type.

use ok_core::normalize::normalize;

use super::layout::SectionSpot;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Outline {
    pub query: String,
    /// Indices into the article's sections that match, in document order.
    pub matches: Vec<usize>,
    /// Position within `matches`.
    pub selected: usize,
    /// The section the reader was in when the outline opened.
    pub current: usize,
}

impl Outline {
    pub fn open(sections: &[SectionSpot], scroll: usize) -> Outline {
        let current = sections.iter().rposition(|s| s.line <= scroll).unwrap_or(0);
        let mut outline = Outline { current, ..Outline::default() };
        outline.refilter(sections);
        outline
    }

    pub fn push(&mut self, c: char, sections: &[SectionSpot]) {
        self.query.push(c);
        self.refilter(sections);
    }

    pub fn pop(&mut self, sections: &[SectionSpot]) {
        self.query.pop();
        self.refilter(sections);
    }

    pub fn clear(&mut self, sections: &[SectionSpot]) {
        self.query.clear();
        self.refilter(sections);
    }

    pub fn move_by(&mut self, delta: isize) {
        if self.matches.is_empty() {
            return;
        }
        let last = self.matches.len() - 1;
        self.selected = self.selected.saturating_add_signed(delta).min(last);
    }

    pub fn select_last(&mut self) {
        self.selected = self.matches.len().saturating_sub(1);
    }

    pub fn selected_section(&self) -> Option<usize> {
        self.matches.get(self.selected).copied()
    }

    /// Recomputes matches. With no query every section matches and the
    /// reader's current section is selected; otherwise the first section whose
    /// heading has a word starting with the query is, or failing that the first match.
    fn refilter(&mut self, sections: &[SectionSpot]) {
        let words: Vec<String> = normalize(&self.query).split(' ').filter(|w| !w.is_empty()).map(str::to_string).collect();
        if words.is_empty() {
            self.matches = (0..sections.len()).collect();
            self.selected = self.current.min(self.matches.len().saturating_sub(1));
            return;
        }
        let mut prefix_hit = None;
        self.matches.clear();
        for (i, section) in sections.iter().enumerate() {
            let heading = normalize(&section.heading);
            if words.iter().all(|w| heading.contains(w.as_str())) {
                if prefix_hit.is_none() && heading.split(' ').any(|h| h.starts_with(words[0].as_str())) {
                    prefix_hit = Some(self.matches.len());
                }
                self.matches.push(i);
            }
        }
        self.selected = prefix_hit.unwrap_or(0);
    }
}

/// Headings of the sections that contain section `index`, outermost first,
/// leaving out the article title.
pub fn ancestors(sections: &[SectionSpot], index: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut level = sections[index].level;
    for s in sections[..index].iter().rev() {
        if s.level < level && s.level > 1 {
            out.push(s.heading.as_str());
            level = s.level;
        }
    }
    out.reverse();
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sections() -> Vec<SectionSpot> {
        [
            (1, "Albert Einstein"),
            (2, "Life and career"),
            (3, "Childhood, youth and education"),
            (3, "Marriages, relationships and children"),
            (2, "Scientific career"),
            (3, "Early life"),
            (2, "Personal views"),
            (3, "Political views"),
            (3, "Religious and philosophical views"),
            (2, "Legacy"),
            (2, "Café Einstein"),
        ]
        .iter()
        .enumerate()
        .map(|(i, &(level, heading))| SectionSpot { line: i * 10, level, heading: heading.into(), anchor: None })
        .collect()
    }

    fn headings(o: &Outline, s: &[SectionSpot]) -> Vec<String> {
        o.matches.iter().map(|&i| s[i].heading.clone()).collect()
    }

    #[test]
    fn opens_on_the_current_section_with_everything_listed() {
        let s = sections();
        let o = Outline::open(&s, 55);
        assert_eq!(o.matches.len(), s.len());
        assert_eq!(s[o.selected_section().unwrap()].heading, "Early life");
    }

    #[test]
    fn filters_by_every_word_ignoring_case_and_accents() {
        let s = sections();
        let mut o = Outline::open(&s, 0);
        for c in "VIEWS".chars() {
            o.push(c, &s);
        }
        assert_eq!(headings(&o, &s), ["Personal views", "Political views", "Religious and philosophical views"]);
        o.clear(&s);
        for c in "rel view".chars() {
            o.push(c, &s);
        }
        assert_eq!(headings(&o, &s), ["Religious and philosophical views"]);
        o.clear(&s);
        for c in "cafe".chars() {
            o.push(c, &s);
        }
        assert_eq!(headings(&o, &s), ["Café Einstein"]);
    }

    #[test]
    fn prefers_a_word_that_starts_with_the_query() {
        let s = sections();
        let mut o = Outline::open(&s, 0);
        for c in "car".chars() {
            o.push(c, &s);
        }
        // "Life and career" comes first in the document and starts a word with "car".
        assert_eq!(headings(&o, &s), ["Life and career", "Scientific career"]);
        assert_eq!(o.selected, 0);
        o.clear(&s);
        for c in "ear".chars() {
            o.push(c, &s);
        }
        // "career" contains "ear" but only "Early life" starts a word with it.
        assert_eq!(s[o.selected_section().unwrap()].heading, "Early life");
    }

    #[test]
    fn editing_moving_and_empty_results() {
        let s = sections();
        let mut o = Outline::open(&s, 0);
        o.push('z', &s);
        o.push('z', &s);
        assert!(o.matches.is_empty());
        assert_eq!(o.selected_section(), None);
        o.move_by(1);
        o.pop(&s);
        o.pop(&s);
        assert_eq!(o.matches.len(), s.len());
        o.move_by(-5);
        assert_eq!(o.selected, 0);
        o.move_by(100);
        assert_eq!(o.selected, s.len() - 1);
    }

    #[test]
    fn ancestors_skip_the_title() {
        let s = sections();
        assert_eq!(ancestors(&s, 8), ["Personal views"]);
        assert!(ancestors(&s, 1).is_empty());
        assert!(ancestors(&s, 0).is_empty());
    }
}
