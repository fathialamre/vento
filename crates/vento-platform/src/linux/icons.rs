//! File icons as Files shows them: gio names the icon, the user's icon
//! theme (freedesktop icon theme spec) supplies the PNG or SVG, drawn to
//! RGBA8 pixels.

use std::cell::RefCell;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use gio::prelude::*;

use crate::RgbaImage;

/// One `[section]` of an `index.theme`: a folder of icons of one size.
#[derive(Debug, Clone, PartialEq)]
struct Dir {
    /// Relative to the theme's folder, as `Directories=` lists it.
    path: String,
    size: u32,
    scale: u32,
    kind: DirKind,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum DirKind {
    Fixed,
    Scalable { min: u32, max: u32 },
    Threshold(u32),
}

impl Dir {
    /// How far its icons are from `side` pixels: 0 when one fits as is.
    /// Smaller icons count double, since scaling up blurs and scaling
    /// down does not.
    fn distance(&self, side: u32) -> u32 {
        let (lo, hi) = match self.kind {
            DirKind::Fixed => (self.size, self.size),
            DirKind::Scalable { min, max } => (min, max),
            DirKind::Threshold(t) => (self.size.saturating_sub(t), self.size + t),
        };
        let (lo, hi) = (lo * self.scale, hi * self.scale);
        if side < lo {
            lo - side
        } else if side > hi {
            (side - hi) * 2
        } else {
            0
        }
    }
}

/// An icon theme: where it lives, the themes it falls back to, and its
/// folders that can hold file icons.
#[derive(Debug, Clone, PartialEq, Default)]
struct Theme {
    inherits: Vec<String>,
    dirs: Vec<Dir>,
}

/// The `Context`s file icons come from; actions, status and the like are
/// skipped, so a lookup stats far fewer folders.
const FILE_CONTEXTS: [&str; 5] = ["MimeTypes", "Places", "Devices", "Applications", "Legacy"];

/// Reads an `index.theme`. Folders without a usable `Size` are dropped.
fn parse_index(text: &str) -> Theme {
    let mut theme = Theme::default();
    let mut listed = Vec::new();
    let mut section = String::new();
    let mut keys: Vec<(String, Vec<(String, String)>)> = Vec::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.to_owned();
            keys.push((section.clone(), Vec::new()));
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let (k, v) = (k.trim(), v.trim());
        if section == "Icon Theme" {
            let list = || v.split(',').map(str::trim).filter(|s| !s.is_empty());
            match k {
                "Inherits" => theme.inherits = list().map(str::to_owned).collect(),
                "Directories" | "ScaledDirectories" => listed.extend(list().map(str::to_owned)),
                _ => {}
            }
        } else if let Some((_, pairs)) = keys.last_mut() {
            pairs.push((k.to_owned(), v.to_owned()));
        }
    }
    for path in listed {
        let Some((_, pairs)) = keys.iter().find(|(s, _)| *s == path) else {
            continue;
        };
        let get = |key: &str| {
            pairs
                .iter()
                .find(|(k, _)| k == key)
                .map(|(_, v)| v.as_str())
        };
        let num = |key: &str| get(key).and_then(|v| v.parse::<u32>().ok());
        if !get("Context").is_some_and(|c| FILE_CONTEXTS.contains(&c)) {
            continue;
        }
        let Some(size) = num("Size").filter(|s| *s > 0) else {
            continue;
        };
        let kind = match get("Type") {
            Some("Fixed") => DirKind::Fixed,
            Some("Scalable") => DirKind::Scalable {
                min: num("MinSize").unwrap_or(size),
                max: num("MaxSize").unwrap_or(size),
            },
            _ => DirKind::Threshold(num("Threshold").unwrap_or(2)),
        };
        if theme.dirs.iter().any(|d| d.path == path) {
            continue;
        }
        theme.dirs.push(Dir {
            path,
            size,
            scale: num("Scale").unwrap_or(1).max(1),
            kind,
        });
    }
    theme
}

/// Folders icon themes live in, most personal first.
fn base_dirs() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".local/share"));
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let mut dirs = vec![home.join(".icons"), data_home.join("icons")];
    dirs.extend(data_dirs.split(':').map(|d| Path::new(d).join("icons")));
    dirs
}

/// A theme ready to search: its folder in every base dir that has it.
#[derive(Debug)]
struct Loaded {
    roots: Vec<PathBuf>,
    theme: Theme,
}

