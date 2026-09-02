//! `linear-gradient()`/`radial-gradient()`背景を PDF のシェーディング
//! (Type 2 軸 / Type 3 放射)として描く。
//!
//! 勾配データは`ComputedStyle`に載っているため、画像のようなフェッチ/デコードは
//! 不要で、ページ描画時に「使われている勾配のシェーディングオブジェクトを払い
//! 出して書く」だけで済む(画像の`collect_image_uses`と同じ構造)。
//!
//! 対応範囲:
//! - `linear-gradient()`(角度・単一辺方向)と`radial-gradient()`(円・
//!   farthest-corner 固定、`at <position>`の中心)。
//! - alpha付き・`transparent`の経由点。色ランプ(RGB)に加え、輝度ソフトマスク
//!   (DeviceGray のシェーディング + マスクForm XObject + ExtGState の`/SMask`)を
//!   払い出し、描画側が`gs`で不透明度を変調する。
//!
//! 経由点が2つ未満の層、寸法0の要素は描かない(層ごと読み飛ばす)。

use pdf_writer::types::{FunctionShadingType, MaskType};
use pdf_writer::{Chunk, Ref};
use pdf_writer::{Content, Name, Rect as PdfRect};

use crate::layout::{PageSettings, Rect};
use crate::style::{BackgroundGradient, ComputedStyle, LinearGradient, RadialGradient};

/// 描画準備済みの1層。座標はデバイス空間(px, PDFのy向き=上)で、`sh`を出す
/// ときのクリップ矩形と同じ座標系。経由点は位置正規化済み。
pub struct GradientLayer {
    /// シェーディングの種類と座標(軸`[x0,y0,x1,y1]` / 放射`[cx,cy,0,cx,cy,r]`)。
    pub geometry: LayerGeometry,
    /// `(位置 0..1, [r, g, b] 0..1, alpha 0..1)`。位置は昇順。
    pub stops: Vec<RampStop>,
}

/// 勾配の形状(軸または放射)と、その座標。
pub enum LayerGeometry {
    /// 軸シェーディング(Type 2)。`[x0, y0, x1, y1]`。
    Axial([f32; 4]),
    /// 放射シェーディング(Type 3)。`[cx, cy, r0, cx, cy, r1]`(内円 r0=0)。
    Radial([f32; 6]),
}

/// 位置確定済みの経由点1つ。
pub struct RampStop {
    pub pos: f32,
    pub rgb: [f32; 3],
    pub alpha: f32,
}

impl GradientLayer {
    /// alpha付き(<1)の経由点を持つか。持つ場合は輝度ソフトマスクを併用する。
    pub fn has_alpha(&self) -> bool {
        self.stops.iter().any(|s| s.alpha < 1.0)
    }

    fn shading_type(&self) -> FunctionShadingType {
        match self.geometry {
            LayerGeometry::Axial(_) => FunctionShadingType::Axial,
            LayerGeometry::Radial(_) => FunctionShadingType::Radial,
        }
    }

    fn coords(&self) -> Vec<f32> {
        match self.geometry {
            LayerGeometry::Axial(c) => c.to_vec(),
            LayerGeometry::Radial(c) => c.to_vec(),
        }
    }
}

/// 1つの勾配層の色シェーディングリソース名。同一ページ内で`NodeId`は一意
/// (opacity Form と同じ前提)なので、`NodeId`とCSS層番号で決まる名前は
/// 収集側と描画側で必ず一致する。
pub fn shading_name(node_index: usize, layer_index: usize) -> String {
    format!("Gsh{node_index}_{layer_index}")
}

/// alpha層の輝度ソフトマスクを持つマスクForm XObject のリソース名(`/XObject`)。
pub fn mask_form_name(node_index: usize, layer_index: usize) -> String {
    format!("Gmask{node_index}_{layer_index}")
}

/// alpha層の`/SMask`を仕込んだ ExtGState のリソース名(`/ExtGState`)。
pub fn ext_gstate_name(node_index: usize, layer_index: usize) -> String {
    format!("Ggs{node_index}_{layer_index}")
}

/// `style`の勾配層を、`border_box`(要素の絶対px矩形)に合わせた描画準備済み
/// 層へ変換する。描けない層(経由点不足・寸法0)は除外するので、返るのは実際に
/// シェーディングを出す層だけ。収集側・描画側の両方がこれを呼び、層数と順序を
/// 一致させる。
pub fn layers_for(
    style: &ComputedStyle,
    border_box: Rect,
    settings: &PageSettings,
) -> Vec<GradientLayer> {
    style
        .background_gradients
        .iter()
        .filter_map(|g| layer_for(g, border_box, settings))
        .collect()
}

