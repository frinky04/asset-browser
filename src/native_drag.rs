//! Native drag-out on Windows. Rune's `drag_out` is macOS-only, so tiles detect the drag with
//! `on_drag` and hand off here.
//!
//! `DoDragDrop` runs its own modal message loop, and winit panics if a window message re-enters
//! a handler it's already inside, so the session starts from a message posted to a hidden
//! window: it runs from the event loop's `DispatchMessage`, where the nested loop is safe and
//! the window keeps painting. It has to be a posted message. Timers are the lowest priority
//! Windows has, and the mouse moves of the drag itself keep starving them.
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

    use windows::Win32::Foundation::{DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HINSTANCE, HWND, LPARAM, LRESULT, POINT, RPC_E_CHANGED_MODE, S_OK, SIZE, WPARAM};
    use windows::Win32::Graphics::Gdi::{BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateDIBSection, DIB_RGB_COLORS, DeleteObject, HBITMAP};
    use windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, IDataObject};
    use windows::Win32::System::Ole::{DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE, DoDragDrop, IDropSource, IDropSource_Impl, OleInitialize};
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::System::SystemServices::{MK_LBUTTON, MODIFIERKEYS_FLAGS};
    use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, ReleaseCapture, VK_LBUTTON};
    use windows::Win32::UI::Shell::Common::ITEMIDLIST;
    use windows::Win32::UI::Shell::{CLSID_DragDropHelper, IDragSourceHelper, ILFindLastID, ILFree, IShellFolder, SHBindToParent, SHDRAGIMAGE, SHParseDisplayName};
    use windows::Win32::UI::WindowsAndMessaging::{CreateWindowExW, DefWindowProcW, HWND_MESSAGE, PostMessageW, RegisterClassW, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_LBUTTONUP, WNDCLASSW};
    use windows::core::{BOOL, HRESULT, PCWSTR, implement, w};

    use super::{Ended, Request};

    thread_local! {
        static WINDOW: Cell<isize> = const { Cell::new(0) };
        /// A hidden window that receives the message starting a drag.
        static RELAY: Cell<isize> = const { Cell::new(0) };
        static PENDING: RefCell<Option<Request>> = const { RefCell::new(None) };
        static RUNNING: Cell<bool> = const { Cell::new(false) };
    }

    const START_DRAG: u32 = WM_APP + 1;

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
        if RELAY.get() == 0 { RELAY.set(create_relay().map_or(0, |h| h.0 as isize)); }
    }

    fn create_relay() -> windows::core::Result<HWND> {
        let class = w!("AssetBrowserDragRelay");
        // SAFETY: registers a class with a valid procedure and makes a message-only window of it.
        unsafe {
            let instance: HINSTANCE = GetModuleHandleW(None)?.into();
            let _ = RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(relay_proc), hInstance: instance, lpszClassName: class, ..Default::default() });
            CreateWindowExW(WINDOW_EX_STYLE(0), class, None, WINDOW_STYLE(0), 0, 0, 0, 0, Some(HWND_MESSAGE), None, Some(instance), None)
        }
    }

    unsafe extern "system" fn relay_proc(hwnd: HWND, message: u32, w: WPARAM, l: LPARAM) -> LRESULT {
        if message == START_DRAG {
            deliver();
            return LRESULT(0);
        }
        // SAFETY: forwards what Windows sent.
        unsafe { DefWindowProcW(hwnd, message, w, l) }
    }

    /// Starts a drag once the current handler has returned. False if none can start.
    pub fn begin(request: Request) -> bool {
        if WINDOW.get() == 0 || RELAY.get() == 0 || RUNNING.get() {
            let _ = request.done.send(Ended::Failed);
            return false;
        }
        PENDING.set(Some(request));
        // SAFETY: posting to our own window.
        if unsafe { PostMessageW(Some(HWND(RELAY.get() as *mut _)), START_DRAG, WPARAM(0), LPARAM(0)) }.is_err() {
            if let Some(request) = PENDING.take() { let _ = request.done.send(Ended::Failed); }
            return false;
        }
        true
    }

    fn deliver() {
        let Some(request) = PENDING.take() else { return };
        // A quick flick can release before the message arrives; a drag then would end at once.
        // SAFETY: reads global key state.
        let held = unsafe { GetAsyncKeyState(i32::from(VK_LBUTTON.0)) } as u16 & 0x8000 != 0;
        RUNNING.set(true);
        let ended = if held { run(&request).unwrap_or(Ended::Failed) } else { Ended::Cancelled };
        RUNNING.set(false);
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

    /// The same data object Explorer offers when you drag these `files` (all in one folder),
    /// so any program that takes a drag from Explorer takes this one.
    pub(super) fn data_object(files: &[std::path::PathBuf]) -> windows::core::Result<IDataObject> {
        let items = files.iter().map(|p| parse(p)).collect::<windows::core::Result<Vec<_>>>()?;
        // SAFETY: each PIDL is valid for the call.
        unsafe {
            let folder: IShellFolder = SHBindToParent(items[0].0, None)?;
            let children: Vec<*const ITEMIDLIST> = items.iter().map(|p| ILFindLastID(p.0) as *const _).collect();
            folder.GetUIObjectOf(HWND::default(), &children, None)
        }
    }

    /// What a session hands over: the files, with the picture under the pointer attached.
    pub(super) fn prepare(request: &Request) -> windows::core::Result<IDataObject> {
        let data = data_object(&request.files)?;
        if let Some(image) = &request.image { let _ = attach_image(&data, image); }
        Ok(data)
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
        let data = prepare(request)?;
        let source: IDropSource = Source.into();
        // Our window took the mouse on the press; the drag's own window needs it now.
        // SAFETY: releases capture held by this thread.
        let _ = unsafe { ReleaseCapture() };
        let mut effect = DROPEFFECT_NONE;
        // SAFETY: `data` and `source` live for the call.
        let result = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
        Ok(if result == DRAGDROP_S_DROP && effect.0 & DROPEFFECT_COPY.0 != 0 { Ended::Dropped } else { Ended::Cancelled })
    }

    #[cfg(test)]
    mod tests {
        use std::cell::{Cell, RefCell};
        use std::path::PathBuf;
        use std::rc::Rc;

        use windows::Win32::Foundation::{COLORREF, HINSTANCE, LRESULT, POINTL};
        use windows::Win32::System::Com::{DATADIR_GET, DVASPECT_CONTENT, FORMATETC, STGMEDIUM, TYMED_HGLOBAL};
        use windows::Win32::System::LibraryLoader::GetModuleHandleW;
        use windows::Win32::System::Ole::{CF_HDROP, IDropTarget, IDropTarget_Impl, OleInitialize, RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop};
        use windows::Win32::UI::Shell::{DragFinish, DragQueryFileW, HDROP};
        use windows::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetCursorPos, HWND_TOPMOST, LWA_ALPHA, MSG, PM_REMOVE, PeekMessageW, RegisterClassW, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SetLayeredWindowAttributes,
            SetWindowPos, WINDOW_EX_STYLE, WM_DROPFILES, WNDCLASSW, WS_EX_ACCEPTFILES, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP, WS_VISIBLE,
        };

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

        fn hdrop_format() -> FORMATETC {
            FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 }
        }

        #[test]
        fn the_data_object_answers_the_questions_apps_ask_before_accepting() {
            // SAFETY: test thread; OLE is initialized once for it.
            let _ = unsafe { OleInitialize(None) };
            let dir = std::env::temp_dir().join(format!("ab-query-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("a.png");
            std::fs::write(&file, b"x").unwrap();
            let data = data_object(std::slice::from_ref(&file)).unwrap();
            // SAFETY: plain IDataObject queries.
            unsafe {
                assert_eq!(data.QueryGetData(&hdrop_format()), S_OK, "QueryGetData says files are available");
                let formats = data.EnumFormatEtc(DATADIR_GET.0 as u32).unwrap();
                let mut listed = false;
                loop {
                    let mut one = [FORMATETC::default()];
                    let mut got = 0;
                    if formats.Next(&mut one, Some(&mut got)) != S_OK || got == 0 { break }
                    listed |= one[0].cfFormat == CF_HDROP.0;
                }
                assert!(listed, "EnumFormatEtc lists CF_HDROP");
            }
            std::fs::remove_dir_all(&dir).unwrap();
        }

        /// What a modern app does: it registers an IDropTarget and asks before accepting.
        #[implement(IDropTarget)]
        struct Probe { report: Rc<RefCell<Report>> }

        #[derive(Default)]
        struct Report { entered: bool, accepts: bool, dropped: Vec<String> }

        fn names_in(data: &IDataObject) -> Vec<String> {
            // SAFETY: an HGLOBAL medium holding a DROPFILES, released below.
            unsafe {
                let Ok(mut medium) = data.GetData(&hdrop_format()) else { return Vec::new() };
                let names = names_of(HDROP(medium.u.hGlobal.0));
                ReleaseStgMedium(&mut medium);
                names
            }
        }

        unsafe fn names_of(drop: HDROP) -> Vec<String> {
            // SAFETY: the caller's HDROP is valid.
            unsafe {
                (0..DragQueryFileW(drop, u32::MAX, None)).map(|i| {
                    let mut buf = vec![0u16; 1024];
                    let n = DragQueryFileW(drop, i, Some(&mut buf)) as usize;
                    String::from_utf16_lossy(&buf[..n])
                }).collect()
            }
        }

        impl IDropTarget_Impl for Probe_Impl {
            fn DragEnter(&self, data: windows_core::Ref<'_, IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
                let data = data.ok()?;
                // SAFETY: the out pointer OLE passes.
                let accepts = unsafe { data.QueryGetData(&hdrop_format()) } == S_OK;
                let mut report = self.report.borrow_mut();
                (report.entered, report.accepts) = (true, accepts);
                // SAFETY: as above.
                unsafe { *effect = if accepts { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
                Ok(())
            }
            fn DragOver(&self, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
                let accepts = self.report.borrow().accepts;
                // SAFETY: the out pointer OLE passes.
                unsafe { *effect = if accepts { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
                Ok(())
            }
            fn DragLeave(&self) -> windows::core::Result<()> { Ok(()) }
            fn Drop(&self, data: windows_core::Ref<'_, IDataObject>, _: MODIFIERKEYS_FLAGS, _: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
                self.report.borrow_mut().dropped = names_in(data.ok()?);
                // SAFETY: the out pointer OLE passes.
                unsafe { *effect = DROPEFFECT_COPY };
                Ok(())
            }
        }

        /// Drops once the target has seen the drag, with no mouse involved; gives up eventually.
        #[implement(IDropSource)]
        struct TimedSource { calls: Cell<u32>, seen: Rc<dyn Fn() -> bool> }

        impl IDropSource_Impl for TimedSource_Impl {
            fn QueryContinueDrag(&self, _: BOOL, _: MODIFIERKEYS_FLAGS) -> HRESULT {
                self.calls.set(self.calls.get() + 1);
                if (self.seen)() && self.calls.get() > 4 { DRAGDROP_S_DROP } else if self.calls.get() > 600 { DRAGDROP_S_CANCEL } else { S_OK }
            }
            fn GiveFeedback(&self, _: DROPEFFECT) -> HRESULT { DRAGDROP_S_USEDEFAULTCURSORS }
        }

        thread_local! { static LEGACY: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) }; }

        unsafe extern "system" fn probe_proc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
            if msg == WM_DROPFILES {
                // SAFETY: WM_DROPFILES's WPARAM is its HDROP.
                unsafe {
                    let drop = HDROP(w.0 as *mut _);
                    LEGACY.set(names_of(drop));
                    DragFinish(drop);
                }
                return LRESULT(0);
            }
            // SAFETY: forwards what Windows sent.
            unsafe { DefWindowProcW(hwnd, msg, w, l) }
        }

        /// An invisible window under the pointer, so a real drag session finds a real target.
        fn probe_window(accept_files: bool) -> HWND {
            let class = windows::core::w!("AbDropProbe");
            // SAFETY: registers a class with a valid procedure and makes a popup of it.
            unsafe {
                let instance: HINSTANCE = GetModuleHandleW(None).unwrap().into();
                let _ = RegisterClassW(&WNDCLASSW { lpfnWndProc: Some(probe_proc), hInstance: instance, lpszClassName: class, ..Default::default() });
                let mut at = POINT::default();
                GetCursorPos(&mut at).unwrap();
                let extra = if accept_files { WS_EX_ACCEPTFILES } else { WINDOW_EX_STYLE(0) };
                let hwnd = CreateWindowExW(WS_EX_TOPMOST | WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | extra, class, None, WS_POPUP | WS_VISIBLE,
                    at.x - 150, at.y - 150, 300, 300, None, None, Some(instance), None).unwrap();
                SetLayeredWindowAttributes(hwnd, COLORREF(0), 1, LWA_ALPHA).unwrap();
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
                hwnd
            }
        }

        fn staged_file(name: &str) -> PathBuf {
            let dir = std::env::temp_dir().join(format!("ab-real-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let file = dir.join("brick wall.png");
            std::fs::write(&file, b"x").unwrap();
            file
        }

        /// A real OLE drag session against a real window. Puts an invisible window under the
        /// pointer for a moment, so it's run by hand: `cargo test real_drop -- --ignored --test-threads=1`.
        #[test]
        #[ignore]
        fn real_drop_reaches_an_app_with_an_ole_drop_target() {
            // SAFETY: test thread.
            let _ = unsafe { OleInitialize(None) };
            let file = staged_file("ole");
            let report = Rc::new(RefCell::new(Report::default()));
            let hwnd = probe_window(false);
            let target: IDropTarget = Probe { report: report.clone() }.into();
            // SAFETY: `hwnd` is ours and `target` outlives the registration.
            unsafe { RegisterDragDrop(hwnd, &target).unwrap() };
            let seen = report.clone();
            let source: IDropSource = TimedSource { calls: Cell::new(0), seen: Rc::new(move || seen.borrow().entered) }.into();
            // The same preparation a real drag gets, drag picture included.
            let (done, _ended) = std::sync::mpsc::channel();
            let picture = (32, 32, vec![200u8; 32 * 32 * 4]);
            let data = prepare(&Request { files: vec![file.clone()], image: Some(picture), done }).unwrap();
            let mut effect = DROPEFFECT_NONE;
            // SAFETY: everything lives for the call.
            let result = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
            // SAFETY: as registered above.
            unsafe {
                let _ = RevokeDragDrop(hwnd);
                let _ = DestroyWindow(hwnd);
            }
            let report = report.borrow();
            assert!(report.entered, "the target saw the drag");
            assert!(report.accepts, "the target accepted it after asking for files");
            assert_eq!(result, DRAGDROP_S_DROP);
            assert_eq!(report.dropped, [file.to_string_lossy().into_owned()]);
        }

        #[test]
        #[ignore]
        fn real_drop_reaches_an_app_using_wm_dropfiles() {
            // SAFETY: test thread.
            let _ = unsafe { OleInitialize(None) };
            let file = staged_file("legacy");
            let hwnd = probe_window(true);
            let calls = Rc::new(Cell::new(0u32));
            let counted = calls.clone();
            let source: IDropSource = TimedSource { calls: Cell::new(0), seen: Rc::new(move || { counted.set(counted.get() + 1); counted.get() > 40 }) }.into();
            let data = data_object(std::slice::from_ref(&file)).unwrap();
            let mut effect = DROPEFFECT_NONE;
            // SAFETY: everything lives for the call.
            let _ = unsafe { DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect) };
            // SAFETY: drains what the drop posted to this thread's window, then closes it.
            unsafe {
                let mut msg = MSG::default();
                while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() { DispatchMessageW(&msg); }
                let _ = DestroyWindow(hwnd);
            }
            assert_eq!(LEGACY.take(), [file.to_string_lossy().into_owned()]);
        }

        #[test]
        fn releasing_the_button_drops_and_escape_cancels() {
            assert_eq!(next_step(false, MK_LBUTTON.0), S_OK);
            assert_eq!(next_step(false, 0), DRAGDROP_S_DROP);
            assert_eq!(next_step(true, MK_LBUTTON.0), DRAGDROP_S_CANCEL);
        }
    }
}
