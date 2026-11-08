//! A key that is heard while the game is in front, to show and move the map over
//! the game.
//!
//! It is heard with a hook on the keyboard, which can also keep the key from
//! the game: Caps Lock, which is the key at first, would otherwise turn capital
//! letters on and off in the game's chat at each press.  The hook does that only
//! while the game is in front; for any other program the key is as it was.
//!
//! What the hook hears is put in a list, with the time, for the window to take
//! when it is drawn: a tap shows or hides the map, and a key held while it is
//! shown is for moving it.

use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU32, Ordering};
use std::sync::{Mutex, Once, OnceLock};
use std::time::{Duration, Instant};

use egui::Context;

/// Caps Lock.
pub const DEFAULT_KEY: u32 = 0x14;

/// What is in front, as the hook is told.
pub const FRONT_OTHER: u8 = 0;
pub const FRONT_GAME: u8 = 1;
pub const FRONT_US: u8 = 2;

static KEY: AtomicU32 = AtomicU32::new(DEFAULT_KEY);
static ENABLED: AtomicBool = AtomicBool::new(false);
static FRONT: AtomicU8 = AtomicU8::new(FRONT_OTHER);
/// Whether the key is down (the keyboard repeats a key that is held).
static HELD: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    Down,
    Up,
}

#[derive(Clone, Copy, Debug)]
pub struct Event {
    pub edge: Edge,
    pub at: Instant,
}

fn events() -> &'static Mutex<Vec<Event>> {
    static EVENTS: OnceLock<Mutex<Vec<Event>>> = OnceLock::new();
    EVENTS.get_or_init(Default::default)
}

fn wake() -> &'static Mutex<Option<Context>> {
    static WAKE: OnceLock<Mutex<Option<Context>>> = OnceLock::new();
    WAKE.get_or_init(Default::default)
}

/// Says which key is heard, and whether any is.  The hook is put in the first
/// time it is wanted.
pub fn configure(enabled: bool, key: u32, ctx: &Context) {
    KEY.store(key, Ordering::Relaxed);
    ENABLED.store(enabled, Ordering::Relaxed);
    if let Ok(mut slot) = wake().lock()
        && slot.is_none()
    {
        *slot = Some(ctx.clone());
    }
    if enabled {
        start();
    }
}

/// What the hook heard since the last call.
pub fn take_events() -> Vec<Event> {
    events().lock().map(|mut list| std::mem::take(&mut *list)).unwrap_or_default()
}

/// Whether the key is held now.
pub fn is_down() -> bool {
    HELD.load(Ordering::Relaxed)
}

/// What is in front: the game, this program, or something else.
pub fn front() -> u8 {
    FRONT.load(Ordering::Relaxed)
}

fn push(edge: Edge) {
    if let Ok(mut list) = events().lock() {
        list.push(Event {
            edge,
            at: Instant::now(),
        });
        // Not more than there can be a use for.
        let extra = list.len().saturating_sub(64);
        list.drain(..extra);
    }
    if let Ok(slot) = wake().lock()
        && let Some(ctx) = slot.as_ref()
    {
        ctx.request_repaint();
    }
}

fn start() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        // What is in front, looked at now and then: the hook has to be quick, and
        // finding out takes a program's name.
        std::thread::spawn(|| {
            loop {
                let wanted = ENABLED.load(Ordering::Relaxed);
                if wanted {
                    FRONT.store(crate::overlay::what_is_in_front(), Ordering::Relaxed);
                } else {
                    FRONT.store(FRONT_OTHER, Ordering::Relaxed);
                }
                std::thread::sleep(Duration::from_millis(if wanted { 80 } else { 500 }));
            }
        });
        platform::start_hook();
    });
}

/// What a key is called, as Windows says it.
pub fn key_name(key: u32) -> String {
    platform::key_name(key)
}

/// A key that is pressed now, if one is (not a button of the mouse): for
/// choosing the key.  Escape is `Some(0x1b)`.
pub fn pressed_key() -> Option<u32> {
    platform::pressed_key()
}