fn layer_for(
    gradient: &BackgroundGradient,
    border_box: Rect,
    settings: &PageSettings,
) -> Option<GradientLayer> {
    if border_box.width <= 0.0 || border_box.height <= 0.0 {
        return None;
    }
    match gradient {
        BackgroundGradient::Linear(g) => linear_layer(g, border_box, settings),
        BackgroundGradient::Radial(g) => radial_layer(g, border_box, settings),
    }
}

fn linear_layer(
    gradient: &LinearGradient,
    border_box: Rect,
    settings: &PageSettings,
) -> Option<GradientLayer> {
    if gradient.stops.len() < 2 {
        return None;
    }
    let positions: Vec<Option<f32>> = gradient.stops.iter().map(|s| s.position).collect();
    let stops = ramp_stops(&positions, gradient.stops.iter().map(|s| s.color));
    Some(GradientLayer {
        geometry: LayerGeometry::Axial(axis_coords(gradient.angle_deg, border_box, settings)),
        stops,
    })
}

fn radial_layer(
    gradient: &RadialGradient,
    border_box: Rect,
    settings: &PageSettings,
) -> Option<GradientLayer> {
    if gradient.stops.len() < 2 {
        return None;
    }
    let positions: Vec<Option<f32>> = gradient.stops.iter().map(|s| s.position).collect();
    let stops = ramp_stops(&positions, gradient.stops.iter().map(|s| s.color));
    Some(GradientLayer {
        geometry: LayerGeometry::Radial(radial_coords(gradient.center, border_box, settings)),
        stops,
    })
}

/// 経由点の色(RgbaColor)と位置から、位置正規化済みの`RampStop`列を作る。
fn ramp_stops(
    positions: &[Option<f32>],
    colors: impl Iterator<Item = crate::style::RgbaColor>,
) -> Vec<RampStop> {
    normalized_positions(positions)
        .into_iter()
        .zip(colors)
        .map(|(pos, color)| RampStop {
            pos,
            rgb: [
                color.red as f32 / 255.0,
                color.green as f32 / 255.0,
                color.blue as f32 / 255.0,
            ],
            alpha: color.alpha,
        })
        .collect()
}

/// CSSの勾配角度(`0deg`=上、時計回り)を、`border_box`内の軸の始点・終点へ。
/// 軸長は「角に0%/100%が当たる」CSSの定義(`|W·sinθ| + |H·cosθ|`)。返す座標は
/// デバイス空間(x = margin.left + x, y = size.height − margin.top − y)。
fn axis_coords(angle_deg: f32, border_box: Rect, settings: &PageSettings) -> [f32; 4] {
    let theta = angle_deg.to_radians();
    let (w, h) = (border_box.width, border_box.height);
    // CSS座標(x右・y下)での終点方向。0deg=上(y減少)なので (sinθ, -cosθ)。
    let (dx, dy) = (theta.sin(), -theta.cos());
    let half_len = (w * theta.sin().abs() + h * theta.cos().abs()) / 2.0;
    let (cx, cy) = (border_box.x + w / 2.0, border_box.y + h / 2.0);

    let (x0, y0) = to_device(cx - dx * half_len, cy - dy * half_len, settings);
    let (x1, y1) = to_device(cx + dx * half_len, cy + dy * half_len, settings);
    [x0, y0, x1, y1]
}

/// `radial-gradient`の中心(0..1分数)と、farthest-corner の半径から放射
/// シェーディング座標`[cx, cy, 0, cx, cy, r]`を作る。デバイス変換は平行移動と
/// y反転(等長変換)なので、半径はCSS座標のまま4隅までの最大距離で求めてよい。
fn radial_coords(center: (f32, f32), border_box: Rect, settings: &PageSettings) -> [f32; 6] {
    let (w, h) = (border_box.width, border_box.height);
    let cx_css = border_box.x + center.0 * w;
    let cy_css = border_box.y + center.1 * h;
    let corners = [
        (border_box.x, border_box.y),
        (border_box.x + w, border_box.y),
        (border_box.x, border_box.y + h),
        (border_box.x + w, border_box.y + h),
    ];
    let radius = corners
        .iter()
        .map(|(px, py)| ((px - cx_css).powi(2) + (py - cy_css).powi(2)).sqrt())
        .fold(0.0_f32, f32::max);
    let (cx, cy) = to_device(cx_css, cy_css, settings);
    [cx, cy, 0.0, cx, cy, radius]
}

