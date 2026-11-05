//! Hiding the main window in the tray, and bringing it back.
//!
//! eframe stops updating a window that is hidden (`ViewportCommand::Visible`),
//! which would stop the capture popup and any reaction to the tray icon.  So the
//! window isn't hidden: it is moved out of the screen and taken off the taskbar,
//! which leaves it running.  This is done with the window system directly, so it
//! also works from the tray icon's thread, where no frame is there to run a
//! command.

use std::sync::atomic::{AtomicBool, Ordering};

static VISIBLE: AtomicBool = AtomicBool::new(true);

/// Where the hidden window waits, far outside any screen.
#[cfg(windows)]
pub const HIDDEN_POSITION: f32 = 30_000.;

/// Whether the window is shown, as opposed to hidden in the tray.
pub fn is_visible() -> bool {
    VISIBLE.load(Ordering::Relaxed)
}

#[cfg(windows)]
pub fn set_visible(visible: bool) {
    use windows::Win32::Foundation::{HWND, RECT};
    use windows::Win32::UI::WindowsAndMessaging::{
        FindWindowW, GWL_EXSTYLE, GetSystemMetrics, GetWindowLongPtrW, GetWindowRect,
        SM_CXSCREEN, SM_CYSCREEN, SW_HIDE, SW_SHOW, SW_SHOWNOACTIVATE, SWP_FRAMECHANGED,
        SWP_NOACTIVATE, SWP_NOSIZE, SWP_NOZORDER, SetForegroundWindow, SetWindowLongPtrW,
        SetWindowPos, ShowWindow, WS_EX_APPWINDOW, WS_EX_TOOLWINDOW,
    };
    use windows::core::{PCWSTR, w};

    VISIBLE.store(visible, Ordering::Relaxed);

    let hwnd: HWND = match unsafe { FindWindowW(PCWSTR::null(), w!("Irminsul")) } {
        Ok(hwnd) => hwnd,
        Err(e) => {
            tracing::warn!("unable to find the main window: {e}");
            return;
        }
    };

    unsafe {
        // The taskbar only notices a changed style on a window that is hidden
        // for a moment.
        let _ = ShowWindow(hwnd, SW_HIDE);

        let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        let tool = WS_EX_TOOLWINDOW.0 as isize;
        let app = WS_EX_APPWINDOW.0 as isize;
        let style = if visible {
            (style & !tool) | app
        } else {
            (style | tool) & !app
        };
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, style);

        if visible {
            // Back to the middle of the main screen.
            let mut rect = RECT::default();
            let _ = GetWindowRect(hwnd, &mut rect);
            let x = (GetSystemMetrics(SM_CXSCREEN) - (rect.right - rect.left)) / 2;
            let y = (GetSystemMetrics(SM_CYSCREEN) - (rect.bottom - rect.top)) / 2;
            let _ = SetWindowPos(
                hwnd,
                None,
                x.max(0),
                y.max(0),
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_FRAMECHANGED,
            );
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = SetForegroundWindow(hwnd);
        } else {
            let _ = SetWindowPos(
                hwnd,
                None,
                HIDDEN_POSITION as i32,
                HIDDEN_POSITION as i32,
                0,
                0,
                SWP_NOSIZE | SWP_NOZORDER | SWP_NOACTIVATE | SWP_FRAMECHANGED,
            );
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
    }
}

#[cfg(not(windows))]
pub fn set_visible(_visible: bool) {}
