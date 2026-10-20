//! Wheel geometry: cell layout, hit-testing and on-screen placement.

use serde::{Deserialize, Serialize};
use std::f32::consts::PI;

/// Maximum number of actions a wheel can show.
pub const MAX_ACTIONS: usize = 12;

/// Drawn circumradius of a flat-top honeycomb hexagon.
pub const HEX_RADIUS: f32 = 38.0;
/// Centre-to-centre distance between adjacent honeycomb cells (about 68.82).
pub const HEX_SPACING: f32 = 1.732_050_8 * HEX_RADIUS + 3.0;
/// Corner rounding of a drawn hexagon at `HEX_RADIUS`, in logical pixels.
pub const HEX_CORNER_RADIUS: f32 = 8.0;
/// Width of the viewbox `rounded_hex_path` draws in (a circumradius-2 hexagon).
pub const HEX_VIEWBOX_W: f32 = 4.0;
/// Height of the viewbox `rounded_hex_path` draws in (2·√3).
pub const HEX_VIEWBOX_H: f32 = 2.0 * 1.732_050_8;

/// SVG path commands for a flat-top hexagon with rounded corners, filling a
/// `HEX_VIEWBOX_W` × `HEX_VIEWBOX_H` viewbox.
///
/// `corner` is the corner radius as a fraction of the circumradius (which equals
/// the side length); it is clamped to `0.0..=0.5` so corners never overlap.
/// Each corner is a quadratic curve whose control point is the original vertex.
pub fn rounded_hex_path(corner: f32) -> String {
    let t = corner.clamp(0.0, 0.5);
    let (cx, cy, r) = (
        HEX_VIEWBOX_W / 2.0,
        HEX_VIEWBOX_H / 2.0,
        HEX_VIEWBOX_W / 2.0,
    );
    let vertex = |k: usize| {
        let a = (k as f32) * PI / 3.0;
        (cx + r * a.cos(), cy + r * a.sin())
    };
    let toward = |from: (f32, f32), to: (f32, f32)| {
        (from.0 + (to.0 - from.0) * t, from.1 + (to.1 - from.1) * t)
    };
    let mut path = String::new();
    for k in 0..6 {
        let corner = vertex(k);
        let start = toward(corner, vertex((k + 5) % 6));
        let end = toward(corner, vertex((k + 1) % 6));
        let cmd = if k == 0 { 'M' } else { 'L' };
        path.push_str(&format!(
            "{cmd} {:.4} {:.4} Q {:.4} {:.4} {:.4} {:.4} ",
            start.0, start.1, corner.0, corner.1, end.0, end.1
        ));
    }
    path.push('Z');
    path
}
/// Distance from the wheel centre to the petal centres.
pub const PETAL_RING: f32 = 100.0;
/// Petal radius for up to six items.
pub const PETAL_RADIUS: f32 = 37.0;
/// Distance from the centre below which no petal is selected.
pub const PETAL_DEAD_ZONE: f32 = 36.0;
/// Distance from the centre beyond which no petal is selected. Kept inside
/// the window, so a highlighted petal always means the cursor is over vento.
pub const PETAL_MAX_REACH: f32 = PETALS_EXTENT - 5.0;

const HONEYCOMB_EXTENT_SMALL: f32 = 110.0;
const HONEYCOMB_EXTENT_LARGE: f32 = 180.0;
const PETALS_EXTENT: f32 = 150.0;
const PETAL_GAP: f32 = 3.0;

/// A position in global logical coordinates: origin at the top-left of the
/// primary screen, y grows downward.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
    pub x: f64,
    pub y: f64,
}

/// An offset from the wheel centre.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vec2 {
    pub x: f32,
    pub y: f32,
}

/// A screen rectangle in global logical coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

/// How the action cells are arranged around the dragged file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layout {
    Honeycomb,
    Petals,
}

/// One drawable, selectable action cell.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell {
    pub center: Vec2,
    pub radius: f32,
}

/// The laid-out wheel for a given layout and action count.
#[derive(Debug, Clone, PartialEq)]
pub struct WheelGeometry {
    layout: Layout,
    cells: Vec<Cell>,
    extent: f32,
}