/// `name` and every theme it inherits from, breadth first, then hicolor
/// (which every theme falls back to). Themes that aren't installed are
/// skipped.
fn load_chain(name: &str, bases: &[PathBuf]) -> Vec<Loaded> {
    let mut queue = VecDeque::from([name.to_owned()]);
    let mut seen: Vec<String> = Vec::new();
    let mut chain = Vec::new();
    let hicolor_last =
        |seen: &[String]| (!seen.iter().any(|s| s == "hicolor")).then(|| "hicolor".to_owned());
    while let Some(name) = queue.pop_front().or_else(|| hicolor_last(&seen)) {
        if seen.contains(&name) {
            continue;
        }
        seen.push(name.clone());
        let roots: Vec<PathBuf> = bases
            .iter()
            .map(|b| b.join(&name))
            .filter(|r| r.is_dir())
            .collect();
        let Some(text) = roots
            .iter()
            .find_map(|r| std::fs::read_to_string(r.join("index.theme")).ok())
        else {
            continue;
        };
        let theme = parse_index(&text);
        queue.extend(theme.inherits.iter().cloned());
        chain.push(Loaded { roots, theme });
    }
    chain
}

/// The best file for any of `names` (in order) at `side` pixels: the first
/// theme of `chain` that has one wins, so a theme's generic icon beats a
/// more specific one from a theme it falls back to.
fn lookup(chain: &[Loaded], names: &[String], side: u32) -> Option<PathBuf> {
    for loaded in chain {
        let mut dirs: Vec<&Dir> = loaded.theme.dirs.iter().collect();
        // Nearest first; at equal distance the larger icon, to scale down.
        dirs.sort_by_key(|d| (d.distance(side), std::cmp::Reverse(d.size * d.scale)));
        for name in names {
            for dir in &dirs {
                for root in &loaded.roots {
                    for ext in ["png", "svg"] {
                        let file = root.join(&dir.path).join(format!("{name}.{ext}"));
                        if file.is_file() {
                            return Some(file);
                        }
                    }
                }
            }
        }
    }
    names.iter().find_map(|name| {
        ["png", "svg"]
            .iter()
            .map(|ext| PathBuf::from(format!("/usr/share/pixmaps/{name}.{ext}")))
            .find(|f| f.is_file())
    })
}

/// The user's icon theme: GNOME's `icon-theme` setting, else Adwaita.
fn theme_name() -> String {
    const SCHEMA: &str = "org.gnome.desktop.interface";
    let installed = gio::SettingsSchemaSource::default()
        .and_then(|s| s.lookup(SCHEMA, true))
        .is_some();
    if installed {
        let name = gio::Settings::new(SCHEMA).string("icon-theme");
        if !name.is_empty() {
            return name.to_string();
        }
    }
    "Adwaita".into()
}

thread_local! {
    /// The theme chain, reloaded when the user picks another theme.
    static CHAIN: RefCell<Option<(String, Vec<Loaded>)>> = const { RefCell::new(None) };
}

/// The icon file for `names` from the user's theme.
fn themed_file(names: &[String], side: u32) -> Option<PathBuf> {
    let name = theme_name();
    CHAIN.with(|cell| {
        let mut cell = cell.borrow_mut();
        if cell.as_ref().is_none_or(|(n, _)| *n != name) {
            *cell = Some((name.clone(), load_chain(&name, &base_dirs())));
        }
        lookup(&cell.as_ref()?.1, names, side)
    })
}

/// The icon gio gives `path`: a file of its own, or theme icon names.
enum Named {
    File(PathBuf),
    Names(Vec<String>),
}

fn icon_of(path: &Path) -> Option<Named> {
    let info = gio::File::for_path(path)
        .query_info(
            gio::FILE_ATTRIBUTE_STANDARD_ICON,
            gio::FileQueryInfoFlags::NONE,
            gio::Cancellable::NONE,
        )
        .ok()?;
    let icon = info.icon()?;
    if let Some(themed) = icon.downcast_ref::<gio::ThemedIcon>() {
        let names = themed.names().iter().map(|n| n.to_string()).collect();
        return Some(Named::Names(names));
    }
    let file = icon.downcast_ref::<gio::FileIcon>()?.file().path()?;
    Some(Named::File(file))
}

