//! Copy from screen on GNOME. The Vento extension dims the screens, lets the
//! user drag a rectangle and captures it (vento's X11 windows cannot see
//! Wayland ones); Tesseract reads the text, locally. The selection is asked
//! for on a thread of its own, since the call waits for the user, and handed
//! to the main thread from the next input sample.

use std::cell::RefCell;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};

use image::imageops::FilterType;
use vento_core::geometry::{Point, Rect};
use vento_core::screen_text::{MIN_SIDE, Selection, TextBox};

use super::shell::{self, Monitor};
use crate::{PlatformError, ScreenText};

/// What the captured picture is scaled to, in times the screens' logical
/// pixels: Tesseract reads screen-sized text far better scaled up (at 1x it
/// misreads Latin words in an Arabic line), and gains nothing past this.
const READ_SCALE: f64 = 3.0;
/// Tesseract's page mode: one block of text, read line by line, which is
/// what a dragged rectangle holds.
const PAGE_MODE: &str = "6";
/// Bidirectional marks Tesseract puts around words; the row assembly orders
/// the text itself.
const BIDI_MARKS: [char; 9] = [
    '\u{200e}', '\u{200f}', '\u{061c}', '\u{202a}', '\u{202b}', '\u{202c}', '\u{202d}', '\u{202e}',
    '\u{2066}',
];

/// Gets the selected area, or `None` when cancelled.
type Done = Box<dyn FnOnce(Option<Selection>)>;

thread_local! {
    /// The callback of the selection under way; main thread only.
    static WAITING: RefCell<Option<Done>> = const { RefCell::new(None) };
}

/// The rectangle the extension answered with (`None` inside: cancelled),
/// until the main thread takes it.
static ANSWER: Mutex<Option<Option<Rect>>> = Mutex::new(None);

/// Copy from screen as GNOME and Tesseract do it.
pub struct LinuxScreenText;

impl ScreenText for LinuxScreenText {
    /// Tesseract is installed; the extension is checked when the area is
    /// selected.
    fn supported(&self) -> bool {
        tesseract().is_some()
    }

    /// The extension is part of the shell: there is nothing to grant.
    fn has_access(&self) -> bool {
        true
    }

    fn request_access(&self) {}

    fn open_access_settings(&self) {}

    fn select_area(&self, done: Done) {
        if WAITING.with(|w| w.borrow().is_some()) {
            done(None);
            return;
        }
        WAITING.with(|w| *w.borrow_mut() = Some(done));
        let asked = std::thread::Builder::new()
            .name("vento-select-area".into())
            .spawn(|| {
                let rect = shell::select_area();
                *ANSWER.lock().unwrap() = Some(rect);
            });
        if let Err(e) = asked {
            log::warn!("cannot ask for an area: {e}");
            *ANSWER.lock().unwrap() = Some(None);
        }
    }
}

/// Hands a selection the extension answered with to its callback, the
/// rectangle placed on the screen holding its centre. From the input
/// sample, on the main thread.
pub(super) fn deliver_selection(monitors: impl FnOnce() -> Vec<Monitor>) {
    let Some(rect) = ANSWER.lock().unwrap().take() else {
        return;
    };
    let selection = rect.and_then(|rect| selection_on(rect, &monitors()));
    if let Some(done) = WAITING.with(|w| w.borrow_mut().take()) {
        done(selection);
    }
}

/// `rect` on the screen holding its centre; `None` when it is too small to
/// hold text.
fn selection_on(rect: Rect, monitors: &[Monitor]) -> Option<Selection> {
    if rect.w < MIN_SIDE || rect.h < MIN_SIDE {
        return None;
    }
    let centre = Point {
        x: rect.x + rect.w / 2.0,
        y: rect.y + rect.h / 2.0,
    };
    let index = monitors
        .iter()
        .position(|m| shell::monitor_at(std::slice::from_ref(m), centre).is_some())
        .unwrap_or(0);
    let display = monitors.get(index).map_or(rect, |m| m.frame);
    Some(Selection {
        display_id: u32::try_from(index).unwrap_or(0),
        display,
        rect,
    })
}