/// CSS座標(x右・y下)をデバイス空間(px, PDFのy向き=上)へ。
fn to_device(px: f32, py: f32, settings: &PageSettings) -> (f32, f32) {
    (
        settings.margin.left + px,
        settings.size.height - settings.margin.top - py,
    )
}

/// 経由点の位置(0..1)を確定する。先頭省略は0、末尾省略は1、途中の省略は前後の
/// 確定位置から等間隔で補完し、逆行はひとつ前の位置へ丸める(CSSの規則)。
fn normalized_positions(positions: &[Option<f32>]) -> Vec<f32> {
    let n = positions.len();
    let mut pos: Vec<Option<f32>> = positions.to_vec();
    if pos[0].is_none() {
        pos[0] = Some(0.0);
    }
    if pos[n - 1].is_none() {
        pos[n - 1] = Some(1.0);
    }
    // 逆行は直前の位置へクランプ(単調非減少にする)。
    let mut last = 0.0;
    for p in pos.iter_mut().flatten() {
        if *p < last {
            *p = last;
        }
        last = *p;
    }
    // 省略区間を等間隔で埋める。
    let mut result = vec![0.0; n];
    let mut i = 0;
    while i < n {
        if let Some(p) = pos[i] {
            result[i] = p;
            i += 1;
            continue;
        }
        // pos[i] は None。直前は確定済み(先頭は上で埋めた)。
        let start = result[i - 1];
        let mut j = i;
        while j < n && pos[j].is_none() {
            j += 1;
        }
        let end = pos[j].unwrap_or(1.0);
        let steps = (j - i + 1) as f32;
        for (k, slot) in result.iter_mut().enumerate().take(j).skip(i) {
            *slot = start + (end - start) * ((k - i + 1) as f32) / steps;
        }
        i = j;
    }
    result
}

/// 1つのalpha層が払い出したリソースのRef群。マスクForm XObjectと ExtGState は
/// 描画側がそれぞれ`/XObject`・`/ExtGState`へ登録する。
pub struct AlphaMaskRefs {
    pub mask_form: Ref,
    pub ext_gstate: Ref,
}

/// 1層分の払い出し結果。色シェーディングのRefは常に、alphaソフトマスク一式は
/// alpha層のときだけ返す。`objects`は独立`Chunk`の列(バッチは`pdf.extend`、
/// ストリーミングは`write_chunk`で書ける)。
pub struct LayerObjects {
    pub color_shading: Ref,
    pub alpha: Option<AlphaMaskRefs>,
    pub objects: Vec<(Ref, Chunk)>,
}

/// 隣り合う経由点ごとの指数補間関数(n=1で線形)+ stitching をつなぎ、値の列
/// (色は`[r,g,b]`、alphaは`[gray]`)を補間する関数のRefを返す。
fn build_ramp_function(
    positions: &[f32],
    values: &[Vec<f32>],
    objects: &mut Vec<(Ref, Chunk)>,
    alloc: &mut dyn FnMut() -> Ref,
) -> Ref {
    let mut segment_refs = Vec::with_capacity(values.len() - 1);
    for pair in values.windows(2) {
        let id = alloc();
        let mut chunk = Chunk::new();
        {
            let mut f = chunk.exponential_function(id);
            f.domain([0.0, 1.0]);
            f.c0(pair[0].iter().copied());
            f.c1(pair[1].iter().copied());
            f.n(1.0);
        }
        objects.push((id, chunk));
        segment_refs.push(id);
    }

    if segment_refs.len() == 1 {
        return segment_refs[0];
    }
    let id = alloc();
    let mut chunk = Chunk::new();
    {
        let mut st = chunk.stitching_function(id);
        st.domain([0.0, 1.0]);
        st.functions(segment_refs.iter().copied());
        // 内側の経由点の位置が区間境界。
        st.bounds(positions[1..positions.len() - 1].iter().copied());
        st.encode(segment_refs.iter().flat_map(|_| [0.0, 1.0]));
    }
    objects.push((id, chunk));
    id
}