/// `img` fitted inside a `side`×`side` square, centred, transparent around.
fn fit_square(img: &image::RgbaImage, side: u32) -> RgbaImage {
    let (w, h) = img.dimensions();
    let scale = side as f64 / w.max(h) as f64;
    let (fw, fh) = (
        ((w as f64 * scale).round() as u32).clamp(1, side),
        ((h as f64 * scale).round() as u32).clamp(1, side),
    );
    let fitted = if (fw, fh) == (w, h) {
        img.clone()
    } else {
        image::imageops::resize(img, fw, fh, image::imageops::FilterType::CatmullRom)
    };
    let mut square = image::RgbaImage::new(side, side);
    image::imageops::overlay(
        &mut square,
        &fitted,
        i64::from((side - fw) / 2),
        i64::from((side - fh) / 2),
    );
    RgbaImage {
        width: side,
        height: side,
        pixels: square.into_raw(),
    }
}

/// An SVG drawn to fill a `side`×`side` square, keeping its proportions.
fn render_svg(data: &[u8], side: u32) -> Option<RgbaImage> {
    use resvg::{tiny_skia, usvg};
    let tree = usvg::Tree::from_data(data, &usvg::Options::default()).ok()?;
    let size = tree.size();
    let scale = side as f32 / size.width().max(size.height());
    let dx = (side as f32 - size.width() * scale) / 2.0;
    let dy = (side as f32 - size.height() * scale) / 2.0;
    let mut pixmap = tiny_skia::Pixmap::new(side, side)?;
    let transform = tiny_skia::Transform::from_scale(scale, scale).post_translate(dx, dy);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let pixels = pixmap
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    Some(RgbaImage {
        width: side,
        height: side,
        pixels,
    })
}

/// The image file at `file` (PNG, SVG or anything `image` reads) at
/// `side`×`side` pixels.
fn render(file: &Path, side: u32) -> Option<RgbaImage> {
    let data = std::fs::read(file).ok()?;
    let is_svg = file
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("svg") || e.eq_ignore_ascii_case("svgz"));
    if is_svg {
        return render_svg(&data, side);
    }
    let img = image::load_from_memory(&data).ok()?.to_rgba8();
    Some(fit_square(&img, side))
}