/// Captures `sel` through the extension and reads it with Tesseract, a
/// box per line. The picture stays in memory.
pub(crate) fn read_screen_text(sel: &Selection) -> Result<Vec<TextBox>, PlatformError> {
    let tesseract = tesseract().ok_or(PlatformError::Unsupported)?;
    let png = shell::capture_area(sel.rect)
        .map_err(|e| PlatformError::Failed(format!("cannot capture the area: {e}")))?;
    let picture = image::load_from_memory(&png)
        .map_err(|e| PlatformError::Failed(format!("the capture does not decode: {e}")))?;
    drop(png);
    let factor = (READ_SCALE * sel.rect.w / f64::from(picture.width().max(1))).max(1.0);
    let picture = if factor > 1.0 {
        let w = (f64::from(picture.width()) * factor).round() as u32;
        let h = (f64::from(picture.height()) * factor).round() as u32;
        picture.resize_exact(w, h, FilterType::CatmullRom)
    } else {
        picture
    };
    let mut scaled = Vec::new();
    picture
        .to_luma8()
        .write_to(
            &mut std::io::Cursor::new(&mut scaled),
            image::ImageFormat::Png,
        )
        .map_err(|e| PlatformError::Failed(format!("cannot hand the capture over: {e}")))?;
    drop(picture);
    let tsv = run_tesseract(tesseract, &scaled)?;
    Ok(words(&tsv))
}

