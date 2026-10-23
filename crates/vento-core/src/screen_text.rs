//! Geometry and text layout for "Copy from screen".
//!
//! Nothing here touches the OS. The app turns a mouse drag into a [`Selection`]
//! with [`selection_from_drag`], hands the selected pixels to OCR, and feeds
//! the recognised [`TextBox`]es to [`assemble`], which rebuilds the lines the
//! reader saw: rows top to bottom, left to right (right to left for Arabic and
//! Hebrew rows), with the indentation kept.

use crate::geometry::{Point, Rect};

/// Smallest side, in logical pixels, a selection must have to count.
pub const MIN_SIDE: f64 = 4.0;

/// One recognised piece of text (an OCR observation).
#[derive(Debug, Clone, PartialEq)]
pub struct TextBox {
    /// Where the text sits, in image pixels with a top-left origin.
    pub rect: Rect,
    /// The recognised text.
    pub text: String,
}

/// A rectangle the user selected on one display.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Selection {
    /// Identifier of the display the selection is on.
    pub display_id: u32,
    /// The display's full frame, in global logical coordinates.
    pub display: Rect,
    /// The selected area, in global logical coordinates.
    pub rect: Rect,
}

/// The rectangle between two drag points, in either direction, clamped to
/// `display`. `None` when either side is under [`MIN_SIDE`] after clamping,
/// when a point is not finite, or when `display` is not a real rectangle (not
/// finite, or negative in size). Never panics, whatever the input: the
/// overlay calls it from AppKit event handlers.
pub fn selection_from_drag(start: Point, end: Point, display: Rect) -> Option<Rect> {
    let real = [
        display.x, display.y, display.w, display.h, start.x, start.y, end.x, end.y,
    ]
    .iter()
    .all(|v| v.is_finite())
        && display.w >= 0.0
        && display.h >= 0.0;
    if !real {
        return None;
    }
    // `max`/`min`, not `clamp`, which panics on bad bounds.
    let clamp = |p: Point| Point {
        x: p.x.max(display.x).min(display.x + display.w),
        y: p.y.max(display.y).min(display.y + display.h),
    };
    let (a, b) = (clamp(start), clamp(end));
    let rect = Rect {
        x: a.x.min(b.x),
        y: a.y.min(b.y),
        w: (a.x - b.x).abs(),
        h: (a.y - b.y).abs(),
    };
    (rect.w >= MIN_SIDE && rect.h >= MIN_SIDE).then_some(rect)
}

/// The selection rectangle relative to its display's top-left corner.
pub fn local_rect(sel: &Selection) -> Rect {
    Rect {
        x: sel.rect.x - sel.display.x,
        y: sel.rect.y - sel.display.y,
        w: sel.rect.w,
        h: sel.rect.h,
    }
}

/// Rebuilds readable text from OCR boxes: rows top to bottom, boxes within a
/// row in reading order, rows indented by their left edge, joined with `\n`.
/// Blank boxes are ignored; no usable box gives an empty string.
pub fn assemble(boxes: &[TextBox]) -> String {
    let mut boxes: Vec<&TextBox> = boxes.iter().filter(|b| !b.text.trim().is_empty()).collect();
    if boxes.is_empty() {
        return String::new();
    }
    boxes.sort_by(|a, b| centre_y(a).total_cmp(&centre_y(b)));

    let left = boxes.iter().map(|b| b.rect.x).fold(f64::INFINITY, f64::min);
    let char_w = char_width(&boxes);

    let lines: Vec<String> = group_rows(&boxes)
        .into_iter()
        .map(|mut row| {
            let rtl = is_rtl(&row);
            row.sort_by(|a, b| a.rect.x.total_cmp(&b.rect.x));
            if rtl {
                row.reverse();
            }
            let indent = if rtl || char_w <= 0.0 {
                0
            } else {
                let row_left = row.iter().map(|b| b.rect.x).fold(f64::INFINITY, f64::min);
                ((row_left - left) / char_w).round().max(0.0) as usize
            };
            let texts: Vec<&str> = row.iter().map(|b| b.text.as_str()).collect();
            format!("{}{}", " ".repeat(indent), texts.join(" "))
                .trim_end()
                .to_string()
        })
        .collect();
    lines.join("\n")
}

fn centre_y(b: &TextBox) -> f64 {
    b.rect.y + b.rect.h / 2.0
}

/// Splits boxes (sorted by vertical centre) into rows: a box joins the current
/// row when its centre is within half the smaller height of the row's first.
fn group_rows<'a>(sorted: &[&'a TextBox]) -> Vec<Vec<&'a TextBox>> {
    let mut rows: Vec<Vec<&TextBox>> = Vec::new();
    for &b in sorted {
        match rows.last_mut() {
            Some(row)
                if (centre_y(b) - centre_y(row[0])).abs() < b.rect.h.min(row[0].rect.h) / 2.0 =>
            {
                row.push(b);
            }
            _ => rows.push(vec![b]),
        }
    }
    rows
}

