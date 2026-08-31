//! `linear-gradient()`背景を PDF の軸シェーディング(Type 2)として描く。
//!
//! 勾配データは`ComputedStyle`に載っているため、画像のようなフェッチ/デコードは
//! 不要で、ページ描画時に「使われている勾配のシェーディングオブジェクトを払い
//! 出して書く」だけで済む(画像の`collect_image_uses`と同じ構造)。
//!
//! 対応範囲(第1弾): 不透明な色経由点のみ。`transparent`やalpha付きの経由点を
//! 含む層、経由点が2つ未満の層、寸法0の要素は描かない(層ごと読み飛ばす)。
//! `radial-gradient`は[`crate::style`]のパース側で層ごと除外済み。

use pdf_writer::types::FunctionShadingType;
use pdf_writer::{Chunk, Ref};

use crate::layout::{PageSettings, Rect};
use crate::style::{ComputedStyle, LinearGradient};

/// 描画準備済みの1層。座標はデバイス空間(px, PDFのy向き=上)で、`sh`を出す
/// ときのクリップ矩形と同じ座標系。経由点は不透明・位置正規化済み。
pub struct GradientLayer {
    /// 軸の始点・終点`[x0, y0, x1, y1]`。
    pub coords: [f32; 4],
    /// `(位置 0..1, [r, g, b] 0..1)`。位置は昇順。
    pub stops: Vec<(f32, [f32; 3])>,
}

/// 1つの勾配層のシェーディングリソース名。同一ページ内で`NodeId`は一意
/// (opacity Form と同じ前提)なので、`NodeId`とCSS層番号で決まる名前は
/// 収集側と描画側で必ず一致する。
pub fn shading_name(node_index: usize, layer_index: usize) -> String {
    format!("Gsh{node_index}_{layer_index}")
}

/// `style`の勾配層を、`border_box`(要素の絶対px矩形)に合わせた描画準備済み
/// 層へ変換する。描けない層(alpha付き・経由点不足・寸法0)は除外するので、
/// 返るのは実際にシェーディングを出す層だけ。収集側・描画側の両方がこれを
/// 呼び、層数と順序を一致させる。
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
    gradient: &LinearGradient,
    border_box: Rect,
    settings: &PageSettings,
) -> Option<GradientLayer> {
    if gradient.stops.len() < 2 || border_box.width <= 0.0 || border_box.height <= 0.0 {
        return None;
    }
    // 不透明の経由点のみ対応(alpha付きはソフトマスクが要るため今は描かない)。
    if gradient.stops.iter().any(|s| s.color.alpha < 1.0) {
        return None;
    }

    let positions = normalized_positions(gradient);
    let stops: Vec<(f32, [f32; 3])> = gradient
        .stops
        .iter()
        .zip(positions)
        .map(|(s, pos)| {
            (
                pos,
                [
                    s.color.red as f32 / 255.0,
                    s.color.green as f32 / 255.0,
                    s.color.blue as f32 / 255.0,
                ],
            )
        })
        .collect();

    Some(GradientLayer {
        coords: axis_coords(gradient.angle_deg, border_box, settings),
        stops,
    })
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

    let to_device = |px: f32, py: f32| {
        (
            settings.margin.left + px,
            settings.size.height - settings.margin.top - py,
        )
    };
    let (x0, y0) = to_device(cx - dx * half_len, cy - dy * half_len);
    let (x1, y1) = to_device(cx + dx * half_len, cy + dy * half_len);
    [x0, y0, x1, y1]
}

