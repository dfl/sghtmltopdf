//! `--dump-outline` XML assembly.
//!
//! The output uses the same structure and namespace as wkhtmltopdf's `--dump-outline`
//! (`dumpOutline` in `src/lib/pdf.cc`). `<item>` elements are nested according to the
//! relative heading levels, and each item carries `title`, `page`, and `link` attributes.
//! `page` is the 1-based physical page number counting the cover and TOC.
//!
//! ```xml
//! <?xml version="1.0" encoding="UTF-8"?>
//! <outline xmlns="http://code.google.com/p/wkhtmltopdf/outline">
//!   <item title="Introduction" page="1" link="#intro">
//!     <item title="Background" page="2" link="#__sgtoc_1"/>
//!   </item>
//! </outline>
//! ```

use std::fmt::Write as _;

use crate::engine::OutlineHeading;

/// Build the wkhtmltopdf-compatible outline XML.
pub fn build_outline_xml(headings: &[OutlineHeading]) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <outline xmlns=\"http://code.google.com/p/wkhtmltopdf/outline\">\n",
    );

    if headings.is_empty() {
        xml.push_str("</outline>\n");
        return xml;
    }

    // Stack of the levels of the currently open `<item>` elements. Same nesting
    // algorithm as `cli::toc::write_entries` (a jump in level counts as one step).
    let mut open_levels: Vec<u8> = Vec::new();

    for h in headings {
        while let Some(&top) = open_levels.last() {
            if h.level > top {
                break; // Deeper: write it as a child of the current item.
            }
            // Same or shallower: close the open item.
            close_item(&mut xml, open_levels.len());
            open_levels.pop();
        }

        open_item(&mut xml, h, open_levels.len());
        open_levels.push(h.level);
    }

    while !open_levels.is_empty() {
        close_item(&mut xml, open_levels.len());
        open_levels.pop();
    }

    xml.push_str("</outline>\n");
    xml
}

/// Write a single opening `<item ...>` tag (left unclosed, since it has children).
fn open_item(xml: &mut String, h: &OutlineHeading, depth: usize) {
    indent(xml, depth + 1);
    let _ = writeln!(
        xml,
        "<item title=\"{}\" page=\"{}\" link=\"#{}\">",
        escape_attr(&h.title),
        h.page,
        escape_attr(&h.anchor),
    );
}

fn close_item(xml: &mut String, depth: usize) {
    indent(xml, depth);
    xml.push_str("</item>\n");
}

fn indent(xml: &mut String, depth: usize) {
    for _ in 0..depth {
        xml.push_str("  ");
    }
}

/// Escape an XML attribute value. Since attributes are quoted with `"`, `"` is escaped too.
fn escape_attr(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn heading(level: u8, title: &str, page: usize) -> OutlineHeading {
        OutlineHeading {
            level,
            title: title.to_string(),
            page,
            anchor: format!("__sgtoc_{page}"),
        }
    }

    #[test]
    fn it_wraps_items_in_the_wkhtmltopdf_namespace() {
        let xml = build_outline_xml(&[heading(1, "Intro", 1)]);
        assert!(xml.starts_with("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(
            xml.contains(r#"<outline xmlns="http://code.google.com/p/wkhtmltopdf/outline">"#),
            "got: {xml}"
        );
        assert!(xml.trim_end().ends_with("</outline>"));
    }

    #[test]
    fn an_item_carries_title_page_and_link() {
        let xml = build_outline_xml(&[heading(1, "Intro", 3)]);
        assert!(
            // Contains `"#` (the link attribute), so the raw string is delimited with `r##`.
            xml.contains(r##"<item title="Intro" page="3" link="#__sgtoc_3">"##),
            "got: {xml}"
        );
        assert!(xml.contains("</item>"));
    }

    #[test]
    fn deeper_levels_nest_inside_the_parent_item() {
        let xml =
            build_outline_xml(&[heading(1, "A", 1), heading(2, "A-1", 2), heading(1, "B", 3)]);
        let a = xml.find(r#"title="A""#).unwrap();
        let a1 = xml.find(r#"title="A-1""#).unwrap();
        let a_close = xml[a..].find("</item>").unwrap() + a;
        let b = xml.find(r#"title="B""#).unwrap();
        // A-1 comes before A's closing tag (i.e. nested inside), B comes after A is closed.
        assert!(a < a1 && a1 < a_close, "A-1 must nest inside A: {xml}");
        assert!(a_close < b, "B must follow A's close: {xml}");
        // The number of opening and closing tags balances out.
        assert_eq!(
            xml.matches("<item ").count(),
            xml.matches("</item>").count()
        );
    }

    #[test]
    fn a_level_jump_counts_as_one_nesting_step() {
        // An h1 -> h3 jump nests just one level deeper, keeping the tags balanced.
        let xml = build_outline_xml(&[heading(1, "A", 1), heading(3, "A-x", 2)]);
        assert_eq!(xml.matches("<item ").count(), 2);
        assert_eq!(
            xml.matches("<item ").count(),
            xml.matches("</item>").count()
        );
    }

    #[test]
    fn special_characters_in_titles_are_escaped() {
        let xml = build_outline_xml(&[heading(1, "a<b>&\"c\"", 1)]);
        assert!(
            xml.contains(r#"title="a&lt;b&gt;&amp;&quot;c&quot;""#),
            "got: {xml}"
        );
    }

    #[test]
    fn no_headings_still_produces_a_valid_outline() {
        let xml = build_outline_xml(&[]);
        assert!(xml.contains("<outline"));
        assert!(xml.contains("</outline>"));
        assert!(!xml.contains("<item"));
    }
}
