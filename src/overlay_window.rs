//! A window that shows a picture that is see-through by degrees, over the game.
//!
//! The windows of the program are drawn by its own graphics, which on some
//! machines have no room for how see-through a place is, so the fading of the
//! map was only dark.  This window is the kind of the system itself that takes
//! a picture with such a share for each place (a layered window), which the
//! system puts over whatever is behind it.
//!
//! Clicks go through it, to the game.  Except while it is made to be moved:
//! then it takes the mouse, and is moved by dragging it, made bigger or smaller
//! by its edges, and zoomed with the wheel.  It does not take the focus.

use std::sync::atomic::{AtomicBool, AtomicI32, AtomicIsize, Ordering};
use std::sync::{Arc, Mutex};

/// Smallest the window can be made, in pixels.
pub const MIN_SIZE: i32 = 160;

/// What the program and the window tell each other.
pub struct Shared {
    hwnd: AtomicIsize,
    visible: AtomicBool,
    edit: AtomicBool,
    /// How many notches the wheel was turned while the window was moved (120
    /// to a notch, up is more).
    wheel: AtomicI32,
    /// Where the window is on the screen and how big: x, y, width, height.
    rect: Mutex<[i32; 4]>,
    /// The window was moved or made bigger by the user.
    changed: AtomicBool,
    /// Buttons in the picture (x, y, width, height), which are clicked, not
    /// taken hold of; the number of each is told when it is clicked.
    buttons: Mutex<Vec<[i32; 4]>>,
    /// The button that was clicked, or -1.
    clicked: AtomicI32,
}

pub struct OverlayWindow {
    shared: Arc<Shared>,
}

impl OverlayWindow {
    /// Makes the window, hidden, at this place and of this size.
    pub fn new(rect: [i32; 4]) -> Self {
        let shared = Arc::new(Shared {
            hwnd: AtomicIsize::new(0),
            visible: AtomicBool::new(false),
            edit: AtomicBool::new(false),
            wheel: AtomicI32::new(0),
            rect: Mutex::new(rect),
            changed: AtomicBool::new(false),
            buttons: Mutex::new(Vec::new()),
            clicked: AtomicI32::new(-1),
        });
        platform::start(shared.clone());
        Self { shared }
    }

    pub fn set_visible(&self, visible: bool) {
        if self.shared.visible.swap(visible, Ordering::Relaxed) != visible {
            platform::apply(&self.shared);
        }
    }

    /// Whether the window takes the mouse, to be moved.
    pub fn set_edit(&self, edit: bool) {
        if self.shared.edit.swap(edit, Ordering::Relaxed) != edit {
            platform::apply(&self.shared);
        }
    }

    pub fn rect(&self) -> [i32; 4] {
        self.shared.rect.lock().map(|rect| *rect).unwrap_or([0, 0, 560, 560])
    }

    /// Puts the window at this place and of this size (and shows the picture
    /// that is given next at it).
    pub fn set_rect(&self, rect: [i32; 4]) {
        if let Ok(mut place) = self.shared.rect.lock() {
            *place = rect;
        }
    }

    /// The notches of the wheel since the last call.
    pub fn take_wheel(&self) -> i32 {
        self.shared.wheel.swap(0, Ordering::Relaxed)
    }

    /// Where the buttons are in the picture, for telling a click on one from
    /// taking hold of the window.
    pub fn set_buttons(&self, buttons: Vec<[i32; 4]>) {
        if let Ok(mut place) = self.shared.buttons.lock() {
            *place = buttons;
        }
    }

    /// The button that was clicked since the last call, if one was.
    pub fn take_click(&self) -> Option<usize> {
        usize::try_from(self.shared.clicked.swap(-1, Ordering::Relaxed)).ok()
    }

    /// Whether the user moved or resized the window since the last call.
    pub fn take_changed(&self) -> bool {
        self.shared.changed.swap(false, Ordering::Relaxed)
    }

    /// Shows the picture (blue, green, red and the share that is seen, each of
    /// the three taken by it, row by row from the top) in the window, which is
    /// made as big as the picture.
    pub fn present(&self, picture: &[u8], width: usize, height: usize) {
        platform::present(&self.shared, picture, width, height);
    }
}

