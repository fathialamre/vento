//! Geometry and text layout for "Copy from screen".
//!
//! Nothing here touches the OS. The app turns a mouse drag into a [`Selection`]
//! with [`selection_from_drag`], hands the selected pixels to OCR, and feeds
//! the recognised [`TextBox`]es to [`assemble`], which rebuilds the lines the
//! reader saw: rows top to bottom, left to right (right to left for Arabic and
//! Hebrew), with the indentation of left-to-right text kept.

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

/// Most spaces a row is indented by. Very narrow characters (a box of
/// punctuation, say) make the estimated character width tiny, and the
/// indent huge; past this it is noise, not layout.
pub const MAX_INDENT: usize = 40;

/// Rebuilds readable text from OCR boxes: rows top to bottom, boxes within a
/// row in reading order, rows indented by their left edge, joined with `\n`.
/// Blank boxes are ignored; no usable box gives an empty string.
///
/// A row of several boxes reads right to left when it holds at least as many
/// Arabic/Hebrew words as Latin-style ones, so `اضغط Ctrl` keeps the Arabic
/// first even when OCR splits it into boxes, while an English line ending in
/// an Arabic phrase stays left to right. A row of one box, whose text OCR
/// already gives in reading order, takes the direction of its first letter,
/// as bidirectional text does, so `s = "مرحبا"` stays a left-to-right code
/// line. Rows read left to right are indented from the leftmost box, at most
/// [`MAX_INDENT`] spaces; right-to-left rows are not, and nothing is indented
/// in a capture that is mostly right-to-left, where lines align right.
pub fn assemble(boxes: &[TextBox]) -> String {
    let mut boxes: Vec<&TextBox> = boxes.iter().filter(|b| !b.text.trim().is_empty()).collect();
    if boxes.is_empty() {
        return String::new();
    }
    boxes.sort_by(|a, b| centre_y(a).total_cmp(&centre_y(b)));

    let capture_rtl = letters(boxes.iter().flat_map(|b| b.text.chars())).rtl_majority();
    let left = boxes.iter().map(|b| b.rect.x).fold(f64::INFINITY, f64::min);
    let char_w = char_width(&boxes);

    let lines: Vec<String> = group_rows(&boxes)
        .into_iter()
        .map(|mut row| {
            let rtl = match row.as_slice() {
                [only] => first_letter_is_rtl(&only.text),
                _ => reads_right_to_left(&row),
            };
            row.sort_by(|a, b| a.rect.x.total_cmp(&b.rect.x));
            let row = reading_order(row, rtl);
            let indent = if capture_rtl || rtl || char_w <= 0.0 {
                0
            } else {
                let row_left = row.iter().map(|b| b.rect.x).fold(f64::INFINITY, f64::min);
                ((row_left - left) / char_w)
                    .round()
                    .clamp(0.0, MAX_INDENT as f64) as usize
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

/// How many right-to-left (Arabic, Hebrew) and left-to-right (any other
/// alphabetic) letters some boxes hold.
#[derive(Debug, Clone, Copy, Default)]
struct Letters {
    rtl: usize,
    ltr: usize,
}

impl Letters {
    /// More right-to-left letters than left-to-right ones.
    fn rtl_majority(self) -> bool {
        self.rtl > self.ltr
    }
}

/// Whether `c` is an Arabic or Hebrew letter (or presentation form). Their
/// digits and punctuation are not letters, and neither are the variation
/// selectors and small or vertical forms between the presentation blocks.
fn is_rtl(c: char) -> bool {
    c.is_alphabetic()
        && matches!(
            c,
            '\u{0590}'..='\u{08FF}' | '\u{FB1D}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFC}'
        )
}

/// Whether the first letter of `text` is Arabic/Hebrew.
fn first_letter_is_rtl(text: &str) -> bool {
    text.chars().find(|c| c.is_alphabetic()).is_some_and(is_rtl)
}

/// Counts the letters of each direction in `chars`.
fn letters(chars: impl Iterator<Item = char>) -> Letters {
    let mut n = Letters::default();
    for c in chars {
        if is_rtl(c) {
            n.rtl += 1;
        } else if c.is_alphabetic() {
            n.ltr += 1;
        }
    }
    n
}

/// How many right-to-left and left-to-right words `boxes` hold. A word goes
/// with the script of most of its letters (a tie to Arabic/Hebrew), so
/// `print("تم")` is a Latin word; numbers and punctuation are neither.
fn word_counts(boxes: &[&TextBox]) -> (usize, usize) {
    let (mut rtl, mut ltr) = (0, 0);
    for word in boxes.iter().flat_map(|b| b.text.split_whitespace()) {
        let n = letters(word.chars());
        match (n.rtl, n.ltr) {
            (0, 0) => {}
            (r, l) if r >= l => rtl += 1,
            _ => ltr += 1,
        }
    }
    (rtl, ltr)
}

/// Whether a row reads right to left: it has an Arabic/Hebrew word and at
/// least as many of them as Latin-style words.
fn reads_right_to_left(row: &[&TextBox]) -> bool {
    let (rtl, ltr) = word_counts(row);
    rtl > 0 && rtl >= ltr
}

/// A box's own direction, or `None` when it holds no letters (numbers,
/// punctuation).
fn box_direction(b: &TextBox) -> Option<bool> {
    match word_counts(&[b]) {
        (0, 0) => None,
        (rtl, ltr) => Some(rtl >= ltr),
    }
}

/// Puts a row's boxes, sorted left to right as they appear on screen, in
/// reading order, the way bidirectional text is laid out: the runs of boxes
/// follow the row's direction, and each run reads in its own direction, so
/// Arabic words OCR split apart inside an English row (or English words
/// inside an Arabic row) keep their order. A box with no letters takes the
/// direction of the boxes around it when both agree, else the row's.
fn reading_order(visual: Vec<&TextBox>, rtl: bool) -> Vec<&TextBox> {
    let strong: Vec<Option<bool>> = visual.iter().map(|b| box_direction(b)).collect();
    let mut runs: Vec<(bool, Vec<&TextBox>)> = Vec::new();
    for (i, b) in visual.into_iter().enumerate() {
        let dir = strong[i].unwrap_or_else(|| {
            let before = strong[..i].iter().rev().find_map(|d| *d);
            let after = strong[i + 1..].iter().find_map(|d| *d);
            match (before, after) {
                (Some(x), Some(y)) if x == y => x,
                _ => rtl,
            }
        });
        match runs.last_mut() {
            Some((run_dir, run)) if *run_dir == dir => run.push(b),
            _ => runs.push((dir, vec![b])),
        }
    }
    if rtl {
        runs.reverse();
    }
    runs.into_iter()
        .flat_map(|(dir, mut run)| {
            if dir {
                run.reverse();
            }
            run
        })
        .collect()
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

    #[test]
    fn word_tie_row_reads_right_to_left() {
        // Vision split a clipped line in two: one Latin word, one Arabic.
        let boxes = [
            b(0.0, 0.0, 40.0, 20.0, "Ctrl"),
            b(60.0, 0.0, 40.0, 20.0, "اضغط"),
            b(0.0, 30.0, 130.0, 20.0, "مرحبا بالعالم"),
        ];
        assert_eq!(assemble(&boxes), "اضغط Ctrl\nمرحبا بالعالم");
    }

    #[test]
    fn more_latin_letters_but_a_word_tie_reads_right_to_left() {
        // Five Latin letters to four Arabic, but one word each: a tie.
        let boxes = [
            b(0.0, 0.0, 50.0, 20.0, "Enter"),
            b(70.0, 0.0, 40.0, 20.0, "اضغط"),
            b(0.0, 30.0, 130.0, 20.0, "مرحبا بالعالم"),
        ];
        assert_eq!(assemble(&boxes), "اضغط Enter\nمرحبا بالعالم");
    }

    #[test]
    fn english_capture_keeps_left_to_right_and_indent() {
        // 10 px per character; the middle row is split in two boxes.
        let boxes = [
            b(0.0, 0.0, 80.0, 20.0, "fn x() {"),
            b(100.0, 30.0, 40.0, 20.0, "= 1;"),
            b(40.0, 30.0, 50.0, 20.0, "let a"),
            b(0.0, 60.0, 10.0, 20.0, "}"),
        ];
        assert_eq!(assemble(&boxes), "fn x() {\n    let a = 1;\n}");
    }

    #[test]
    fn rtl_capture_is_not_indented() {
        // Right-aligned lines: the shorter ones start further right, and an
        // English-only line in an Arabic capture is not padded either.
        let boxes = [
            b(0.0, 0.0, 130.0, 20.0, "مرحبا بالعالم"),
            b(90.0, 30.0, 40.0, 20.0, "اهلا"),
            b(90.0, 60.0, 40.0, 20.0, "Ctrl"),
        ];
        assert_eq!(assemble(&boxes), "مرحبا بالعالم\nاهلا\nCtrl");
    }

    #[test]
    fn arabic_row_in_english_capture_reads_right_to_left() {
        // The gate's case: English menus around a clipped line split in two
        // boxes, as many Latin letters as Arabic. Not indented either.
        let boxes = [
            b(0.0, 0.0, 200.0, 20.0, "Preview File Edit View"),
            b(0.0, 30.0, 160.0, 20.0, "Screen Recording"),
            b(400.0, 60.0, 40.0, 20.0, "Ctrl"),
            b(460.0, 60.0, 40.0, 20.0, "اضغط"),
        ];
        assert_eq!(
            assemble(&boxes),
            "Preview File Edit View\nScreen Recording\nاضغط Ctrl"
        );
    }

    #[test]
    fn arabic_heavy_row_in_english_capture_reads_right_to_left() {
        let boxes = [
            b(0.0, 0.0, 200.0, 20.0, "Preview File Edit View"),
            b(0.0, 30.0, 160.0, 20.0, "Screen Recording"),
            b(300.0, 60.0, 60.0, 20.0, "Finder"),
            b(380.0, 60.0, 120.0, 20.0, "افتح الملف من"),
        ];
        assert_eq!(
            assemble(&boxes),
            "Preview File Edit View\nScreen Recording\nافتح الملف من Finder"
        );
    }

    #[test]
    fn english_row_with_an_arabic_phrase_reads_left_to_right() {
        // Owner's full-screen check: five English words to three Arabic.
        let boxes = [
            b(260.0, 0.0, 130.0, 20.0, "افتح الملف من"),
            b(0.0, 0.0, 240.0, 20.0, "Line 12: The quick brown"),
            b(400.0, 0.0, 60.0, 20.0, "Finder"),
        ];
        assert_eq!(
            assemble(&boxes),
            "Line 12: The quick brown افتح الملف من Finder"
        );
    }

    #[test]
    fn arabic_words_split_in_an_english_row_keep_their_order() {
        // Owner's full-screen check: Vision gave each Arabic word its own box,
        // placed as shown on screen (right to left within each Arabic run).
        let boxes = [
            b(0.0, 0.0, 180.0, 20.0, "Line 1: The quick brown fox"),
            b(200.0, 0.0, 30.0, 20.0, "من"),
            b(240.0, 0.0, 50.0, 20.0, "الملف"),
            b(300.0, 0.0, 40.0, 20.0, "افتح"),
            b(360.0, 0.0, 60.0, 20.0, "Finder"),
            b(430.0, 0.0, 40.0, 20.0, "اضغط"),
            b(480.0, 0.0, 60.0, 20.0, "وبعدين"),
            b(560.0, 0.0, 40.0, 20.0, "Ctrl"),
        ];
        assert_eq!(
            assemble(&boxes),
            "Line 1: The quick brown fox افتح الملف من Finder وبعدين اضغط Ctrl"
        );
    }

    #[test]
    fn english_words_split_in_an_arabic_row_keep_their_order() {
        let boxes = [
            b(0.0, 0.0, 40.0, 20.0, "Ctrl"),
            b(50.0, 0.0, 50.0, 20.0, "Enter"),
            b(120.0, 0.0, 30.0, 20.0, "على"),
            b(170.0, 0.0, 40.0, 20.0, "اضغط"),
        ];
        assert_eq!(assemble(&boxes), "اضغط على Ctrl Enter");
    }

    #[test]
    fn code_with_arabic_strings_keeps_its_indent() {
        // 10 px per character.
        let boxes = [
            b(0.0, 0.0, 60.0, 20.0, "if ok:"),
            b(40.0, 30.0, 110.0, 20.0, "print(\"تم\")"),
            b(40.0, 60.0, 110.0, 20.0, "s = \"مرحبا\""),
            b(40.0, 90.0, 80.0, 20.0, "return x"),
        ];
        assert_eq!(
            assemble(&boxes),
            "if ok:\n    print(\"تم\")\n    s = \"مرحبا\"\n    return x"
        );
    }

    #[test]
    fn arabic_digits_and_presentation_marks_are_not_letters() {
        let boxes = [
            b(0.0, 0.0, 40.0, 20.0, "Page"),
            b(60.0, 0.0, 10.0, 20.0, "٣"),
        ];
        assert_eq!(assemble(&boxes), "Page ٣");
        let boxes = [
            b(0.0, 0.0, 50.0, 20.0, "Hello"),
            b(60.0, 0.0, 10.0, 20.0, "﹐"),
        ];
        assert_eq!(assemble(&boxes), "Hello ﹐");
    }

    #[test]
    fn indentation_is_capped() {
        // 10 px per character: 2000 px would be 200 spaces.
        let boxes = [
            b(0.0, 0.0, 10.0, 20.0, "a"),
            b(2000.0, 30.0, 10.0, 20.0, "b"),
        ];
        assert_eq!(assemble(&boxes), format!("a\n{}b", " ".repeat(MAX_INDENT)));
    }
}
