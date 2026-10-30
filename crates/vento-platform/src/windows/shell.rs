//! The Shell's services: opening files and "Open With", showing files in
//! Explorer, the kind Explorer names, file icons, and the Recycle Bin.
//! Most go through the Shell's COM interfaces, so each call first makes sure
//! COM runs on its thread (a worker's too).

use std::cell::Cell;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use windows::Win32::Foundation::{HWND, SIZE};
use windows::Win32::Graphics::Gdi::{
    BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, DIB_RGB_COLORS, DeleteObject, GetDC, GetDIBits,
    GetObjectW, HBITMAP, HGDIOBJ, ReleaseDC,
};
use windows::Win32::System::Com::{
    CLSCTX_ALL, COINIT_APARTMENTTHREADED, CoCreateInstance, CoInitializeEx, CoTaskMemFree,
    IDataObject,
};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    ASSOC_FILTER_RECOMMENDED, ASSOCF_NONE, ASSOCSTR_EXECUTABLE, AssocQueryStringW, BHID_DataObject,
    FOF_ALLOWUNDO, FOF_NOCONFIRMATION, FOF_NOERRORUI, FOF_SILENT, FOF_WANTNUKEWARNING,
    FOFX_RECYCLEONDELETE, FileOperation, IAssocHandler, IFileOperation, ILFindLastID, ILFree,
    IShellItem, IShellItemImageFactory, SHAssocEnumHandlers, SHCreateItemFromParsingName,
    SHCreateShellItemArrayFromIDLists, SHOpenFolderAndSelectItems, SHParseDisplayName,
    SIIGBF_BIGGERSIZEOK, SIIGBF_ICONONLY, ShellExecuteW,
};
use windows::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
use windows::core::{HSTRING, PCWSTR, PWSTR, w};

use crate::{AppInfo, PlatformError, RgbaImage};

thread_local! {
    /// Whether this thread has asked for COM.
    static COM: Cell<bool> = const { Cell::new(false) };
}

/// Makes sure COM runs on this thread. The main thread has it already
/// (winit's drop targets need OLE); a worker joins a single-threaded
/// apartment, or keeps the one it is in.
fn com() {
    COM.with(|asked| {
        if !asked.replace(true) {
            // SAFETY: plain call; never undone, as the thread may keep
            // using the Shell. A thread already in another apartment says
            // so and keeps it, which serves as well.
            let _ = unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED) };
        }
    });
}

/// `path` for a wide-string parameter.
fn wide(path: &Path) -> HSTRING {
    HSTRING::from(path.as_os_str())
}

/// The string a Shell call handed over, freed.
fn take(text: PWSTR) -> String {
    // SAFETY: `text` is a NUL-terminated string the Shell allocated with the
    // COM allocator; it is read once and freed.
    unsafe {
        let s = text.to_string().unwrap_or_default();
        CoTaskMemFree(Some(text.as_ptr().cast()));
        s
    }
}

fn failed(e: windows::core::Error) -> PlatformError {
    PlatformError::Failed(e.message())
}

/// Moves `path` to the Recycle Bin. Where it cannot go there (a network
/// share, a drive without one), Windows asks before deleting it for good,
/// and nothing goes if the user says no.
pub(super) fn trash(path: &Path) -> Result<(), PlatformError> {
    com();
    // SAFETY: COM calls on live interfaces, on a thread in COM.
    unsafe {
        let item: IShellItem = SHCreateItemFromParsingName(&wide(path), None).map_err(failed)?;
        let op: IFileOperation =
            CoCreateInstance(&FileOperation, None, CLSCTX_ALL).map_err(failed)?;
        op.SetOperationFlags(
            FOF_ALLOWUNDO
                | FOFX_RECYCLEONDELETE
                | FOF_NOCONFIRMATION
                | FOF_WANTNUKEWARNING
                | FOF_SILENT
                | FOF_NOERRORUI,
        )
        .map_err(failed)?;
        op.DeleteItem(&item, None).map_err(failed)?;
        op.PerformOperations().map_err(failed)?;
        if op.GetAnyOperationsAborted().map_err(failed)?.as_bool() {
            return Err(PlatformError::Failed("not moved to the Recycle Bin".into()));
        }
    }
    Ok(())
}