#[cfg(windows)]
mod platform {
    use std::ffi::c_void;

    use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
    use windows::Win32::Graphics::Gdi::{
        AC_SRC_ALPHA, AC_SRC_OVER, BI_RGB, BITMAPINFO, BITMAPINFOHEADER, BLENDFUNCTION,
        CreateCompatibleDC, CreateDIBSection, DIB_RGB_COLORS, DeleteDC, DeleteObject, SelectObject,
    };
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DispatchMessageW, GWL_EXSTYLE, GetCursorPos, GetMessageW,
        GetWindowLongPtrW, GetWindowRect, HTCLIENT, IDC_ARROW, IDC_SIZEALL, IDC_SIZENESW,
        IDC_SIZENS, IDC_SIZENWSE, IDC_SIZEWE, LoadCursorW, MA_NOACTIVATE, MSG, PostMessageW,
        RegisterClassExW, SW_HIDE, SW_SHOWNA, SWP_NOACTIVATE, SWP_NOZORDER,
        SetCursor, SetWindowLongPtrW, SetWindowPos, ShowWindow, TranslateMessage,
        ULW_ALPHA, UpdateLayeredWindow, WM_APP, WM_LBUTTONDOWN, WM_LBUTTONUP, WM_MOUSEACTIVATE,
        WM_MOUSEMOVE, WM_MOUSEWHEEL, WM_NCHITTEST, WM_SETCURSOR, WNDCLASSEXW, WS_EX_LAYERED,
        WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_EX_TRANSPARENT, WS_POPUP,
        HWND_TOPMOST,
    };
    use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture};
    use windows::core::{PCWSTR, w};

    use super::*;

    const APPLY: u32 = WM_APP + 1;
    /// How near to an edge a click takes hold of it, in pixels.
    const EDGE: i32 = 10;

    /// What the window is doing with the mouse: moving, or making itself bigger
    /// or smaller by the edges named.
    #[derive(Clone, Copy, Default)]
    struct Drag {
        active: bool,
        left: bool,
        right: bool,
        top: bool,
        bottom: bool,
        moving: bool,
        start_cursor: (i32, i32),
        start_rect: [i32; 4],
    }

    static SHARED: std::sync::OnceLock<Arc<Shared>> = std::sync::OnceLock::new();
    static DRAG: Mutex<Drag> = Mutex::new(Drag {
        active: false,
        left: false,
        right: false,
        top: false,
        bottom: false,
        moving: false,
        start_cursor: (0, 0),
        start_rect: [0; 4],
    });

    pub fn start(shared: Arc<Shared>) {
        let _ = SHARED.set(shared.clone());
        std::thread::spawn(move || unsafe {
            let instance = GetModuleHandleW(None).unwrap_or_default();
            let class = w!("IrminsulMapOverlay");
            let window_class = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(procedure),
                hInstance: instance.into(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&window_class);
            let rect = shared.rect.lock().map(|rect| *rect).unwrap_or([0, 0, 560, 560]);
            let hwnd = CreateWindowExW(
                WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE | WS_EX_TRANSPARENT,
                class,
                w!("Irminsul map"),
                WS_POPUP,
                rect[0],
                rect[1],
                rect[2],
                rect[3],
                None,
                None,
                Some(instance.into()),
                None,
            );
            let Ok(hwnd) = hwnd else {
                tracing::error!("unable to make the window of the map over the game");
                return;
            };
            shared.hwnd.store(hwnd.0 as isize, Ordering::Relaxed);
            // What was asked before the window was there.
            apply(&shared);
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        });
    }

    fn window(shared: &Shared) -> Option<HWND> {
        let value = shared.hwnd.load(Ordering::Relaxed);
        (value != 0).then(|| HWND(value as *mut c_void))
    }

    /// Tells the window's thread to show or hide itself, and to take the mouse
    /// or not, as it was told.
    pub fn apply(shared: &Shared) {
        if let Some(hwnd) = window(shared) {
            unsafe {
                let _ = PostMessageW(Some(hwnd), APPLY, WPARAM(0), LPARAM(0));
            }
        }
    }

    pub fn present(shared: &Shared, picture: &[u8], width: usize, height: usize) {
        let Some(hwnd) = window(shared) else {
            return;
        };
        if width == 0 || height == 0 || picture.len() < width * height * 4 {
            return;
        }
        let rect = shared.rect.lock().map(|rect| *rect).unwrap_or([0, 0, width as i32, height as i32]);
        unsafe {
            let memory = CreateCompatibleDC(None);
            let info = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width as i32,
                    // Negative: the rows go from the top.
                    biHeight: -(height as i32),
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits: *mut c_void = std::ptr::null_mut();
            let Ok(bitmap) = CreateDIBSection(Some(memory), &info, DIB_RGB_COLORS, &mut bits, None, 0)
            else {
                let _ = DeleteDC(memory);
                return;
            };
            if bits.is_null() {
                let _ = DeleteObject(bitmap.into());
                let _ = DeleteDC(memory);
                return;
            }
            std::ptr::copy_nonoverlapping(picture.as_ptr(), bits as *mut u8, width * height * 4);
            let previous = SelectObject(memory, bitmap.into());
            let position = POINT { x: rect[0], y: rect[1] };
            let size = SIZE {
                cx: width as i32,
                cy: height as i32,
            };
            let source = POINT { x: 0, y: 0 };
            let blend = BLENDFUNCTION {
                BlendOp: AC_SRC_OVER as u8,
                BlendFlags: 0,
                SourceConstantAlpha: 255,
                AlphaFormat: AC_SRC_ALPHA as u8,
            };
            let _ = UpdateLayeredWindow(
                hwnd,
                None,
                Some(&position),
                Some(&size),
                Some(memory),
                Some(&source),
                COLORREF(0),
                Some(&blend),
                ULW_ALPHA,
            );
            SelectObject(memory, previous);
            let _ = DeleteObject(bitmap.into());
            let _ = DeleteDC(memory);
        }
        // The size it was made is the size it is.
        if let Ok(mut place) = shared.rect.lock() {
            place[2] = width as i32;
            place[3] = height as i32;
        }
    }

    /// Which edges a point of the window is at.
    fn edges(x: i32, y: i32, width: i32, height: i32) -> (bool, bool, bool, bool) {
        (x < EDGE, x >= width - EDGE, y < EDGE, y >= height - EDGE)
    }

    fn cursor_for(left: bool, right: bool, top: bool, bottom: bool) -> PCWSTR {
        match (left, right, top, bottom) {
            (true, _, true, _) | (_, true, _, true) => IDC_SIZENWSE,
            (_, true, true, _) | (true, _, _, true) => IDC_SIZENESW,
            (true, ..) | (_, true, ..) => IDC_SIZEWE,
            (_, _, true, _) | (_, _, _, true) => IDC_SIZENS,
            _ => IDC_SIZEALL,
        }
    }

    unsafe extern "system" fn procedure(hwnd: HWND, message: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        let Some(shared) = SHARED.get() else {
            return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
        };
        let x = (lparam.0 & 0xffff) as i16 as i32;
        let y = ((lparam.0 >> 16) & 0xffff) as i16 as i32;
        match message {
            APPLY => {
                unsafe {
                    let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    let transparent = WS_EX_TRANSPARENT.0 as isize;
                    let edit = shared.edit.load(Ordering::Relaxed);
                    // Clicks go through, unless it is being moved.
                    let wanted = if edit { style & !transparent } else { style | transparent };
                    if wanted != style {
                        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, wanted);
                    }
                    if shared.visible.load(Ordering::Relaxed) {
                        let _ = ShowWindow(hwnd, SW_SHOWNA);
                        let _ = SetWindowPos(
                            hwnd,
                            Some(HWND_TOPMOST),
                            0,
                            0,
                            0,
                            0,
                            windows::Win32::UI::WindowsAndMessaging::SWP_NOMOVE
                                | windows::Win32::UI::WindowsAndMessaging::SWP_NOSIZE
                                | SWP_NOACTIVATE,
                        );
                    } else {
                        let _ = ShowWindow(hwnd, SW_HIDE);
                    }
                }
                LRESULT(0)
            }
            WM_MOUSEACTIVATE => LRESULT(MA_NOACTIVATE as isize),
            WM_NCHITTEST => LRESULT(HTCLIENT as isize),
            WM_SETCURSOR => {
                unsafe {
                    let mut size = RECT::default();
                    let mut cursor = POINT::default();
                    let _ = GetWindowRect(hwnd, &mut size);
                    let _ = GetCursorPos(&mut cursor);
                    let (left, right, top, bottom) = edges(
                        cursor.x - size.left,
                        cursor.y - size.top,
                        size.right - size.left,
                        size.bottom - size.top,
                    );
                    let name = if shared.edit.load(Ordering::Relaxed) {
                        cursor_for(left, right, top, bottom)
                    } else {
                        IDC_ARROW
                    };
                    if let Ok(cursor) = LoadCursorW(None, name) {
                        SetCursor(Some(cursor));
                    }
                }
                LRESULT(1)
            }
            WM_LBUTTONDOWN if shared.edit.load(Ordering::Relaxed) => {
                // A button: that is a click, not taking hold of the window.
                let hit = shared.buttons.lock().ok().and_then(|buttons| {
                    buttons
                        .iter()
                        .position(|[bx, by, bw, bh]| x >= *bx && x < bx + bw && y >= *by && y < by + bh)
                });
                if let Some(number) = hit {
                    shared.clicked.store(number as i32, Ordering::Relaxed);
                    return LRESULT(0);
                }
                unsafe {
                    let mut size = RECT::default();
                    let mut cursor = POINT::default();
                    let _ = GetWindowRect(hwnd, &mut size);
                    let _ = GetCursorPos(&mut cursor);
                    let (width, height) = (size.right - size.left, size.bottom - size.top);
                    let (left, right, top, bottom) = edges(x, y, width, height);
                    if let Ok(mut drag) = DRAG.lock() {
                        *drag = Drag {
                            active: true,
                            left,
                            right,
                            top,
                            bottom,
                            moving: !(left || right || top || bottom),
                            start_cursor: (cursor.x, cursor.y),
                            start_rect: [size.left, size.top, width, height],
                        };
                    }
                    SetCapture(hwnd);
                }
                LRESULT(0)
            }
            WM_MOUSEMOVE => {
                let drag = DRAG.lock().map(|drag| *drag).unwrap_or_default();
                if drag.active {
                    unsafe {
                        let mut cursor = POINT::default();
                        let _ = GetCursorPos(&mut cursor);
                        let (dx, dy) = (cursor.x - drag.start_cursor.0, cursor.y - drag.start_cursor.1);
                        let [mut left, mut top, width, height] = drag.start_rect;
                        let (mut new_width, mut new_height) = (width, height);
                        if drag.moving {
                            left += dx;
                            top += dy;
                        } else {
                            if drag.right {
                                new_width = (width + dx).max(MIN_SIZE);
                            }
                            if drag.bottom {
                                new_height = (height + dy).max(MIN_SIZE);
                            }
                            if drag.left {
                                new_width = (width - dx).max(MIN_SIZE);
                                left += width - new_width;
                            }
                            if drag.top {
                                new_height = (height - dy).max(MIN_SIZE);
                                top += height - new_height;
                            }
                        }
                        let _ = SetWindowPos(
                            hwnd,
                            None,
                            left,
                            top,
                            new_width,
                            new_height,
                            SWP_NOZORDER | SWP_NOACTIVATE,
                        );
                        if let Ok(mut rect) = shared.rect.lock() {
                            *rect = [left, top, new_width, new_height];
                        }
                        shared.changed.store(true, Ordering::Relaxed);
                    }
                }
                LRESULT(0)
            }
            WM_LBUTTONUP => {
                if let Ok(mut drag) = DRAG.lock() {
                    drag.active = false;
                }
                unsafe {
                    let _ = ReleaseCapture();
                }
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let delta = ((wparam.0 >> 16) & 0xffff) as i16 as i32;
                shared.wheel.fetch_add(delta, Ordering::Relaxed);
                LRESULT(0)
            }
            _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
        }
    }
}

#[cfg(not(windows))]
mod platform {
    use super::*;
    pub fn start(_shared: Arc<Shared>) {}
    pub fn apply(_shared: &Shared) {}
    pub fn present(_shared: &Shared, _picture: &[u8], _width: usize, _height: usize) {}
}
