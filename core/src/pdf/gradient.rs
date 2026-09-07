//! Draw `linear-gradient()`/`radial-gradient()` backgrounds as PDF shadings
//! (Type 2 axial / Type 3 radial).
//!
//! The gradient data already lives on `ComputedStyle`, so there is no fetch/decode
//! step like there is for images; at page-draw time it is enough to "emit and write
//! the shading objects for the gradients that are actually used" (the same structure
//! as `collect_image_uses` for images).
//!
//! Supported range:
//! - `linear-gradient()` (angle / single-side direction) and `radial-gradient()`
//!   (circle, fixed farthest-corner, `at <position>` center).
//! - Stops with alpha and `transparent`. In addition to the color ramp (RGB), a
//!   luminosity soft mask (a DeviceGray shading + a mask Form XObject + an ExtGState
//!   `/SMask`) is emitted, and the draw side modulates opacity with `gs`.
//!
//! Layers with fewer than 2 stops, and zero-sized elements, are not drawn (the whole
//! layer is skipped).

use pdf_writer::types::{FunctionShadingType, MaskType};
use pdf_writer::{Chunk, Ref};
use pdf_writer::{Content, Name, Rect as PdfRect};

use crate::layout::{PageSettings, Rect};
use crate::style::{BackgroundGradient, ComputedStyle, LinearGradient, RadialGradient};

/// A single draw-ready layer. Coordinates are in device space (px, PDF y-up), the
/// same coordinate system as the clip rect used when emitting `sh`. Stop positions are
/// already normalized.
pub struct GradientLayer {
    /// The shading kind and its coordinates (axial `[x0,y0,x1,y1]` / radial `[cx,cy,0,cx,cy,r]`).
    pub geometry: LayerGeometry,
    /// `(position 0..1, [r, g, b] 0..1, alpha 0..1)`. Positions are ascending.
    pub stops: Vec<RampStop>,
}

/// The gradient shape (axial or radial) and its coordinates.
pub enum LayerGeometry {
    /// Axial shading (Type 2). `[x0, y0, x1, y1]`.
    Axial([f32; 4]),
    /// Radial shading (Type 3). `[cx, cy, r0, cx, cy, r1]` (inner circle r0=0).
    Radial([f32; 6]),
}

/// A single stop with its position already resolved.
pub struct RampStop {
    pub pos: f32,
    pub rgb: [f32; 3],
    pub alpha: f32,
}

impl GradientLayer {
    /// Whether it has any stop with alpha (< 1). If so, a luminosity soft mask is used
    /// alongside.
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

/// Resource name of the color shading for one gradient layer. Within a single page a
/// `NodeId` is unique (the same assumption as the opacity Form), so a name derived from
/// the `NodeId` and CSS layer index is guaranteed to match between the collect side and
/// the draw side.
pub fn shading_name(node_index: usize, layer_index: usize) -> String {
    format!("Gsh{node_index}_{layer_index}")
}

/// Resource name (`/XObject`) of the mask Form XObject that holds an alpha layer's
/// luminosity soft mask.
pub fn mask_form_name(node_index: usize, layer_index: usize) -> String {
    format!("Gmask{node_index}_{layer_index}")
}

/// Resource name (`/ExtGState`) of the ExtGState that carries an alpha layer's `/SMask`.
pub fn ext_gstate_name(node_index: usize, layer_index: usize) -> String {
    format!("Ggs{node_index}_{layer_index}")
}

/// Convert the gradient layers of `style` into draw-ready layers fitted to `border_box`
/// (the element's absolute px rectangle). Layers that cannot be drawn (too few stops,
/// zero size) are excluded, so what is returned is only the layers that actually emit a
/// shading. Both the collect side and the draw side call this, keeping the layer count
/// and order in sync.
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

/// Build a position-normalized `RampStop` list from the stop colors (RgbaColor) and
/// positions.
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

/// Turn a CSS gradient angle (`0deg` = up, clockwise) into the axis's start and end
/// points within `border_box`. The axis length follows the CSS definition where 0%/100%
/// land on the corners (`|W·sinθ| + |H·cosθ|`). The returned coordinates are in device
/// space (x = margin.left + x, y = size.height − margin.top − y).
fn axis_coords(angle_deg: f32, border_box: Rect, settings: &PageSettings) -> [f32; 4] {
    let theta = angle_deg.to_radians();
    let (w, h) = (border_box.width, border_box.height);
    // End-point direction in CSS coordinates (x right, y down). 0deg = up (y decreasing),
    // hence (sinθ, -cosθ).
    let (dx, dy) = (theta.sin(), -theta.cos());
    let half_len = (w * theta.sin().abs() + h * theta.cos().abs()) / 2.0;
    let (cx, cy) = (border_box.x + w / 2.0, border_box.y + h / 2.0);

    let (x0, y0) = to_device(cx - dx * half_len, cy - dy * half_len, settings);
    let (x1, y1) = to_device(cx + dx * half_len, cy + dy * half_len, settings);
    [x0, y0, x1, y1]
}

/// Build the radial shading coordinates `[cx, cy, 0, cx, cy, r]` from a `radial-gradient`
/// center (0..1 fraction) and the farthest-corner radius. The device transform is a
/// translation plus a y-flip (an isometry), so the radius can be computed as the maximum
/// distance to the four corners directly in CSS coordinates.
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

/// CSS coordinates (x right, y down) to device space (px, PDF y-up).
fn to_device(px: f32, py: f32, settings: &PageSettings) -> (f32, f32) {
    (
        settings.margin.left + px,
        settings.size.height - settings.margin.top - py,
    )
}

/// Resolve stop positions (0..1). A missing first position becomes 0, a missing last
/// becomes 1, missing positions in between are filled evenly from the surrounding
/// resolved positions, and a position that goes backwards is rounded up to the previous
/// one (the CSS rules).
fn normalized_positions(positions: &[Option<f32>]) -> Vec<f32> {
    let n = positions.len();
    let mut pos: Vec<Option<f32>> = positions.to_vec();
    if pos[0].is_none() {
        pos[0] = Some(0.0);
    }
    if pos[n - 1].is_none() {
        pos[n - 1] = Some(1.0);
    }
    // Clamp any backwards move to the previous position (make it monotonically
    // non-decreasing).
    let mut last = 0.0;
    for p in pos.iter_mut().flatten() {
        if *p < last {
            *p = last;
        }
        last = *p;
    }
    // Fill the missing runs at even spacing.
    let mut result = vec![0.0; n];
    let mut i = 0;
    while i < n {
        if let Some(p) = pos[i] {
            result[i] = p;
            i += 1;
            continue;
        }
        // pos[i] is None. The previous one is already resolved (the first was filled
        // above).
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

/// The Refs of the resources emitted for one alpha layer. The draw side registers the
/// mask Form XObject and the ExtGState under `/XObject` and `/ExtGState` respectively.
pub struct AlphaMaskRefs {
    pub mask_form: Ref,
    pub ext_gstate: Ref,
}

/// The emit result for one layer. The color shading Ref is always returned; the full
/// alpha soft-mask set only for an alpha layer. `objects` is a list of independent
/// `Chunk`s (writable with `pdf.extend` in batch mode, `write_chunk` in streaming mode).
pub struct LayerObjects {
    pub color_shading: Ref,
    pub alpha: Option<AlphaMaskRefs>,
    pub objects: Vec<(Ref, Chunk)>,
}

/// Chain an exponential interpolation function per adjacent stop pair (n=1 is linear)
/// plus a stitching function, and return the Ref of the function that interpolates the
/// value sequence (`[r,g,b]` for color, `[gray]` for alpha).
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
        // The inner stop positions are the segment boundaries.
        st.bounds(positions[1..positions.len() - 1].iter().copied());
        st.encode(segment_refs.iter().flat_map(|_| [0.0, 1.0]));
    }
    objects.push((id, chunk));
    id
}

/// Write one `FunctionShading` (DeviceRGB for color, DeviceGray for alpha).
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
        // Beyond the axis/radius, keep painting with the end color.
        sh.extend([true, true]);
    }
    objects.push((shading_ref, chunk));
    shading_ref
}