/// The icon Files shows for `path`, `side`×`side` pixels. `None` if `path`
/// doesn't exist or no icon is found.
pub(crate) fn file_icon(path: &Path, side: u32) -> Option<RgbaImage> {
    if side == 0 {
        return None;
    }
    let file = match icon_of(path)? {
        Named::File(file) => file,
        Named::Names(names) => themed_file(&names, side)?,
    };
    render(&file, side)
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = "\
[Icon Theme]
Name=Test
Inherits=Parent, hicolor
Directories=16x16/mimetypes,48x48/mimetypes,48x48@2x/places,scalable/mimetypes,48x48/actions
ScaledDirectories=48x48@2x/places

[16x16/mimetypes]
Context=MimeTypes
Size=16
Type=Fixed

[48x48/mimetypes]
Context=MimeTypes
Size=48
Type=Fixed

[48x48@2x/places]
Context=Places
Size=48
Scale=2
Type=Fixed

[scalable/mimetypes]
Context=MimeTypes
Size=128
MinSize=8
MaxSize=512
Type=Scalable

[48x48/actions]
Context=Actions
Size=48
";

    #[test]
    fn index_files_give_inherits_and_file_icon_folders() {
        let t = parse_index(INDEX);
        assert_eq!(t.inherits, ["Parent", "hicolor"]);
        let paths: Vec<&str> = t.dirs.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "16x16/mimetypes",
                "48x48/mimetypes",
                "48x48@2x/places",
                "scalable/mimetypes"
            ],
            "actions are skipped and a folder listed twice is kept once"
        );
        assert_eq!(t.dirs[2].scale, 2);
        assert_eq!(t.dirs[3].kind, DirKind::Scalable { min: 8, max: 512 });
    }

    #[test]
    fn distances_prefer_fitting_then_larger_icons() {
        let fixed = |size, scale| Dir {
            path: String::new(),
            size,
            scale,
            kind: DirKind::Fixed,
        };
        assert_eq!(fixed(48, 1).distance(48), 0);
        assert_eq!(fixed(48, 1).distance(40), 8);
        assert_eq!(fixed(32, 1).distance(40), 16, "scaling up counts double");
        assert_eq!(fixed(24, 2).distance(48), 0, "scale multiplies the size");
        let threshold = Dir {
            kind: DirKind::Threshold(2),
            ..fixed(32, 1)
        };
        assert_eq!(threshold.distance(31), 0);
        let scalable = Dir {
            kind: DirKind::Scalable { min: 8, max: 512 },
            ..fixed(128, 1)
        };
        assert_eq!(scalable.distance(40), 0);
    }

    /// A theme folder with `files` (relative paths) and `index`.
    fn theme_dir(base: &Path, name: &str, index: &str, files: &[&str]) {
        let root = base.join(name);
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.theme"), index).unwrap();
        for f in files {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"").unwrap();
        }
    }

    #[test]
    fn lookups_pick_the_nearest_size_and_fall_back_through_themes() {
        let base = tempfile::tempdir().unwrap();
        theme_dir(
            base.path(),
            "Test",
            INDEX,
            &[
                "16x16/mimetypes/text-plain.png",
                "48x48/mimetypes/text-plain.png",
            ],
        );
        theme_dir(
            base.path(),
            "Parent",
            "[Icon Theme]\nDirectories=32x32/places\n\n[32x32/places]\nContext=Places\nSize=32\n",
            &["32x32/places/folder.png"],
        );
        theme_dir(
            base.path(),
            "hicolor",
            "[Icon Theme]\nDirectories=48x48/mimetypes\n\n[48x48/mimetypes]\nContext=MimeTypes\nSize=48\nType=Fixed\n",
            &["48x48/mimetypes/text-x-generic.png"],
        );
        let chain = load_chain("Test", &[base.path().to_path_buf()]);
        let names = |n: &[&str]| n.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let rel = |p: PathBuf| p.strip_prefix(base.path()).unwrap().to_owned();

        assert_eq!(
            rel(lookup(&chain, &names(&["text-plain"]), 40).unwrap()),
            Path::new("Test/48x48/mimetypes/text-plain.png")
        );
        assert_eq!(
            rel(lookup(&chain, &names(&["text-plain"]), 16).unwrap()),
            Path::new("Test/16x16/mimetypes/text-plain.png")
        );
        assert_eq!(
            rel(lookup(&chain, &names(&["folder-music", "folder"]), 40).unwrap()),
            Path::new("Parent/32x32/places/folder.png"),
            "a fallback name in an inherited theme"
        );
        assert_eq!(
            rel(lookup(&chain, &names(&["text-x-generic"]), 40).unwrap()),
            Path::new("hicolor/48x48/mimetypes/text-x-generic.png")
        );
        assert_eq!(lookup(&chain, &names(&["no-such-icon-anywhere"]), 40), None);
    }

    #[test]
    fn themes_that_inherit_in_a_loop_are_loaded_once() {
        let base = tempfile::tempdir().unwrap();
        theme_dir(base.path(), "A", "[Icon Theme]\nInherits=B\n", &[]);
        theme_dir(base.path(), "B", "[Icon Theme]\nInherits=A\n", &[]);
        assert_eq!(load_chain("A", &[base.path().to_path_buf()]).len(), 2);
    }

    #[test]
    fn pngs_are_fitted_into_a_square_keeping_proportions() {
        let wide = image::RgbaImage::from_pixel(40, 20, image::Rgba([255, 0, 0, 255]));
        let out = fit_square(&wide, 20);
        assert_eq!(
            (out.width, out.height, out.pixels.len()),
            (20, 20, 20 * 20 * 4)
        );
        let alpha = |x: u32, y: u32| out.pixels[((y * 20 + x) * 4 + 3) as usize];
        assert_eq!(alpha(10, 10), 255, "the middle row is drawn");
        assert_eq!(alpha(10, 0), 0, "above it stays clear");
    }

    #[test]
    fn svgs_are_drawn_unpremultiplied() {
        let svg = br#"<svg xmlns="http://www.w3.org/2000/svg" width="10" height="10">
            <rect width="10" height="10" fill="rgb(255,0,0)" fill-opacity="0.5"/></svg>"#;
        let out = render_svg(svg, 8).unwrap();
        assert_eq!((out.width, out.height), (8, 8));
        let px = &out.pixels[0..4];
        assert!(
            px[0] >= 250 && px[1] == 0 && (126..=129).contains(&px[3]),
            "{px:?}"
        );
    }

    #[test]
    fn a_missing_path_or_zero_size_has_no_icon() {
        let dir = tempfile::tempdir().unwrap();
        assert!(file_icon(&dir.path().join("gone"), 32).is_none());
        assert!(file_icon(dir.path(), 0).is_none());
    }
}
