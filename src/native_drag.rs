//! Native drag-out on Windows. Rune's `drag_out` is macOS-only, so tiles detect the drag with
//! `on_drag` and hand off here.
//!
//! `DoDragDrop` runs its own modal message loop, and winit panics if a window message re-enters
//! a handler it's already inside. A zero-length thread timer gets out of the handler first: its
//! callback runs from the event loop's `DispatchMessage`, where the nested loop is safe and the
//! window keeps painting throughout.
//!
//! Only `DROPEFFECT_COPY` is offered, so a drop can never move or delete the file.

use std::path::PathBuf;
use std::sync::mpsc::Sender;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ended {
    Dropped,
    Cancelled,
    Failed,
}

pub struct Request {
    pub files: Vec<PathBuf>,
    /// Straight-alpha RGBA for the picture under the pointer.
    pub image: Option<(u32, u32, Vec<u8>)>,
    pub done: Sender<Ended>,
}

#[cfg(windows)]
pub use win::{begin, set_window};

#[cfg(not(windows))]
pub fn begin(request: Request) -> bool {
    let _ = request.done.send(Ended::Failed);
    false
}

#[cfg(not(windows))]
pub fn set_window(_: isize) {}

#[cfg(windows)]
mod win {
    use std::cell::{Cell, RefCell};
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND, LPARAM, POINT, RPC_E_CHANGED_MODE, S_OK, SIZE, WPARAM};
    use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, HBITMAP};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IDataObject};
    use windows::Win32::System::Ole::{DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize};
    use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, ReleaseCapture, VK_LBUTTON};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{CLSID_DragDropHelper, IDragSourceHelper, ILClone, ILFindLastID, ILFree, ILRemoveLastID, SHCreateDataObject, SHDRAGIMAGE, SHParseDisplayName};
    use windows::Win32::UI::WindowsAndMessaging::{GetCursorPos, KillTimer, PostMessageW, SetTimer, WM_LBUTTONUP};
    use windows::core::{BOOL, HRESULT, PCWSTR, implement};

    use super::{Ended, Request};

    thread_local! {
        static WINDOW: Cell<isize> = const { Cell::new(0) };
        static PENDING: RefCell<Option<Request>> = const { RefCell::new(None) };
    }

    #[implement(IDropSource)]
    struct Source;

    /// What a drag should do next, from the keys and buttons held now.
    pub(super) fn next_step(escape: bool, keys: u32) -> HRESULT {
        if escape { return DRAGDROP_S_CANCEL; }
        if keys & MK_LBUTTON.0 == 0 { return DRAGDROP_S_DROP; }
        S_OK
    }

    impl IDropSource_Impl for Source_Impl {
        fn QueryContinueDrag(&self, escape: BOOL, keys: MODIFIERKEYS_FLAGS) -> HRESULT {
            next_step(escape.as_bool(), keys.0)
        }
        fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT {
            DRAGDROP_S_USEDEFAULTCURSORS
        }
    }

    /// The window to drive; call on the UI thread.
    pub fn set_window(hwnd: isize) {
        WINDOW.set(hwnd);
    }

    /// Starts a drag once the current handler has returned. False if none can start.
    pub fn begin(request: Request) -> bool {
        if WINDOW.get() == 0 {
            let _ = request.done.send(Ended::Failed);
            return false;
        }
        PENDING.set(Some(request));
        // SAFETY: a thread timer with a valid callback; killed in the callback.
        unsafe { SetTimer(None, 0, 0, Some(fire)) };
        true
    }

    unsafe extern "system" fn fire(_: HWND, _: u32, id: usize, _: u32) {
        // SAFETY: the timer this callback was started by.
        let _ = unsafe { KillTimer(None, id) };
        let Some(request) = PENDING.take() else { return };
        // A quick flick can release before the timer fires; a drag then would end at once.
        // SAFETY: reads global key state.
        let held = unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) } as u16 & 0x8000 != 0;
        let ended = if held { run(&request).unwrap_or(Ended::Failed) } else { Ended::Cancelled };
        let _ = request.done.send(ended);
        // The system took the mouse, so the window never saw the button go up; without one
        // the tile stays pressed.
        let hwnd = HWND(WINDOW.get() as *mut _);
        // SAFETY: posting to our own window.
        let _ = unsafe { PostMessageW(Some(hwnd), WM_LBUTTONUP, WPARAM(0), LPARAM(0)) };
    }

    struct Pidl(*mut ITEMIDLIST);

    impl Drop for Pidl {
        fn drop(&mut self) {
            // SAFETY: allocated by the shell and owned here.
            unsafe { ILFree(Some(self.0)) };
        }
    }

    fn parse(path: &Path) -> windows::core::Result<Pidl> {
        let wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        let mut pidl = std::ptr::null_mut();
        // SAFETY: `wide` is NUL-terminated and outlives the call.
        unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None)? };
        Ok(Pidl(pidl))
    }

    /// A shell data object offering `files` (all in one folder) to any drop target.
    pub(super) fn data_object(files: &[std::path::PathBuf]) -> windows::core::Result<IDataObject> {
        let items = files.iter().map(|p| parse(p)).collect::<windows::core::Result<Vec<_>>>()?;
        // SAFETY: each PIDL is valid for the call; the parent clone is owned by `parent`.
        unsafe {
            let parent = Pidl(ILClone(items[0].0));
            let _ = ILRemoveLastID(Some(parent.0));
            let children: Vec<*const ITEMIDLIST> = items.iter().map(|p| ILFindLastID(p.0) as *const _).collect();
            SHCreateDataObject(Some(parent.0), Some(&children), None)
        }
    }

    /// Top-down, premultiplied BGRA: what `InitializeFromBitmap` wants.
    fn bitmap(width: u32, height: u32, rgba: &[u8]) -> windows::core::Result<HBITMAP> {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        // SAFETY: `info` describes a 32-bit DIB and `bits` receives its buffer.
        let map = unsafe { CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)? };
        // SAFETY: the section is `width * height` 4-byte pixels and `rgba` is checked to match.
        let out = unsafe { std::slice::from_raw_parts_mut(bits.cast::<u8>(), rgba.len()) };
        for (d, s) in out.chunks_exact_mut(4).zip(rgba.chunks_exact(4)) {
            let a = u32::from(s[3]);
            d[0] = (u32::from(s[2]) * a / 255) as u8;
            d[1] = (u32::from(s[1]) * a / 255) as u8;
            d[2] = (u32::from(s[0]) * a / 255) as u8;
            d[3] = s[3];
        }
        Ok(map)
    }

    fn attach_image(data: &IDataObject, (width, height, rgba): &(u32, u32, Vec<u8>)) -> windows::core::Result<()> {
        if rgba.len() != (*width as usize) * (*height as usize) * 4 { return Ok(()); }
        // SAFETY: standard COM creation of the shell's drag helper.
        let helper: IDragSourceHelper = unsafe { CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER)? };
        let map = bitmap(*width, *height, rgba)?;
        let image = SHDRAGIMAGE {
            sizeDragImage: SIZE { cx: *width as i32, cy: *height as i32 },
            ptOffset: POINT { x: *width as i32 / 2, y: *height as i32 / 2 },
            hbmpDragImage: map,
            crColorKey: windows::Win32::Foundation::COLORREF(0xFFFF_FFFF),
        };
        // SAFETY: on success the helper owns the bitmap; on failure it's freed here.
        unsafe {
            if let Err(e) = helper.InitializeFromBitmap(&image, data) {
                let _ = DeleteObject(map.into());
                return Err(e);
            }
        }
        Ok(())
    }

    fn run(request: &Request) -> windows::core::Result<Ended> {
        // SAFETY: called on the UI thread, where winit already initialized OLE.
        let init = unsafe { OleInitialize(None) };
        if let Err(e) = init && e.code() != RPC_E_CHANGED_MODE { return Err(e); }
        let data = data_object(&request.files)?;
        if let Some(image) = &request.image { let _ = attach_image(&data, image); }
        let source: IDropSource = Source.into();
        // Our window took the mouse on the press; the drag's own window needs it now.
        // SAFETY: releases capture held by this thread.
        let _ = unsafe { ReleaseCapture() };
        let mut point = POINT::default();
        // SAFETY: writes to a local.
        let _ = unsafe { GetCursorPos(&mut point) };
        let mut effect = DROPEFFECT_NONE;
        // SAFETY: `data` and `source` live for the call.
        let result = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
        Ok(if result == DRAGDROP_S_DROP && effect.0 & DROPEFFECT_COPY.0 != 0 { Ended::Dropped } else { Ended::Cancelled })
    }

    #[cfg(test)]
    mod tests {
        use windows::Win32::System::Com::{DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL};
        use windows::Win32::System::Ole::{CF_HDROP, OleInitialize, ReleaseStgMedium};
        use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};

        use super::*;

        #[test]
        fn the_data_object_offers_exactly_the_staged_paths_as_a_file_drop() {
            // SAFETY: test thread; OLE is initialized once for it.
            let _ = unsafe { OleInitialize(None) };
            let dir = std::env::temp_dir().join(format!("ab-drag-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let files = [dir.join("one tex.png"), dir.join("two.mtl")];
            for f in &files { std::fs::write(f, b"x").unwrap(); }
            let data = data_object(&files).unwrap();
            let format = FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
            // SAFETY: standard IDataObject read; the medium is released below.
            let mut medium: STGMEDIUM = unsafe { data.GetData(&format).unwrap() };
            // SAFETY: HGLOBAL medium asked for above, holding a DROPFILES.
            let names: Vec<String> = unsafe {
                let drop = HDROP(medium.u.hGlobal.0);
                let count = DragQueryFileW(drop, u32::MAX, None);
                (0..count).map(|i| {
                    let mut buf = vec![0u16; 1024];
                    let n = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
                    String::from_utf16_lossy(&buf[..n])
                }).collect()
            };
            // SAFETY: releases the medium read above.
            unsafe { ReleaseStgMedium(&mut medium) };
            std::fs::remove_dir_all(&dir).unwrap();
            let wanted: Vec<String> = files.iter().map(|f| f.to_string_lossy().into_owned()).collect();
            assert_eq!(names, wanted);
        }

        #[test]
        fn releasing_the_button_drops_and_escape_cancels() {
            assert_eq!(next_step(false, MK_LBUTTON.0), S_OK);
            assert_eq!(next_step(false, 0), DRAGDROP_S_DROP);
            assert_eq!(next_step(true, MK_LBUTTON.0), DRAGDROP_S_CANCEL);
        }
    }
}
