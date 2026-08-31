//! `background-position`/`-size`/`-repeat`/`-attachment`と`background`
//! ショートハンドのE2Eテスト。
//!
//! `typography.rs`/`box_model.rs`と同じ方針: 実際のパイプライン(HTMLパース→
//! スタイルカスケード→背景画像デコード→ページ分割→PDFエンコード)を通して
//! 回帰を検知する。

use std::path::PathBuf;

use base64::engine::general_purpose::STANDARD;
use base64::Engine;

use sghtmltopdf_core::fonts::{Font, FontCollection};
use sghtmltopdf_core::html;
use sghtmltopdf_core::layout::{paginate_document, resolve_background_images, PageSettings};
use sghtmltopdf_core::pdf::{encode_pdf, ImageAssetCache};
use sghtmltopdf_core::style::{compute_styles, parse_stylesheet, user_agent_stylesheet};

const FONT_PATH: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fonts/DejaVuSans.ttf");
const PNG_PATH: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/images/spike_opaque.png"
);

fn test_fonts() -> FontCollection {
    FontCollection::new(vec![
        Font::load(FONT_PATH).expect("should load bundled test font")
    ])
}

/// `spike_opaque.png`(20x16、既存フィクスチャ)をdata URIへ
/// エンコードする。ネットワーク/ファイルI/Oに依存せず`ImageAssetCache`で
/// 実際にデコードされるパスを通すため。
fn png_data_uri() -> String {
    let bytes = std::fs::read(PNG_PATH).expect("fixture image should exist");
    format!("data:image/png;base64,{}", STANDARD.encode(bytes))
}

fn count_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|w| *w == needle)
        .count()
}

/// PDFのcontent streamはFlateDecodeで圧縮されているため、`Do`のような
/// コンテンツストリーム内演算子を検索するには解凍が必要
/// (`pdf::document`テストモジュール内の同名関数と同じロジック)。
fn decompressed_stream_bytes(pdf_bytes: &[u8]) -> Vec<u8> {
    fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        haystack.windows(needle.len()).position(|w| w == needle)
    }

    let mut out = Vec::new();
    let mut i = 0;
    while let Some(pos) = find_subslice(&pdf_bytes[i..], b"stream\n") {
        let start = i + pos + b"stream\n".len();
        let Some(end_rel) = find_subslice(&pdf_bytes[start..], b"\nendstream") else {
            break;
        };
        let end = start + end_rel;
        let raw = &pdf_bytes[start..end];

        let mut decoder = flate2::read::ZlibDecoder::new(raw);
        let mut decompressed = Vec::new();
        if std::io::Read::read_to_end(&mut decoder, &mut decompressed).is_ok() {
            out.extend_from_slice(&decompressed);
        } else {
            out.extend_from_slice(raw);
        }
        i = end + b"\nendstream".len();
    }
    out
}

/// HTML+CSSから、実際のパイプライン(パース→カスケード→背景画像デコード→
/// ページ分割→PDFエンコード)を一通り実行する。
fn build_pdf(html_src: &str, css: &str) -> Vec<u8> {
    let dom = html::parse(html_src.as_bytes());
    let ua = user_agent_stylesheet();
    let author = parse_stylesheet(css);
    let styles = compute_styles(&dom, &ua, &author);
    let fonts = test_fonts();
    let settings = PageSettings::default();

    // ネットワーク/ローカルファイルへは実際にはアクセスしない(data URIのみ
    // 使う)ため、base_dirは任意で構わない。
    let image_cache = ImageAssetCache::new(PathBuf::from("."), false);
    let background_images = resolve_background_images(&styles, &image_cache);

    let pages = paginate_document(&dom, &styles, &fonts, &settings);
    let bytes = encode_pdf(&pages, &styles, &background_images, &fonts, &settings);

    assert!(bytes.starts_with(b"%PDF-"));
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
    bytes
}