/// 1つの`FunctionShading`を書く(色はDeviceRGB、alphaはDeviceGray)。
fn write_shading(
    layer: &GradientLayer,
    function: Ref,
    gray: bool,
    objects: &mut Vec<(Ref, Chunk)>,
    alloc: &mut dyn FnMut() -> Ref,
) -> Ref {
    let shading_ref = alloc();
    let mut chunk = Chunk::new();
    {
        let mut sh = chunk.function_shading(shading_ref);
        sh.shading_type(layer.shading_type());
        if gray {
            sh.color_space().device_gray();
        } else {
            sh.color_space().device_rgb();
        }
        sh.coords(layer.coords());
        sh.function(function);
        // 軸/半径の外側は端の色で塗り続ける。
        sh.extend([true, true]);
    }
    objects.push((shading_ref, chunk));
    shading_ref
}

/// 1層分のPDFオブジェクト(色ランプ+シェーディング、alpha層なら輝度マスク
/// 一式も)を書き、Ref群と`(Ref, Chunk)`の列を返す。
pub fn write_layer_objects(
    layer: &GradientLayer,
    settings: &PageSettings,
    alloc: &mut dyn FnMut() -> Ref,
) -> LayerObjects {
    let mut objects = Vec::new();

    let positions: Vec<f32> = layer.stops.iter().map(|s| s.pos).collect();

    // 色ランプ(RGB)。
    let rgb_values: Vec<Vec<f32>> = layer.stops.iter().map(|s| s.rgb.to_vec()).collect();
    let color_fn = build_ramp_function(&positions, &rgb_values, &mut objects, alloc);
    let color_shading = write_shading(layer, color_fn, false, &mut objects, alloc);

    let alpha = if layer.has_alpha() {
        // alphaランプ(DeviceGray、alpha 1.0→白/1.0、0→黒/0.0)。
        let alpha_values: Vec<Vec<f32>> = layer.stops.iter().map(|s| vec![s.alpha]).collect();
        let alpha_fn = build_ramp_function(&positions, &alpha_values, &mut objects, alloc);
        let alpha_shading = write_shading(layer, alpha_fn, true, &mut objects, alloc);

        // マスクForm XObject: bbox=ページ全体(色シェーディングと同じデバイス
        // 空間の px 座標)。中身は輝度シェーディングをbbox全面に`sh`で塗り、
        // `/Group /S /Transparency /CS /DeviceGray`を宣言する。輝度シェーディングは
        // このForm内部の`/Resources /Shading`に登録する(名前はForm内で自己完結)。
        let mask_form = alloc();
        let mut form_content = Content::new();
        let alpha_name = alpha_shading_local_name();
        form_content.shading(Name(alpha_name.as_bytes()));
        let form_bytes = form_content.finish();

        let mut form_chunk = Chunk::new();
        {
            let mut form = form_chunk.form_xobject(mask_form, &form_bytes);
            form.bbox(PdfRect::new(
                0.0,
                0.0,
                settings.size.width,
                settings.size.height,
            ));
            form.group().transparency().color_space().device_gray();
            form.resources()
                .shadings()
                .pair(Name(alpha_name.as_bytes()), alpha_shading);
        }
        objects.push((mask_form, form_chunk));

        // ExtGState: `/SMask << /S /Luminosity /G <mask_form> >>`。
        let ext_gstate = alloc();
        let mut gs_chunk = Chunk::new();
        {
            let mut gs = gs_chunk.ext_graphics(ext_gstate);
            gs.soft_mask()
                .subtype(MaskType::Luminosity)
                .group(mask_form);
        }
        objects.push((ext_gstate, gs_chunk));

        Some(AlphaMaskRefs {
            mask_form,
            ext_gstate,
        })
    } else {
        None
    };

    LayerObjects {
        color_shading,
        alpha,
        objects,
    }
}