/// Tesseract's TSV for the PNG `png`, piped in and out.
fn run_tesseract(tesseract: &Tesseract, png: &[u8]) -> Result<String, PlatformError> {
    let failed = |e: std::io::Error| PlatformError::Failed(format!("tesseract: {e}"));
    let mut child = Command::new(&tesseract.path)
        .args([
            "stdin",
            "stdout",
            "-l",
            &tesseract.languages,
            "--psm",
            PAGE_MODE,
            "tsv",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(failed)?;
    // Written on a thread, so a full output pipe cannot stall the input.
    let mut stdin = child.stdin.take().expect("stdin is piped");
    let png = png.to_vec();
    let writer = std::thread::spawn(move || stdin.write_all(&png));
    let output = child.wait_with_output().map_err(failed)?;
    writer
        .join()
        .map_err(|_| PlatformError::Failed("tesseract: the writer panicked".into()))?
        .map_err(failed)?;
    if !output.status.success() {
        return Err(PlatformError::Failed(format!(
            "tesseract: {}",
            output.status
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Tesseract's words, a box each, those of one line (block, paragraph,
/// line) given the line's top and height so the row assembly keeps them in
/// one row. The assembly puts them in reading order from where they are:
/// Tesseract's own order is unreliable in a line mixing Arabic and Latin,
/// sometimes reading order, sometimes left to right. The boxes are in the
/// read picture's pixels.
fn words(tsv: &str) -> Vec<TextBox> {
    let mut lines: Vec<((u32, u32, u32), Vec<TextBox>)> = Vec::new();
    for row in tsv.lines().skip(1) {
        let cols: Vec<&str> = row.split('\t').collect();
        let [
            level,
            _,
            block,
            par,
            line,
            _,
            left,
            top,
            width,
            height,
            _,
            text,
        ] = cols[..]
        else {
            continue;
        };
        let word: String = text.chars().filter(|c| !BIDI_MARKS.contains(c)).collect();
        let word = word.trim();
        if level != "5" || word.is_empty() {
            continue;
        }
        let num = |s: &str| s.parse::<f64>().unwrap_or(0.0);
        let id = |s: &str| s.parse::<u32>().unwrap_or(0);
        let key = (id(block), id(par), id(line));
        let word = TextBox {
            rect: Rect {
                x: num(left),
                y: num(top),
                w: num(width),
                h: num(height),
            },
            text: word.to_owned(),
        };
        match lines.last_mut() {
            Some((last, words)) if *last == key => words.push(word),
            _ => lines.push((key, vec![word])),
        }
    }
    lines
        .into_iter()
        .flat_map(|(_, mut words)| {
            if let Some(line) = words.iter().map(|w| w.rect).reduce(union) {
                for word in &mut words {
                    word.rect.y = line.y;
                    word.rect.h = line.h;
                }
            }
            words
        })
        .collect()
}

fn union(a: Rect, b: Rect) -> Rect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    Rect {
        x,
        y,
        w: (a.x + a.w).max(b.x + b.w) - x,
        h: (a.y + a.h).max(b.y + b.h) - y,
    }
}

/// The `tesseract` program and the languages it is run with.
struct Tesseract {
    path: PathBuf,
    /// `ara+eng`, or what of the two is installed.
    languages: String,
}

/// Tesseract, if installed with English or Arabic; looked up once.
fn tesseract() -> Option<&'static Tesseract> {
    static FOUND: OnceLock<Option<Tesseract>> = OnceLock::new();
    FOUND
        .get_or_init(|| {
            let path = std::env::var_os("PATH")?
                .to_str()?
                .split(':')
                .map(|dir| PathBuf::from(dir).join("tesseract"))
                .find(|p| p.is_file())?;
            let installed = list_languages(&path);
            let languages: Vec<&str> = ["ara", "eng"]
                .into_iter()
                .filter(|l| installed.iter().any(|i| i == l))
                .collect();
            if languages.is_empty() {
                log::warn!("tesseract has neither Arabic nor English: Copy from screen is off");
                return None;
            }
            Some(Tesseract {
                path,
                languages: languages.join("+"),
            })
        })
        .as_ref()
}

/// The languages `tesseract --list-langs` names.
fn list_languages(path: &std::path::Path) -> Vec<String> {
    let Ok(output) = Command::new(path).arg("--list-langs").output() else {
        return Vec::new();
    };
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .skip(1)
        .map(|l| l.trim().to_owned())
        .filter(|l| !l.is_empty())
        .collect()
}

/// The languages Copy from screen reads, for the log.
pub(crate) fn screen_text_languages() -> Vec<String> {
    tesseract().map_or_else(Vec::new, |t| {
        t.languages.split('+').map(str::to_owned).collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEADER: &str = "level\tpage_num\tblock_num\tpar_num\tline_num\tword_num\tleft\ttop\twidth\theight\tconf\ttext";

    fn tsv(rows: &[&str]) -> String {
        std::iter::once(HEADER)
            .chain(rows.iter().copied())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_mixed_line_reads_in_order_whatever_order_tesseract_gives() {
        use vento_core::screen_text::assemble;
        // A mixed Arabic line as Tesseract gave it once left to right (as
        // on screen), then an English line.
        let tsv = tsv(&[
            "4\t1\t1\t1\t1\t0\t0\t0\t560\t20\t-1\t",
            "5\t1\t1\t1\t1\t1\t320\t10\t100\t16\t48\tCtrl+Alt+T\u{200e}",
            "5\t1\t1\t1\t1\t2\t426\t8\t35\t26\t89\tواضغط",
            "5\t1\t1\t1\t1\t3\t472\t14\t45\t12\t90\tVento",
            "5\t1\t1\t1\t1\t4\t523\t10\t26\t17\t92\t\u{200f}افتح",
            "5\t1\t1\t1\t2\t1\t11\t50\t34\t15\t96\tCopy",
            "5\t1\t1\t1\t2\t2\t51\t49\t22\t13\t94\tthe",
            "5\t1\t1\t1\t2\t3\t80\t49\t39\t13\t86\tfile",
        ]);
        let boxes = words(&tsv);
        assert_eq!(boxes.len(), 7);
        // The words of a line share its top and height.
        assert!(
            boxes[..4]
                .iter()
                .all(|b| b.rect.y == 8.0 && b.rect.h == 26.0)
        );
        assert_eq!(
            assemble(&boxes),
            "افتح Vento واضغط Ctrl+Alt+T\nCopy the file"
        );
        // The same line in reading order reads the same.
        let mut reading = boxes.clone();
        reading[..4].reverse();
        assert_eq!(assemble(&reading), assemble(&boxes));
    }

    #[test]
    fn blank_words_and_other_levels_are_skipped() {
        let boxes = words(&tsv(&[
            "5\t1\t1\t1\t1\t1\t0\t0\t10\t10\t95\t \u{200e} ",
            "3\t1\t1\t1\t0\t0\t0\t0\t10\t10\t-1\t",
            "garbage",
        ]));
        assert!(boxes.is_empty());
    }

    /// Reads the picture at `$VENTO_OCR_SAMPLE` as a capture at 1x and
    /// prints the text; by hand, with Tesseract installed.
    #[test]
    #[ignore]
    fn reads_a_sample() {
        let path = std::env::var("VENTO_OCR_SAMPLE").expect("VENTO_OCR_SAMPLE");
        let picture = image::open(path).unwrap();
        let w = (f64::from(picture.width()) * READ_SCALE).round() as u32;
        let h = (f64::from(picture.height()) * READ_SCALE).round() as u32;
        let mut png = Vec::new();
        picture
            .resize_exact(w, h, FilterType::CatmullRom)
            .to_luma8()
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let tsv = run_tesseract(tesseract().expect("tesseract"), &png).unwrap();
        println!("{}", vento_core::screen_text::assemble(&words(&tsv)));
    }

    #[test]
    fn a_selection_lands_on_the_screen_holding_its_centre() {
        let monitor = |x| Monitor {
            frame: Rect {
                x,
                y: 0.0,
                w: 1000.0,
                h: 800.0,
            },
            work: Rect {
                x,
                y: 32.0,
                w: 1000.0,
                h: 768.0,
            },
            scale: 1.0,
        };
        let monitors = [monitor(0.0), monitor(1000.0)];
        let rect = Rect {
            x: 980.0,
            y: 100.0,
            w: 100.0,
            h: 40.0,
        };
        let sel = selection_on(rect, &monitors).unwrap();
        assert_eq!((sel.display_id, sel.display.x), (1, 1000.0));
        assert_eq!(sel.rect, rect);
        // Too thin to hold text.
        let thin = Rect { h: 2.0, ..rect };
        assert!(selection_on(thin, &monitors).is_none());
    }
}