#[cfg(windows)]
mod platform {
    use super::*;
    use windows::Win32::Foundation::{LPARAM, LRESULT, WPARAM};
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        GetAsyncKeyState, GetKeyNameTextW, MAPVK_VK_TO_VSC, MapVirtualKeyW,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        CallNextHookEx, DispatchMessageW, GetMessageW, HC_ACTION, KBDLLHOOKSTRUCT, MSG,
        SetWindowsHookExW, TranslateMessage, WH_KEYBOARD_LL, WM_KEYDOWN, WM_KEYUP, WM_SYSKEYDOWN,
        WM_SYSKEYUP,
    };

    unsafe extern "system" fn hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
        if code as u32 == HC_ACTION && ENABLED.load(Ordering::Relaxed) {
            let info = unsafe { &*(lparam.0 as *const KBDLLHOOKSTRUCT) };
            let key = KEY.load(Ordering::Relaxed);
            if key != 0 && info.vkCode == key {
                let message = wparam.0 as u32;
                let down = message == WM_KEYDOWN || message == WM_SYSKEYDOWN;
                let up = message == WM_KEYUP || message == WM_SYSKEYUP;
                let front = FRONT.load(Ordering::Relaxed);
                if front != FRONT_OTHER {
                    if down && !HELD.swap(true, Ordering::Relaxed) {
                        push(Edge::Down);
                    } else if up && HELD.swap(false, Ordering::Relaxed) {
                        push(Edge::Up);
                    }
                    // The game does not get it: nothing of it reaches the game's
                    // chat, and Caps Lock stays as it was.
                    if front == FRONT_GAME {
                        return LRESULT(1);
                    }
                } else if up && HELD.swap(false, Ordering::Relaxed) {
                    // Let go in another window than it was pressed in.
                    push(Edge::Up);
                }
            }
        }
        unsafe { CallNextHookEx(None, code, wparam, lparam) }
    }

    pub fn start_hook() {
        std::thread::spawn(|| unsafe {
            let installed = SetWindowsHookExW(WH_KEYBOARD_LL, Some(hook), None, 0);
            if let Err(e) = installed {
                tracing::error!("unable to listen for the key of the map: {e}");
                return;
            }
            // A hook of the keyboard works as long as its thread takes messages.
            let mut message = MSG::default();
            while GetMessageW(&mut message, None, 0, 0).as_bool() {
                let _ = TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        });
    }

    pub fn key_name(key: u32) -> String {
        unsafe {
            let scan = MapVirtualKeyW(key, MAPVK_VK_TO_VSC);
            // The keys of the block between the letters and the numbers, and the
            // arrows, are of the extended kind.
            let extended = matches!(key, 0x21..=0x28 | 0x2c..=0x2e | 0x5b | 0x5c | 0x5d | 0x6f | 0x90 | 0xa3 | 0xa5);
            let lparam = ((scan << 16) | if extended { 1 << 24 } else { 0 }) as i32;
            let mut name = [0u16; 64];
            let length = GetKeyNameTextW(lparam, &mut name).max(0) as usize;
            if length == 0 {
                return format!("0x{key:02X}");
            }
            let text = String::from_utf16_lossy(&name[..length.min(name.len())]);
            // Windows says it in capitals.
            let mut words: Vec<String> = Vec::new();
            for word in text.split(' ') {
                let mut letters = word.chars();
                words.push(match letters.next() {
                    Some(first) if word.chars().count() > 1 => {
                        format!("{first}{}", letters.as_str().to_lowercase())
                    }
                    _ => word.to_owned(),
                });
            }
            words.join(" ")
        }
    }

    pub fn pressed_key() -> Option<u32> {
        // Not the mouse (1 to 6), and not the keys that only change what
        // another key means (Shift, Ctrl, Alt in their general kinds).
        (7u32..=0xfe)
            .filter(|key| !matches!(key, 0x10..=0x12))
            .find(|key| unsafe { (GetAsyncKeyState(*key as i32) as u16) & 0x8000 != 0 })
    }
}

#[cfg(not(windows))]
mod platform {
    pub fn start_hook() {}
    pub fn key_name(key: u32) -> String {
        format!("0x{key:02X}")
    }
    pub fn pressed_key() -> Option<u32> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_default_key_is_caps_lock() {
        assert_eq!(DEFAULT_KEY, 0x14);
    }

    #[test]
    fn events_wait_to_be_taken_once() {
        events().lock().unwrap().clear();
        push(Edge::Down);
        push(Edge::Up);
        let taken = take_events();
        assert_eq!(taken.len(), 2);
        assert_eq!(taken[0].edge, Edge::Down);
        assert!(take_events().is_empty());
    }
}