/// 経由点の位置(0..1)を確定する。先頭省略は0、末尾省略は1、途中の省略は前後の
/// 確定位置から等間隔で補完し、逆行はひとつ前の位置へ丸める(CSSの規則)。
fn normalized_positions(gradient: &LinearGradient) -> Vec<f32> {
    let n = gradient.stops.len();
    let mut pos: Vec<Option<f32>> = gradient.stops.iter().map(|s| s.position).collect();
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

/// 1層分のPDFオブジェクト(色ランプの関数群+軸シェーディング)を書き、
/// シェーディングのRefと`(Ref, Chunk)`の列を返す。各オブジェクトは独立した
/// `Chunk`なので、バッチは`pdf.extend`、ストリーミングは`write_chunk`で書ける
/// (画像の`embed_image_streaming_chunks`と同じ形)。
pub fn write_layer_objects(
    layer: &GradientLayer,
    alloc: &mut dyn FnMut() -> Ref,
) -> (Ref, Vec<(Ref, Chunk)>) {
    let mut objects = Vec::new();

    // 隣り合う経由点ごとに指数補間関数(n=1 で線形)。
    let mut segment_refs = Vec::with_capacity(layer.stops.len() - 1);
    for pair in layer.stops.windows(2) {
        let id = alloc();
        let mut chunk = Chunk::new();
        {
            let mut f = chunk.exponential_function(id);
            f.domain([0.0, 1.0]);
            f.c0(pair[0].1);
            f.c1(pair[1].1);
            f.n(1.0);
        }
        objects.push((id, chunk));
        segment_refs.push(id);
    }

    // 経由点が2つなら関数は1つ。3つ以上なら stitching で繋ぐ。
    let color_function = if segment_refs.len() == 1 {
        segment_refs[0]
    } else {
        let id = alloc();
        let mut chunk = Chunk::new();
        {
            let mut st = chunk.stitching_function(id);
            st.domain([0.0, 1.0]);
            st.functions(segment_refs.iter().copied());
            // 内側の経由点の位置が区間境界。
            st.bounds(
                layer.stops[1..layer.stops.len() - 1]
                    .iter()
                    .map(|(pos, _)| *pos),
            );
            st.encode(segment_refs.iter().flat_map(|_| [0.0, 1.0]));
        }
        objects.push((id, chunk));
        id
    };

    let shading_ref = alloc();
    let mut chunk = Chunk::new();
    {
        let mut sh = chunk.function_shading(shading_ref);
        sh.shading_type(FunctionShadingType::Axial);
        sh.color_space().device_rgb();
        sh.coords(layer.coords);
        sh.function(color_function);
        // 軸の外側は端の色で塗り続ける。
        sh.extend([true, true]);
    }
    objects.push((shading_ref, chunk));

    (shading_ref, objects)
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

    fn gradient(angle_deg: f32, stops: Vec<GradientStop>) -> LinearGradient {
        LinearGradient { angle_deg, stops }
    }

    #[test]
    fn to_bottom_axis_runs_down_the_box_in_device_space() {
        // 180deg = to bottom。デバイス空間ではyが上向きなので、始点(0%)が上端
        // (大きいy)、終点(100%)が下端(小さいy)。
        let g = gradient(
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
        let g = gradient(
            90.0,
            vec![
                GradientStop {
                    color: opaque(0, 0, 0),
                    position: None,
                },
                GradientStop {
                    color: opaque(1, 1, 1),
                    position: None,
                },
                GradientStop {
                    color: opaque(2, 2, 2),
                    position: None,
                },
                GradientStop {
                    color: opaque(3, 3, 3),
                    position: None,
                },
            ],
        );
        let p = normalized_positions(&g);
        let expected = [0.0, 1.0 / 3.0, 2.0 / 3.0, 1.0];
        for (got, want) in p.iter().zip(expected) {
            assert!((got - want).abs() < 1e-6, "got {p:?}");
        }
    }

    #[test]
    fn alpha_stops_make_the_layer_unpaintable() {
        let g = gradient(
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
            background_gradients: vec![g],
            ..ComputedStyle::default()
        };
        let border_box = Rect {
            x: 0.0,
            y: 0.0,
            width: 10.0,
            height: 10.0,
        };
        assert!(layers_for(&style, border_box, &settings()).is_empty());
    }
}