#[test]
fn background_shorthand_with_cover_and_no_repeat_draws_a_single_tile_end_to_end() {
    let css = format!(
        r#"body {{ margin: 0; }}
           .box {{
               width: 100px; height: 60px;
               background: url("{}") no-repeat center / cover;
           }}"#,
        png_data_uri()
    );
    let bytes = build_pdf(r#"<div class="box"></div>"#, &css);
    let decompressed = decompressed_stream_bytes(&bytes);
    assert_eq!(
        count_occurrences(&decompressed, b" Do\n"),
        1,
        "no-repeat should draw exactly one tile"
    );
}

#[test]
fn background_repeat_tiles_the_image_across_the_box_end_to_end() {
    // intrinsicサイズ(20x16)より大きいbox(100x60)へ`repeat`(既定値)を
    // 指定すると、水平5列(20刻み)×垂直4行(16刻み)=20タイル敷き詰められる。
    let css = format!(
        r#"body {{ margin: 0; }}
           .box {{
               width: 100px; height: 64px;
               background-image: url("{}");
           }}"#,
        png_data_uri()
    );
    let bytes = build_pdf(r#"<div class="box"></div>"#, &css);
    let decompressed = decompressed_stream_bytes(&bytes);
    assert_eq!(count_occurrences(&decompressed, b" Do\n"), 5 * 4);
}

#[test]
fn background_repeat_x_only_tiles_horizontally_end_to_end() {
    let css = format!(
        r#"body {{ margin: 0; }}
           .box {{
               width: 60px; height: 16px;
               background-image: url("{}");
               background-repeat: repeat-x;
           }}"#,
        png_data_uri()
    );
    let bytes = build_pdf(r#"<div class="box"></div>"#, &css);
    let decompressed = decompressed_stream_bytes(&bytes);
    // 幅60を20刻みで3列、縦は1行のみ(repeat-xなので垂直方向は敷き詰めない)。
    assert_eq!(count_occurrences(&decompressed, b" Do\n"), 3);
}

#[test]
fn background_size_percentage_and_position_percentage_render_a_valid_pdf_end_to_end() {
    let css = format!(
        r#"body {{ margin: 0; }}
           .box {{
               width: 200px; height: 100px;
               background-image: url("{}");
               background-repeat: no-repeat;
               background-size: 50% 50%;
               background-position: 100% 100%;
           }}"#,
        png_data_uri()
    );
    let bytes = build_pdf(r#"<div class="box"></div>"#, &css);
    let decompressed = decompressed_stream_bytes(&bytes);
    assert_eq!(count_occurrences(&decompressed, b" Do\n"), 1);
}

#[test]
fn background_attachment_fixed_still_renders_like_scroll_end_to_end() {
    // `fixed`は`scroll`と同一視するため、
    // クラッシュせず通常通り1枚描画されるはず。
    let css = format!(
        r#"body {{ margin: 0; }}
           .box {{
               width: 100px; height: 60px;
               background-image: url("{}");
               background-attachment: fixed;
               background-repeat: no-repeat;
           }}"#,
        png_data_uri()
    );
    let bytes = build_pdf(r#"<div class="box"></div>"#, &css);
    let decompressed = decompressed_stream_bytes(&bytes);
    assert_eq!(count_occurrences(&decompressed, b" Do\n"), 1);
}

#[test]
fn all_background_details_combined_render_a_valid_pdf_end_to_end() {
    let uri = png_data_uri();
    let html_src = r#"
        <div class="cover">cover</div>
        <div class="contain">contain</div>
        <div class="tiled">tiled</div>
        <div class="shorthand">shorthand</div>
        "#;
    let css = format!(
        r#"body {{ margin: 0; }}
           div {{ width: 100px; height: 60px; }}
           .cover {{ background-image: url("{uri}"); background-size: cover; background-repeat: no-repeat; }}
           .contain {{ background-image: url("{uri}"); background-size: contain; background-repeat: no-repeat; background-position: center; }}
           .tiled {{ background-image: url("{uri}"); background-repeat: repeat-y; }}
           .shorthand {{ background: url("{uri}") no-repeat right bottom / contain; }}
        "#
    );
    let bytes = build_pdf(html_src, &css);
    assert!(count_occurrences(&bytes, b"%%EOF") > 0);
}

#[test]
fn a_multi_stop_linear_gradient_emits_an_axial_shading_end_to_end() {
    // 3経由点 → 指数関数2つを stitching(Type 3)で繋いだ軸シェーディング(Type 2)。
    let css = r#"body { margin: 0; }
       .box { width: 200px; height: 100px;
              background: linear-gradient(90deg, #ff0000 0%, #00ff00 50%, #0000ff 100%); }"#;
    let bytes = build_pdf(r#"<div class="box"></div>"#, css);

    assert!(
        count_occurrences(&bytes, b"/ShadingType 2") > 0,
        "an axial shading object should be written"
    );
    assert!(
        count_occurrences(&bytes, b"/FunctionType 3") > 0,
        "3+ stops should stitch exponential functions"
    );
    // content stream 側では名前付きシェーディングを参照して塗る。
    let content = decompressed_stream_bytes(&bytes);
    assert!(
        count_occurrences(&content, b"/Gsh") > 0,
        "the content stream should reference the gradient shading resource"
    );
}

#[test]
fn an_unsupported_radial_layer_is_skipped_but_the_linear_base_still_paints() {
    // 複数背景: 未対応の radial-gradient は層ごと読み飛ばし、linear の基層だけ
    // シェーディングになる(宣言全体を捨てない)。
    let css = r#"body { margin: 0; }
       .box { width: 200px; height: 100px;
              background: radial-gradient(circle, #ffffff, #000000),
                          linear-gradient(0deg, #123456, #abcdef); }"#;
    let bytes = build_pdf(r#"<div class="box"></div>"#, css);
    assert_eq!(
        count_occurrences(&bytes, b"/ShadingType 2"),
        1,
        "only the linear base layer should emit a shading; the radial is skipped"
    );
}

#[test]
fn a_gradient_with_a_transparent_stop_is_not_painted() {
    // alpha 付き(`transparent`)の層は今は描かない。宣言は有効だがシェーディングは出ない。
    let css = r#"body { margin: 0; }
       .box { width: 200px; height: 100px;
              background: linear-gradient(90deg, #ff0000 0%, transparent 100%); }"#;
    let bytes = build_pdf(r#"<div class="box"></div>"#, css);
    assert_eq!(
        count_occurrences(&bytes, b"/ShadingType 2"),
        0,
        "a gradient with an alpha stop should be skipped for now"
    );
}

#[test]
fn background_clip_text_fills_glyphs_with_the_gradient() {
    // グラデーション文字: テキストをクリップ(Tr 7)に積み、`sh`で軸シェーディングを
    // グリフへ流し込む。矩形の背景塗り(fill)ではなくクリップ塗りになる。
    let css = r#"body { margin: 0; }
       h1 { font-size: 40px;
            background: linear-gradient(90deg, #ff0000, #0000ff);
            -webkit-background-clip: text; background-clip: text;
            -webkit-text-fill-color: transparent; color: transparent; }"#;
    let bytes = build_pdf(r#"<h1>GOLD</h1>"#, css);

    // 軸シェーディングは背景勾配と同じく書かれている。
    assert!(
        count_occurrences(&bytes, b"/ShadingType 2") > 0,
        "the gradient still emits an axial shading"
    );

    let content = decompressed_stream_bytes(&bytes);
    // テキストをクリップモード(7 Tr)で積む。
    assert!(
        count_occurrences(&content, b"7 Tr") > 0,
        "glyphs should be added to the clip path (text rendering mode 7): {}",
        String::from_utf8_lossy(&content)
    );
    // クリップへシェーディングを流し込む(名前付きシェーディング + sh)。
    assert!(
        count_occurrences(&content, b"/Gsh") > 0 && count_occurrences(&content, b" sh") > 0,
        "the gradient shading should be painted into the text clip"
    );
}
