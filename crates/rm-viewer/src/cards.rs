//! The look of a Stage Manager card: its size, its tilt and the matrix that puts it there.
//! Ported from StageManagerForWindows (MIT, depoledna / Andreas Wäscher): `SceneModel.CardSizeDip`,
//! `SceneModel.EdgeTiltDegreesAt`, `CompositionThumbnail.SolveEdgeAngles` and `ComposeTransform`.
//! The constants are theirs.

const BASE_CARD_SCALE: f64 = 0.135693;
const MIN_CARD_HEIGHT_DIP: f64 = 96.0;
const EDGE_PERSPECTIVE_DISTANCE_RATIO: f64 = 1379.0 / 1169.0;

/// Vanishing distance of the perspective, fixed for every card.
pub const PERSPECTIVE_DEPTH_PX: f64 = 220.0;
/// Per side, how much larger than its card the visual is (so a tilted near edge is not clipped).
pub const HOVER_HEADROOM: f64 = 0.30;
/// Distance per index of the cascade of one scene's windows (IndexToOffsetMarginConverter).
pub const STACK_STEP_DIP: f64 = 10.0;
/// The sidebar's width and left padding (MainWindow.xaml: Width=240, Padding 22,0,0,0).
pub const SIDEBAR_WIDTH_DIP: f64 = 240.0;
pub const SIDEBAR_PAD_DIP: f64 = 22.0;
/// Space under each scene (MainWindow.xaml: Margin 0,0,0,28).
pub const SCENE_GAP_DIP: f64 = 28.0;
/// The cursor this close to the left edge brings the sidebar out (MainWindow: e.Data.X <= 44).
pub const EDGE_TRIGGER_DIP: f64 = 44.0;
pub const CARD_RADIUS_DIP: f64 = 8.0;

/// The card size a window of this size gets, in dips (`SceneModel.CardSizeDip`).
pub fn card_size_dip(source_w: f64, source_h: f64, screen_h_dip: f64) -> (f64, f64) {
    if source_w <= 0.0 || source_h <= 0.0 {
        return (0.0, 0.0);
    }
    let s = BASE_CARD_SCALE.max(MIN_CARD_HEIGHT_DIP / source_h);
    let (card_w, card_h) = (source_w * s, source_h * s);
    // macOS anchors the card's perspective at its left edge; ours converges symmetrically, so both
    // dimensions shrink by the mid-column factor.
    let d = screen_h_dip * EDGE_PERSPECTIVE_DISTANCE_RATIO;
    let squeeze = 1.0 - card_w / (2.0 * d);
    (card_w * squeeze, card_h * squeeze)
}

/// The slope, in degrees, an edge at height `y_dip` of the screen has (`SceneModel.EdgeTiltDegreesAt`).
pub fn edge_tilt_at(y_dip: f64, screen_h_dip: f64) -> f64 {
    if screen_h_dip <= 0.0 {
        return 0.0;
    }
    let d = screen_h_dip * EDGE_PERSPECTIVE_DISTANCE_RATIO;
    ((y_dip - screen_h_dip / 2.0) / d).atan().to_degrees()
}

/// Splits the slopes of the top and bottom edges into the vertical shear (on the content visual)
/// and the sprite's rotation about Y that the perspective foreshortens (`SolveEdgeAngles`). Solved
/// against the sprite's current height, so the look is the same at any size or DPI.
pub fn solve_edge_angles(top_deg: f64, bottom_deg: f64, base_h_px: f64) -> (f64, f64) {
    let slope_top = -top_deg.to_radians().tan();
    let slope_bottom = -bottom_deg.to_radians().tan();
    let shear_slope = (slope_top + slope_bottom) / 2.0;
    let converge = (slope_top - slope_bottom) / 2.0;
    let shear = shear_slope.atan().to_degrees();
    let rotation = if base_h_px > 0.0 { (2.0 * PERSPECTIVE_DEPTH_PX * converge / base_h_px).atan().to_degrees() } else { 0.0 };
    (shear, rotation)
}

pub type Mat = [f32; 16];

fn mul(a: &Mat, b: &Mat) -> Mat {
    let mut r = [0.0; 16];
    for i in 0..4 {
        for j in 0..4 {
            r[i * 4 + j] = (0..4).map(|k| a[i * 4 + k] * b[k * 4 + j]).sum();
        }
    }
    r
}

const IDENTITY: Mat = [1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0];

fn translate(x: f32, y: f32) -> Mat {
    let mut m = IDENTITY;
    m[12] = x;
    m[13] = y;
    m
}

/// The affine part of a card's look, about the centre of its (oversized) visual
/// (`ComposeTransform`; row-vector convention, as System.Numerics). `scale` is the hover scale,
/// anchored on the card's left edge.
pub fn compose_transform(shear_deg: f64, scale: f64, base_w_px: f64, w_px: f64, h_px: f64) -> Mat {
    let s = scale as f32;
    let tx = ((scale - 1.0) * base_w_px / 2.0) as f32;
    let (cx, cy) = ((w_px / 2.0) as f32, (h_px / 2.0) as f32);
    let mut inner = IDENTITY;
    inner[0] = s;
    inner[5] = s;
    if shear_deg != 0.0 {
        let mut shear = IDENTITY;
        shear[1] = shear_deg.to_radians().tan() as f32; // M12
        inner = mul(&inner, &shear);
    }
    let m = mul(&mul(&translate(-cx, -cy), &inner), &translate(cx, cy));
    mul(&m, &translate(-tx, 0.0))
}

/// The pure perspective on the root of a card's visuals: T(-c) * P * T(c), P.M34 = -1/depth.
pub fn perspective(w_px: f64, h_px: f64) -> Mat {
    let (cx, cy) = ((w_px / 2.0) as f32, (h_px / 2.0) as f32);
    let mut p = IDENTITY;
    p[11] = (-1.0 / PERSPECTIVE_DEPTH_PX) as f32; // M34
    mul(&mul(&translate(-cx, -cy), &p), &translate(cx, cy))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_flat_pair_of_edges_needs_no_rotation() {
        let (shear, rot) = solve_edge_angles(3.0, 3.0, 200.0);
        assert!(rot.abs() < 1e-9);
        assert!((shear - (-(3.0f64.to_radians().tan())).atan().to_degrees()).abs() < 1e-9);
    }

    #[test]
    fn converging_edges_rotate_by_the_height_they_are_solved_for() {
        let (_, small) = solve_edge_angles(2.0, -2.0, 200.0);
        let (_, big) = solve_edge_angles(2.0, -2.0, 1600.0);
        assert!(small.abs() > big.abs() * 4.0, "the rotation must shrink as the card grows");
    }

    #[test]
    fn the_screens_middle_is_flat_and_the_ends_lean_opposite_ways() {
        assert!(edge_tilt_at(540.0, 1080.0).abs() < 1e-9);
        assert!(edge_tilt_at(0.0, 1080.0) < 0.0 && edge_tilt_at(1080.0, 1080.0) > 0.0);
    }

    #[test]
    fn a_card_is_never_shorter_than_the_minimum() {
        let (_, h) = card_size_dip(400.0, 300.0, 1080.0);
        assert!(h > 90.0 && h < 96.0 + 0.5);
        assert_eq!(card_size_dip(0.0, 10.0, 1080.0), (0.0, 0.0));
    }

    #[test]
    fn a_resting_transform_keeps_the_centre_in_place() {
        let m = compose_transform(0.0, 1.0, 100.0, 160.0, 120.0);
        for (i, v) in IDENTITY.iter().enumerate() {
            assert!((m[i] - v).abs() < 1e-4, "{i}");
        }
    }
}