/// Opens each of `paths` with its default app.
pub(super) fn open_paths(paths: &[PathBuf]) -> Result<(), PlatformError> {
    com();
    let mut unopened = Vec::new();
    for path in paths {
        // SAFETY: plain call; the strings live through it.
        let done = unsafe {
            ShellExecuteW(
                None,
                w!("open"),
                &wide(path),
                PCWSTR::null(),
                PCWSTR::null(),
                SW_SHOWNORMAL,
            )
        };
        // Above 32 is success.
        if done.0 as usize <= 32 {
            unopened.push(name_of(path));
        }
    }
    if unopened.is_empty() {
        Ok(())
    } else {
        Err(PlatformError::NoAppOpened(unopened))
    }
}

/// The name a path shows.
fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// The extension of `path` with its dot, as the Shell's associations key
/// on, or `None` for a folder or a file without one.
fn dot_ext(path: &Path) -> Option<String> {
    if path.is_dir() {
        return None;
    }
    path.extension()
        .map(|e| format!(".{}", e.to_string_lossy()))
}

/// The handlers the Shell recommends for `path`'s extension.
fn handlers(path: &Path) -> Vec<IAssocHandler> {
    let Some(ext) = dot_ext(path) else {
        return Vec::new();
    };
    com();
    // SAFETY: COM calls on live interfaces, on a thread in COM.
    unsafe {
        let Ok(found) = SHAssocEnumHandlers(&HSTRING::from(ext), ASSOC_FILTER_RECOMMENDED) else {
            return Vec::new();
        };
        let mut all = Vec::new();
        loop {
            let mut one = [None];
            let mut fetched = 0;
            if found.Next(&mut one, Some(&mut fetched)).is_err() || fetched == 0 {
                break;
            }
            all.extend(one[0].take());
        }
        all
    }
}

/// The program that opens `path` by default.
fn default_program(path: &Path) -> Option<PathBuf> {
    let ext = HSTRING::from(dot_ext(path)?);
    let mut buf = [0u16; 1024];
    let mut len = buf.len() as u32;
    // SAFETY: the buffer and its length go together.
    unsafe {
        AssocQueryStringW(
            ASSOCF_NONE,
            ASSOCSTR_EXECUTABLE,
            &ext,
            w!("open"),
            Some(PWSTR(buf.as_mut_ptr())),
            &mut len,
        )
        .ok()
        .ok()?;
    }
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(PathBuf::from(OsString::from_wide(&buf[..end])))
}

/// The apps that open `path`, its default first, each once. An app is
/// named by the Shell's name for it (a program's path, or a Store app's
/// id), which [`open_with`] takes back.
pub(super) fn apps_for(path: &Path) -> Vec<AppInfo> {
    if !path.exists() {
        return Vec::new();
    }
    let default = default_program(path);
    let mut apps: Vec<AppInfo> = Vec::new();
    for handler in handlers(path) {
        // SAFETY: COM calls on a live interface.
        let (Ok(name), Ok(ui)) = (unsafe { handler.GetName() }, unsafe { handler.GetUIName() })
        else {
            continue;
        };
        let app = PathBuf::from(take(name));
        let name = take(ui);
        if apps.iter().any(|a| a.path == app) {
            continue;
        }
        let is_default = default
            .as_ref()
            .is_some_and(|d| d.as_os_str().eq_ignore_ascii_case(app.as_os_str()));
        apps.push(AppInfo {
            path: app,
            name,
            is_default,
        });
    }
    // Stable: the Shell's order otherwise.
    apps.sort_by_key(|a| !a.is_default);
    apps
}

/// Opens all of `paths` with `app`, one of [`apps_for`]'s, through its
/// handler, which opens Store apps too; a program not among the handlers
/// is run with the paths.
pub(super) fn open_with(paths: &[PathBuf], app: &Path) -> Result<(), PlatformError> {
    let first = paths
        .first()
        .ok_or(PlatformError::NoAppOpened(Vec::new()))?;
    let handler = handlers(first).into_iter().find(|h| {
        // SAFETY: COM call on a live interface.
        unsafe { h.GetName() }.is_ok_and(|n| Path::new(&take(n)) == app)
    });
    match handler {
        Some(handler) => {
            let data = data_object(paths)?;
            // SAFETY: COM call on live interfaces.
            unsafe { handler.Invoke(&data) }.map_err(failed)
        }
        None if app.is_file() => Command::new(app)
            .args(paths)
            .spawn()
            .map(drop)
            .map_err(|error| PlatformError::CouldNotRun {
                program: name_of(app),
                error,
            }),
        None => Err(PlatformError::AppGone(name_of(app))),
    }
}

