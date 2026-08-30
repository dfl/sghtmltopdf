//! `--dump-outline`のXML組み立て。
//!
//! 出力はwkhtmltopdfの`--dump-outline`と同じ構造・名前空間にする
//! (`src/lib/pdf.cc`の`dumpOutline`)。見出しレベルの相対関係で
//! `<item>`を入れ子にし、各項目は`title`・`page`・`link`属性を持つ。
//! `page`はcover・TOCを数えた1始まりの物理ページ番号。
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

/// wkhtmltopdf互換のアウトラインXMLを組み立てる。
pub fn build_outline_xml(headings: &[OutlineHeading]) -> String {
    let mut xml = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <outline xmlns=\"http://code.google.com/p/wkhtmltopdf/outline\">\n",
    );

    if headings.is_empty() {
        xml.push_str("</outline>\n");
        return xml;
    }

    // 現在開いている`<item>`のレベルを積む。`cli::toc::write_entries`と
    // 同じ入れ子アルゴリズム(レベルの飛びは1段として扱う)。
    let mut open_levels: Vec<u8> = Vec::new();

    for h in headings {
        while let Some(&top) = open_levels.last() {
            if h.level > top {
                break; // 深くなる: いまの項目の子として書く。
            }
            // 同じか浅い: 開いている項目を閉じる。
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

/// 開きタグ`<item ...>`を1つ書く(子があるので閉じない)。
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

/// XML属性値のエスケープ。`"`を含む属性で囲むため`"`も逃がす。
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
            // `"#`(link属性)を含むためraw stringは`r##`で囲む。
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
        // A-1 は A の閉じタグより前(=中に入れ子)、B は A を閉じた後。
        assert!(a < a1 && a1 < a_close, "A-1 must nest inside A: {xml}");
        assert!(a_close < b, "B must follow A's close: {xml}");
        // 開き/閉じの数が釣り合う。
        assert_eq!(
            xml.matches("<item ").count(),
            xml.matches("</item>").count()
        );
    }

    #[test]
    fn a_level_jump_counts_as_one_nesting_step() {
        // h1 -> h3 の飛びも1段だけ深くし、タグの釣り合いは崩さない。
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