/// Honeycomb cells in axial `(q, r)` coordinates, clockwise from the top.
/// The centre `(0, 0)` is the dragged file and is never an action.
const HONEYCOMB_AXIAL: [(i32, i32); 18] = [
    // Ring 1
    (0, -1),
    (1, -1),
    (1, 0),
    (0, 1),
    (-1, 1),
    (-1, 0),
    // Ring 2
    (0, -2),
    (1, -2),
    (2, -2),
    (2, -1),
    (2, 0),
    (1, 1),
    (0, 2),
    (-1, 2),
    (-2, 2),
    (-2, 1),
    (-2, 0),
    (-1, -1),
];

fn axial_to_offset(q: i32, r: i32) -> Vec2 {
    let (q, r) = (q as f32, r as f32);
    Vec2 {
        x: HEX_SPACING * (3f32.sqrt() / 2.0) * q,
        y: HEX_SPACING * (r + q / 2.0),
    }
}

fn cube_round(qf: f32, rf: f32) -> (i32, i32) {
    let sf = -qf - rf;
    let (mut q, mut r, s) = (qf.round(), rf.round(), sf.round());
    let (dq, dr, ds) = ((q - qf).abs(), (r - rf).abs(), (s - sf).abs());
    if dq > dr && dq > ds {
        q = -r - s
    } else if dr > ds {
        r = -q - s
    }
    (q as i32, r as i32)
}

impl WheelGeometry {
    /// Lays out `count` cells (clamped to `1..=MAX_ACTIONS`) in `layout`.
    pub fn new(layout: Layout, count: usize) -> Self {
        let count = count.clamp(1, MAX_ACTIONS);
        match layout {
            Layout::Honeycomb => Self {
                layout,
                cells: HONEYCOMB_AXIAL[..count]
                    .iter()
                    .map(|&(q, r)| Cell {
                        center: axial_to_offset(q, r),
                        radius: HEX_RADIUS,
                    })
                    .collect(),
                extent: if count <= 6 {
                    HONEYCOMB_EXTENT_SMALL
                } else {
                    HONEYCOMB_EXTENT_LARGE
                },
            },
            Layout::Petals => {
                let n = count as f32;
                let radius = if count > 6 {
                    PETAL_RADIUS.min(PI * PETAL_RING / n - PETAL_GAP)
                } else {
                    PETAL_RADIUS
                };
                let cells = (0..count)
                    .map(|i| {
                        let theta = 2.0 * PI * i as f32 / n;
                        Cell {
                            center: Vec2 {
                                x: PETAL_RING * theta.sin(),
                                y: -PETAL_RING * theta.cos(),
                            },
                            radius,
                        }
                    })
                    .collect();
                Self {
                    layout,
                    cells,
                    extent: PETALS_EXTENT,
                }
            }
        }
    }

    /// The cells, in action order.
    pub fn cells(&self) -> &[Cell] {
        &self.cells
    }

    /// Half the side of the (square) wheel window.
    pub fn extent(&self) -> f32 {
        self.extent
    }

    /// Index of the cell under `offset` (relative to the wheel centre), if any.
    pub fn hit(&self, offset: Vec2) -> Option<usize> {
        match self.layout {
            Layout::Honeycomb => {
                let w = HEX_SPACING * (3f32.sqrt() / 2.0);
                let qf = offset.x / w;
                let rf = offset.y / HEX_SPACING - qf / 2.0;
                let cell = cube_round(qf, rf);
                HONEYCOMB_AXIAL[..self.cells.len()]
                    .iter()
                    .position(|&c| c == cell)
            }
            Layout::Petals => {
                let dist = offset.x.hypot(offset.y);
                if !(PETAL_DEAD_ZONE..=PETAL_MAX_REACH).contains(&dist) {
                    return None;
                }
                let n = self.cells.len() as f32;
                let deg = (offset.y.atan2(offset.x).to_degrees() + 90.0).rem_euclid(360.0);
                Some(((deg / (360.0 / n)).round() as usize) % self.cells.len())
            }
        }
    }
}

/// The screen area that contains `p`, or the one nearest to it (`p` can lie
/// outside every work area, e.g. over the Dock or menu bar). Ties go to the
/// earlier screen; `None` only for an empty list.
pub fn screen_for(p: Point, screens: &[Rect]) -> Option<&Rect> {
    let distance = |s: &Rect| {
        let dx = (s.x - p.x).max(p.x - (s.x + s.w)).max(0.0);
        let dy = (s.y - p.y).max(p.y - (s.y + s.h)).max(0.0);
        dx * dx + dy * dy
    };
    screens
        .iter()
        .min_by(|a, b| distance(a).total_cmp(&distance(b)))
}