/// Write the PDF objects for one layer (the color ramp + shading, plus the full
/// luminosity mask set for an alpha layer) and return the Refs and the list of
/// `(Ref, Chunk)`.
pub fn write_layer_objects(
    layer: &GradientLayer,
    settings: &PageSettings,
    alloc: &mut dyn FnMut() -> Ref,
) -> LayerObjects {
    let mut objects = Vec::new();

    let positions: Vec<f32> = layer.stops.iter().map(|s| s.pos).collect();

    // Color ramp (RGB).
    let rgb_values: Vec<Vec<f32>> = layer.stops.iter().map(|s| s.rgb.to_vec()).collect();
    let color_fn = build_ramp_function(&positions, &rgb_values, &mut objects, alloc);
    let color_shading = write_shading(layer, color_fn, false, &mut objects, alloc);

    let alpha = if layer.has_alpha() {
        // Alpha ramp (DeviceGray; alpha 1.0 → white/1.0, 0 → black/0.0).
        let alpha_values: Vec<Vec<f32>> = layer.stops.iter().map(|s| vec![s.alpha]).collect();
        let alpha_fn = build_ramp_function(&positions, &alpha_values, &mut objects, alloc);
        let alpha_shading = write_shading(layer, alpha_fn, true, &mut objects, alloc);

        // Mask Form XObject: bbox = the whole page (the same device-space px coordinates
        // as the color shading). Its content paints the luminosity shading over the whole
        // bbox with `sh` and declares `/Group /S /Transparency /CS /DeviceGray`. The
        // luminosity shading is registered in this Form's own `/Resources /Shading` (the
        // name is self-contained within the Form).
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

/// The fixed name given to the luminosity shading within the mask Form XObject's
/// `/Resources /Shading` (since it is self-contained within the Form, it need not depend
/// on the layer index).
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
        // 180deg = to bottom. In device space y points up, so the start (0%) is at the
        // top edge (large y) and the end (100%) is at the bottom edge (small y).
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
        // A 600x800 box with center 30%,20%. Center (180, 160). The farthest corner is
        // bottom-right (600, 800): √(420² + 640²) = √(176400 + 409600) = √586000 ≈ 765.5.
        let center = (0.3, 0.2);
        let border_box = Rect {
            x: 0.0,
            y: 0.0,
            width: 600.0,
            height: 800.0,
        };
        let [cx, cy, r0, cx1, cy1, r1] = radial_coords(center, border_box, &settings());
        // Device space: x=180, y=800-160=640.
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
        // alpha 1.0 → gray 1.0 (white), alpha 0.0 → gray 0.0 (black). Confirms that a
        // RampStop's alpha becomes the DeviceGray value directly.
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