/// Item ID lists, freed when dropped.
struct Pidls(Vec<*mut ITEMIDLIST>);

impl Drop for Pidls {
    fn drop(&mut self) {
        for &pidl in &self.0 {
            // SAFETY: each came from `SHParseDisplayName` and is freed once.
            unsafe { ILFree(Some(pidl)) };
        }
    }
}

/// The Shell's ID lists of `paths`; the ones it does not know are left out.
fn pidls(paths: &[PathBuf]) -> Pidls {
    com();
    Pidls(
        paths
            .iter()
            .filter_map(|p| {
                let mut pidl = std::ptr::null_mut();
                // SAFETY: the out-pointer is a live local.
                unsafe { SHParseDisplayName(&wide(p), None, &mut pidl, 0, None) }.ok()?;
                Some(pidl)
            })
            .collect(),
    )
}

/// `paths` as one data object, as Explorer hands files to an app.
fn data_object(paths: &[PathBuf]) -> Result<IDataObject, PlatformError> {
    let list = pidls(paths);
    let ids: Vec<*const ITEMIDLIST> = list.0.iter().map(|&p| p.cast_const()).collect();
    // SAFETY: the ID lists live until `list` drops, after the calls.
    unsafe {
        let items = SHCreateShellItemArrayFromIDLists(&ids).map_err(failed)?;
        items.BindToHandler(None, &BHID_DataObject).map_err(failed)
    }
}

/// Shows `paths` selected in Explorer: one window per folder they are in.
pub(super) fn reveal(paths: &[PathBuf]) {
    let mut folders: Vec<(&Path, Vec<PathBuf>)> = Vec::new();
    for path in paths {
        let parent = path.parent().unwrap_or(path);
        match folders.iter_mut().find(|(f, _)| *f == parent) {
            Some((_, items)) => items.push(path.clone()),
            None => folders.push((parent, vec![path.clone()])),
        }
    }
    for (folder, items) in folders {
        let folder_id = pidls(&[folder.to_path_buf()]);
        let item_ids = pidls(&items);
        let Some(&folder_pidl) = folder_id.0.first() else {
            log::warn!("cannot show {} in Explorer", folder.display());
            continue;
        };
        // SAFETY: each item's last ID lies inside its own list, which lives
        // through the call.
        let children: Vec<*const ITEMIDLIST> = item_ids
            .0
            .iter()
            .map(|&p| unsafe { ILFindLastID(p) }.cast_const())
            .collect();
        // SAFETY: as above.
        if let Err(e) = unsafe { SHOpenFolderAndSelectItems(folder_pidl, Some(&children), 0) } {
            log::warn!("cannot show {} in Explorer: {e}", folder.display());
        }
    }
}

/// The kind Explorer names for `path` ("Text Document", "File folder").
pub(super) fn kind_description(path: &Path) -> Option<String> {
    use windows_sys::Win32::UI::Shell::{SHFILEINFOW, SHGFI_TYPENAME, SHGetFileInfoW};

    let name: Vec<u16> = path.as_os_str().encode_wide_nul();
    let mut info: SHFILEINFOW = unsafe { std::mem::zeroed() };
    // SAFETY: `name` is NUL-terminated; `info` is sized.
    let done = unsafe {
        SHGetFileInfoW(
            name.as_ptr(),
            0,
            &mut info,
            size_of::<SHFILEINFOW>() as u32,
            SHGFI_TYPENAME,
        )
    };
    let end = info.szTypeName.iter().position(|&c| c == 0).unwrap_or(0);
    (done != 0 && end > 0).then(|| String::from_utf16_lossy(&info.szTypeName[..end]))
}

/// NUL-terminated wide strings of OS strings.
trait WideNul {
    fn encode_wide_nul(&self) -> Vec<u16>;
}

impl WideNul for std::ffi::OsStr {
    fn encode_wide_nul(&self) -> Vec<u16> {
        use std::os::windows::ffi::OsStrExt;
        self.encode_wide().chain(Some(0)).collect()
    }
}