/// Picks the wheel centre for `cursor`: on the screen area containing (or
/// nearest to) the cursor, shifted so the whole wheel fits.
pub fn place_center(cursor: Point, extent: f32, screens: &[Rect]) -> Point {
    let Some(screen) = screen_for(cursor, screens) else {
        return cursor;
    };
    let e = f64::from(extent);
    Point {
        x: clamp_axis(cursor.x, screen.x, screen.w, e),
        y: clamp_axis(cursor.y, screen.y, screen.h, e),
    }
}

/// Clamps `c` so `[c - e, c + e]` fits in `[start, start + len]`; a screen
/// smaller than the wheel centres the wheel on it instead.
fn clamp_axis(c: f64, start: f64, len: f64, e: f64) -> f64 {
    let (lo, hi) = (start + e, start + len - e);
    if lo > hi {
        start + len / 2.0
    } else {
        c.clamp(lo, hi)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f32, y: f32) -> Vec2 {
        Vec2 { x, y }
    }

    fn close(a: Vec2, b: Vec2) -> bool {
        (a.x - b.x).abs() < 0.05 && (a.y - b.y).abs() < 0.05
    }

    #[test]
    fn honeycomb_centre_is_none() {
        assert_eq!(
            WheelGeometry::new(Layout::Honeycomb, 6).hit(v(0., 0.)),
            None
        )
    }

    #[test]
    fn honeycomb_ring1_order() {
        let g = WheelGeometry::new(Layout::Honeycomb, 6);
        assert!(close(g.cells()[0].center, v(0., -68.82)));
        assert!(close(g.cells()[1].center, v(59.60, -34.41)));
        for i in 0..6 {
            assert_eq!(g.hit(g.cells()[i].center), Some(i));
        }
    }

    #[test]
    fn honeycomb_tiles_have_no_gap() {
        let g = WheelGeometry::new(Layout::Honeycomb, 6);
        assert_eq!(g.hit(v(0., -33.0)), None); // inside centre hex
        assert_eq!(g.hit(v(0., -36.0)), Some(0)); // just past the shared edge
    }

    #[test]
    fn honeycomb_ring2_only_beyond_six() {
        assert_eq!(
            WheelGeometry::new(Layout::Honeycomb, 6).hit(v(0., -137.6)),
            None
        );
        let g = WheelGeometry::new(Layout::Honeycomb, 8);
        assert_eq!(g.hit(v(0., -137.6)), Some(6));
        assert_eq!(g.hit(g.cells()[7].center), Some(7));
        assert_eq!(g.cells().len(), 8);
    }

    #[test]
    fn petals_sectors_and_zones() {
        let g = WheelGeometry::new(Layout::Petals, 6);
        assert_eq!(g.hit(v(0., -100.)), Some(0));
        assert_eq!(g.hit(v(86.6, -50.)), Some(1));
        assert_eq!(g.hit(v(0., -30.)), None);
        assert_eq!(g.hit(v(0., -200.)), None);
    }

    #[test]
    fn nothing_is_hit_outside_the_window() {
        // A highlighted cell while the cursor is outside the wheel window
        // would send the drop to whatever is underneath.
        for layout in [Layout::Honeycomb, Layout::Petals] {
            for n in 1..=MAX_ACTIONS {
                let g = WheelGeometry::new(layout, n);
                let e = g.extent();
                for beyond in [0.01, 1.0, 10.0, e] {
                    let r = e + beyond;
                    // Walk the square ring at half-extent r, 1 px apart.
                    let steps = (8.0 * r) as usize;
                    for i in 0..steps {
                        let t = i as f32 / steps as f32 * 8.0 * r;
                        let (side, along) = ((t / (2.0 * r)) as u32, t % (2.0 * r) - r);
                        let p = match side {
                            0 => v(along, -r),
                            1 => v(r, along),
                            2 => v(-along, r),
                            _ => v(-r, -along),
                        };
                        assert_eq!(g.hit(p), None, "{layout:?} n={n} at {p:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn petals_shrink_past_six() {
        assert!((WheelGeometry::new(Layout::Petals, 12).cells()[0].radius - 23.18).abs() < 0.05)
    }

    #[test]
    fn extents() {
        assert_eq!(WheelGeometry::new(Layout::Honeycomb, 6).extent(), 110.);
        assert_eq!(WheelGeometry::new(Layout::Honeycomb, 7).extent(), 180.);
        assert_eq!(WheelGeometry::new(Layout::Petals, 3).extent(), 150.);
    }

    #[test]
    fn place_center_clamps_to_edge() {
        let s = [Rect {
            x: 0.,
            y: 0.,
            w: 1440.,
            h: 900.,
        }];
        assert_eq!(
            place_center(Point { x: 5., y: 500. }, 110., &s),
            Point { x: 110., y: 500. }
        );
    }

    #[test]
    fn place_center_over_a_dock_uses_the_nearest_work_area() {
        // Work areas: primary below a 25 px menu bar; a secondary screen on
        // the left whose Dock takes its bottom 80 px.
        let s = [
            Rect {
                x: 0.,
                y: 25.,
                w: 1440.,
                h: 875.,
            },
            Rect {
                x: -1920.,
                y: 0.,
                w: 1920.,
                h: 1000.,
            },
        ];
        // Over the secondary's Dock: stays on the secondary, above the Dock.
        assert_eq!(
            place_center(Point { x: -900., y: 1050. }, 110., &s),
            Point { x: -900., y: 890. }
        );
        // Over the primary's menu bar: pushed below it.
        assert_eq!(
            place_center(Point { x: 700., y: 5. }, 110., &s),
            Point { x: 700., y: 135. }
        );
    }

    #[test]
    fn place_center_secondary_screen_negative_coords() {
        let s = [
            Rect {
                x: 0.,
                y: 0.,
                w: 1440.,
                h: 900.,
            },
            Rect {
                x: -1920.,
                y: 0.,
                w: 1920.,
                h: 1080.,
            },
        ];
        assert_eq!(
            place_center(
                Point {
                    x: -1915.,
                    y: 1075.
                },
                110.,
                &s
            ),
            Point { x: -1810., y: 970. }
        );
    }

    /// Every number in an SVG path string, in order.
    fn path_numbers(path: &str) -> Vec<f32> {
        path.split(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-'))
            .filter(|s| !s.is_empty())
            .map(|s| s.parse().unwrap())
            .collect()
    }

    #[test]
    fn rounded_hex_path_has_six_rounded_corners() {
        let path = rounded_hex_path(HEX_CORNER_RADIUS / HEX_RADIUS);
        assert!(path.starts_with('M') && path.ends_with('Z'));
        assert_eq!(path.matches('Q').count(), 6);
        // Five straight sides; `Z` draws the sixth back to the start.
        assert_eq!(path.matches('L').count(), 5);
    }

    #[test]
    fn rounded_hex_path_stays_inside_the_viewbox() {
        let nums = path_numbers(&rounded_hex_path(HEX_CORNER_RADIUS / HEX_RADIUS));
        for xy in nums.chunks(2) {
            assert!(
                (-0.001..=HEX_VIEWBOX_W + 0.001).contains(&xy[0]),
                "x {}",
                xy[0]
            );
            assert!(
                (-0.001..=HEX_VIEWBOX_H + 0.001).contains(&xy[1]),
                "y {}",
                xy[1]
            );
        }
    }

    #[test]
    fn rounded_hex_path_corner_starts_where_expected() {
        // Corner at vertex (4, √3): radius 8 px of 38 px → t = 8/38 along each side.
        let nums = path_numbers(&rounded_hex_path(8.0 / 38.0));
        let t = 8.0 / 38.0;
        // First point: on the side from the top-right vertex (3, 0) towards (4, √3).
        assert!((nums[0] - (4.0 - t)).abs() < 0.01);
        assert!((nums[1] - (1.732_050_8 - 1.732_050_8 * t)).abs() < 0.01);
        // Its control point is the vertex itself.
        assert!((nums[2] - 4.0).abs() < 0.01 && (nums[3] - 1.732_050_8).abs() < 0.01);
    }

    #[test]
    fn rounded_hex_path_corner_is_clamped() {
        let nums = path_numbers(&rounded_hex_path(5.0));
        // t clamps to 0.5: the first point is the midpoint of the top-right side.
        assert!((nums[0] - 3.5).abs() < 0.01);
    }
}