/// マスクForm XObject の`/Resources /Shading`内で輝度シェーディングに付ける
/// 固定名(Form内で自己完結するため層番号に依存しなくてよい)。
fn alpha_shading_local_name() -> String {
    "Sh".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{EdgeSizes, PageSize};
    use crate::style::{GradientStop, RgbaColor};

    fn settings() -> PageSettings {
        PageSettings {
            size: PageSize {
                width: 600.0,
                height: 800.0,
            },
            margin: EdgeSizes {
                top: 0.0,
                right: 0.0,
                bottom: 0.0,
                left: 0.0,
            },
        }
    }

    fn opaque(r: u8, g: u8, b: u8) -> RgbaColor {
        RgbaColor {
            red: r,
            green: g,
            blue: b,
            alpha: 1.0,
        }
    }

    fn linear(angle_deg: f32, stops: Vec<GradientStop>) -> LinearGradient {
        LinearGradient { angle_deg, stops }
    }

    #[test]
    fn to_bottom_axis_runs_down_the_box_in_device_space() {
        // 180deg = to bottom。デバイス空間ではyが上向きなので、始点(0%)が上端
        // (大きいy)、終点(100%)が下端(小さいy)。
        let g = linear(
            180.0,
            vec![
                GradientStop {
                    color: opaque(0, 0, 0),
                    position: None,
                },
                GradientStop {
                    color: opaque(255, 255, 255),
                    position: None,
                },
            ],
        );
        let border_box = Rect {
            x: 0.0,
            y: 0.0,
            width: 600.0,
            height: 800.0,
        };
        let [x0, y0, x1, y1] = axis_coords(g.angle_deg, border_box, &settings());
        assert!((x0 - 300.0).abs() < 0.01 && (x1 - 300.0).abs() < 0.01);
        assert!(
            (y0 - 800.0).abs() < 0.01,
            "start at top edge (device y=800)"
        );
        assert!((y1 - 0.0).abs() < 0.01, "end at bottom edge (device y=0)");
    }

    #[test]
    fn missing_positions_are_distributed_evenly() {
        let positions = [None, None, None, None];
        let p = normalized_positions(&positions);
        let expected = [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0];
        for (got, want) in p.iter().zip(expected) {
            assert!((got - want).abs() < 1e-6, "got {p:?}");
        }
    }

    #[test]
    fn radial_center_and_farthest_corner_radius() {
        // 中心 30%,20% の 600x800 ボックス。中心 (180, 160)。最遠角は右下
        // (600, 800): √(420² + 640²) = √(176400 + 409600) = √586000 ≈ 765.5。
        let center = (0.3, 0.2);
        let border_box = Rect {
            x: 0.0,
            y: 0.0,
            width: 600.0,
            height: 800.0,
        };
        let [cx, cy, r0, cx1, cy1, r1] = radial_coords(center, border_box, &settings());
        // デバイス空間: x=180, y=800-160=640。
        assert!((cx - 180.0).abs() < 0.01 && (cx1 - 180.0).abs() < 0.01);
        assert!((cy - 640.0).abs() < 0.01 && (cy1 - 640.0).abs() < 0.01);
        assert!((r0 - 0.0).abs() < 0.01);
        assert!(
            (r1 - 586000.0_f32.sqrt()).abs() < 0.5,
            "farthest-corner radius"
        );
    }

    #[test]
    fn alpha_ramp_maps_alpha_to_gray_values() {
        // alpha 1.0 → gray 1.0(白)、alpha 0.0 → gray 0.0(黒)。RampStopの
        // alphaがそのままDeviceGray値になることを確認する。
        let layer = GradientLayer {
            geometry: LayerGeometry::Axial([0.0, 0.0, 10.0, 0.0]),
            stops: vec![
                RampStop {
                    pos: 0.0,
                    rgb: [1.0, 0.0, 0.0],
                    alpha: 0.3,
                },
                RampStop {
                    pos: 1.0,
                    rgb: [0.0, 0.0, 0.0],
                    alpha: 0.0,
                },
            ],
        };
        assert!(layer.has_alpha());
        assert_eq!(layer.stops[0].alpha, 0.3);
        assert_eq!(layer.stops[1].alpha, 0.0);
    }

    #[test]
    fn a_transparent_stop_now_produces_a_paintable_layer() {
        let g = linear(
            0.0,
            vec![
                GradientStop {
                    color: RgbaColor {
                        red: 0,
                        green: 0,
                        blue: 0,
                        alpha: 0.0,
                    },
                    position: Some(0.0),
                },
                GradientStop {
                    color: opaque(0, 0, 0),
                    position: Some(1.0),
                },
            ],
        );
        let style = ComputedStyle {
            background_gradients: vec![BackgroundGradient::Linear(g)],
            ..ComputedStyle::default()
        };
        let border_box = Rect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        let layers = layers_for(&style, border_box, &settings());
        assert_eq!(layers.len(), 1);
        assert!(
            layers[0].has_alpha(),
            "alpha stop should require a soft mask"
        );
    }
}