/// The icon Explorer shows for `path`, `px` pixels square.
pub(super) fn file_icon(path: &Path, px: u32) -> Option<RgbaImage> {
    if px == 0 || std::fs::symlink_metadata(path).is_err() {
        return None;
    }
    com();
    let side = i32::try_from(px).ok()?;
    // SAFETY: COM calls on live interfaces; the bitmap is freed below.
    let bitmap = unsafe {
        let factory: IShellItemImageFactory =
            SHCreateItemFromParsingName(&wide(path), None).ok()?;
        factory
            .GetImage(
                SIZE { cx: side, cy: side },
                SIIGBF_ICONONLY | SIIGBF_BIGGERSIZEOK,
            )
            .ok()?
    };
    let image = rgba_of(bitmap);
    // SAFETY: the bitmap is the Shell's, handed to us to free.
    unsafe {
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
    }
    image
}

/// The pixels of a 32-bit bitmap with premultiplied alpha, as the Shell's
/// images are, as straight RGBA, top row first.
fn rgba_of(bitmap: HBITMAP) -> Option<RgbaImage> {
    let mut bm = BITMAP::default();
    // SAFETY: `bm` is sized for the call.
    let got = unsafe {
        GetObjectW(
            HGDIOBJ(bitmap.0),
            size_of::<BITMAP>() as i32,
            Some((&raw mut bm).cast()),
        )
    };
    if got == 0 || bm.bmWidth <= 0 || bm.bmHeight == 0 {
        return None;
    }
    let (width, height) = (bm.bmWidth, bm.bmHeight.abs());
    let mut info = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width,
            // Top row first.
            biHeight: -height,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut pixels = vec![0u8; width as usize * height as usize * 4];
    // SAFETY: the buffer holds `height` rows of `width` 32-bit pixels.
    let rows = unsafe {
        let dc = GetDC(Some(HWND::default()));
        let rows = GetDIBits(
            dc,
            bitmap,
            0,
            height as u32,
            Some(pixels.as_mut_ptr().cast()),
            &mut info,
            DIB_RGB_COLORS,
        );
        ReleaseDC(Some(HWND::default()), dc);
        rows
    };
    if rows != height {
        return None;
    }
    for p in pixels.as_chunks_mut::<4>().0 {
        let [b, g, r, a] = *p;
        let straight = |c: u8| {
            if a == 0 {
                0
            } else {
                ((u32::from(c) * 255 + u32::from(a) / 2) / u32::from(a)).min(255) as u8
            }
        };
        *p = [straight(r), straight(g), straight(b), a];
    }
    Some(RgbaImage {
        width: width as u32,
        height: height as u32,
        pixels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_folder_has_an_icon_and_a_kind() {
        let dir = std::env::temp_dir();
        let icon = file_icon(&dir, 48).expect("a folder icon");
        assert_eq!(icon.pixels.len(), (icon.width * icon.height * 4) as usize);
        assert!(icon.width >= 32);
        assert!(icon.pixels.as_chunks::<4>().0.iter().any(|p| p[3] > 0));
        assert!(kind_description(&dir).is_some());
        assert!(file_icon(&dir.join("vento-no-such-file"), 48).is_none());
    }

    #[test]
    fn text_files_open_with_notepad_among_others() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("a.txt");
        std::fs::write(&file, "x").unwrap();
        let apps = apps_for(&file);
        assert!(!apps.is_empty());
        assert!(apps.iter().filter(|a| a.is_default).count() <= 1);
        assert!(
            apps.iter()
                .any(|a| a.path.to_string_lossy().to_lowercase().contains("notepad")),
            "{apps:?}"
        );
        assert!(apps_for(dir.path()).is_empty());
    }

    #[test]
    fn pictures_have_apps_too() {
        let dir = tempfile::tempdir().unwrap();
        for ext in ["jpg", "png", "gif"] {
            let file = dir.path().join(format!("a.{ext}"));
            std::fs::write(&file, "x").unwrap();
            let apps = apps_for(&file);
            assert!(!apps.is_empty(), "{ext}: {apps:?}");
        }
    }

    #[test]
    #[ignore = "puts a file in the Recycle Bin"]
    fn a_file_goes_to_the_recycle_bin() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("vento-trash-test.txt");
        std::fs::write(&file, "x").unwrap();
        trash(&file).unwrap();
        assert!(!file.exists());
    }
}