/// Whether a row reads right to left: more Arabic/Hebrew characters than
/// Latin-style letters.
fn is_rtl(row: &[&TextBox]) -> bool {
    let (mut rtl, mut ltr) = (0usize, 0usize);
    for c in row.iter().flat_map(|b| b.text.chars()) {
        if matches!(c, '\u{0590}'..='\u{08FF}' | '\u{FB1D}'..='\u{FEFF}') {
            rtl += 1;
        } else if c.is_alphabetic() {
            ltr += 1;
        }
    }
    rtl > ltr
}

/// Typical width of one character: the median of width per character.
fn char_width(boxes: &[&TextBox]) -> f64 {
    let mut widths: Vec<f64> = boxes
        .iter()
        .map(|b| b.rect.w / b.text.chars().count() as f64)
        .collect();
    widths.sort_by(f64::total_cmp);
    let mid = widths.len() / 2;
    if widths.len() % 2 == 1 {
        widths[mid]
    } else {
        (widths[mid - 1] + widths[mid]) / 2.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(x: f64, y: f64, w: f64, h: f64) -> Rect {
        Rect { x, y, w, h }
    }
    fn b(x: f64, y: f64, w: f64, h: f64, t: &str) -> TextBox {
        TextBox {
            rect: r(x, y, w, h),
            text: t.into(),
        }
    }
    const D: Rect = Rect {
        x: 0.0,
        y: 0.0,
        w: 1000.0,
        h: 800.0,
    };

    #[test]
    fn drag_up_left_is_normalised() {
        let got = selection_from_drag(Point { x: 300.0, y: 200.0 }, Point { x: 100.0, y: 50.0 }, D);
        assert_eq!(got, Some(r(100.0, 50.0, 200.0, 150.0)));
    }

    #[test]
    fn drag_past_the_edge_is_clamped() {
        let got = selection_from_drag(
            Point { x: 900.0, y: 700.0 },
            Point {
                x: 1300.0,
                y: 900.0,
            },
            D,
        );
        assert_eq!(got, Some(r(900.0, 700.0, 100.0, 100.0)));
    }

    #[test]
    fn tiny_drag_is_none() {
        assert_eq!(
            selection_from_drag(Point { x: 10.0, y: 10.0 }, Point { x: 13.0, y: 40.0 }, D),
            None
        );
        assert_eq!(
            selection_from_drag(Point { x: 10.0, y: 10.0 }, Point { x: 10.0, y: 10.0 }, D),
            None
        );
    }

    #[test]
    fn degenerate_display_does_not_panic() {
        let (a, z) = (Point { x: 10.0, y: 10.0 }, Point { x: 200.0, y: 200.0 });
        let nan_w = Rect { w: f64::NAN, ..D };
        assert_eq!(selection_from_drag(a, z, nan_w), None);
        let negative = Rect { w: -100.0, ..D };
        assert_eq!(selection_from_drag(a, z, negative), None);
        let nan_point = Point {
            x: f64::NAN,
            y: 10.0,
        };
        assert_eq!(selection_from_drag(nan_point, z, D), None);
    }

    #[test]
    fn local_rect_on_display_left_of_primary() {
        let sel = Selection {
            display_id: 2,
            display: r(-1920.0, -200.0, 1920.0, 1080.0),
            rect: r(-1800.0, -100.0, 300.0, 50.0),
        };
        assert_eq!(local_rect(&sel), r(120.0, 100.0, 300.0, 50.0));
    }

    #[test]
    fn rows_come_back_top_to_bottom_left_to_right() {
        // Vision order: the line-number column first, then the code (spike).
        let boxes = [
            b(0.0, 0.0, 10.0, 20.0, "1"),
            b(0.0, 30.0, 10.0, 20.0, "2"),
            b(40.0, 1.0, 100.0, 18.0, "class Hello"),
            b(40.0, 31.0, 10.0, 18.0, "{"),
        ];
        assert_eq!(assemble(&boxes), "1 class Hello\n2 {");
    }

    #[test]
    fn arabic_row_reads_right_to_left() {
        let boxes = [
            b(300.0, 0.0, 100.0, 20.0, "مرحبا"),
            b(100.0, 0.0, 150.0, 20.0, "بالعالم"),
        ];
        assert_eq!(assemble(&boxes), "مرحبا بالعالم");
    }

    #[test]
    fn mixed_observation_is_kept_as_is() {
        let boxes = [b(
            0.0,
            0.0,
            500.0,
            20.0,
            "افتح الملف من Finder وبعدين اضغط Ctrl",
        )];
        assert_eq!(assemble(&boxes), "افتح الملف من Finder وبعدين اضغط Ctrl");
    }

    #[test]
    fn indentation_follows_the_left_edge() {
        // 10 px per character.
        let boxes = [
            b(0.0, 0.0, 50.0, 20.0, "fn x("),
            b(40.0, 30.0, 60.0, 20.0, "body()"),
            b(0.0, 60.0, 10.0, 20.0, "}"),
        ];
        assert_eq!(assemble(&boxes), "fn x(\n    body()\n}");
    }

    #[test]
    fn assemble_of_blank_boxes_is_empty() {
        assert_eq!(assemble(&[]), "");
        assert_eq!(
            assemble(&[b(0.0, 0.0, 30.0, 20.0, "  "), b(0.0, 30.0, 30.0, 20.0, "")]),
            ""
        );
    }
}
