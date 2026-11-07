//! Starts Genshin Impact the way that lets Irminsul see its traffic: HoYoPlay
//! first, and when it is up, the game.  Whatever of the two is running is closed
//! before, so that both start fresh.
//!
//! The game started by itself, without HoYoPlay running, is not seen by the
//! capture, and HoYoPlay's own `--game=` only shows the game in the launcher
//! without starting it; this is what works.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// How the start is going.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    #[default]
    Idle,
    Working(String),
    Done(String),
    Failed(String),
}

pub type SharedStatus = Arc<Mutex<Status>>;

#[derive(Clone, Debug)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
}

/// The names of the game's programs.
const GAME_PROGRAMS: [&str; 2] = ["genshinimpact.exe", "yuanshen.exe"];
/// The names of HoYoPlay's programs; `launcher.exe` is a name others may have,
/// so those are told by being in HoYoPlay's folder.
const LAUNCHER_PROGRAMS: [&str; 5] = [
    "launcher.exe",
    "hyp.exe",
    "hypworker.exe",
    "hyphelper.exe",
    "hyupdater.exe",
];

/// HoYoPlay's folder: the one the given program (or folder) is in, or where it
/// is installed by default.
pub fn hoyoplay_dir(custom: &str) -> Option<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();
    let custom = custom.trim().trim_matches('"');
    if !custom.is_empty() {
        let path = PathBuf::from(custom);
        if path.extension().is_some_and(|ext| ext.eq_ignore_ascii_case("exe")) {
            if let Some(parent) = path.parent() {
                candidates.push(parent.to_path_buf());
            }
        } else {
            candidates.push(path);
        }
    }
    for variable in ["ProgramFiles", "ProgramFiles(x86)", "ProgramW6432"] {
        if let Some(base) = std::env::var_os(variable) {
            candidates.push(Path::new(&base).join("HoYoPlay"));
        }
    }
    candidates.push(r"C:\Program Files\HoYoPlay".into());
    candidates
        .into_iter()
        .find(|dir| dir.join("launcher.exe").is_file())
}

/// The game's program, in the `games` folder of HoYoPlay.
pub fn game_exe(dir: &Path) -> Option<PathBuf> {
    let games = dir.join("games");
    for entry in std::fs::read_dir(&games).ok()?.flatten() {
        for program in ["GenshinImpact.exe", "YuanShen.exe"] {
            let path = entry.path().join(program);
            if path.is_file() {
                return Some(path);
            }
        }
    }
    None
}

fn is_under(path: &str, dir: &Path) -> bool {
    path.to_lowercase()
        .replace('/', "\\")
        .starts_with(&dir.to_string_lossy().to_lowercase().replace('/', "\\"))
}

/// The game's and HoYoPlay's programs that are running.
pub fn find_running(dir: &Path) -> Vec<ProcessInfo> {
    platform::processes()
        .into_iter()
        .filter(|(pid, name)| {
            let lower = name.to_lowercase();
            GAME_PROGRAMS.contains(&lower.as_str())
                || (LAUNCHER_PROGRAMS.contains(&lower.as_str())
                    && platform::image_path(*pid).is_some_and(|path| is_under(&path, dir)))
        })
        .map(|(pid, name)| ProcessInfo { pid, name })
        .collect()
}

/// Whether the game is running.
pub fn game_running() -> bool {
    platform::processes()
        .into_iter()
        .any(|(_, name)| GAME_PROGRAMS.contains(&name.to_lowercase().as_str()))
}

fn set(status: &SharedStatus, value: Status) {
    if let Ok(mut status) = status.lock() {
        *status = value;
    }
}

/// Closes what is running, starts HoYoPlay, waits for it, starts the game.  It
/// runs in a thread of its own; `status` tells how it is going.
pub fn run(dir: PathBuf, status: SharedStatus) {
    std::thread::spawn(move || {
        let result = (|| -> Result<(), String> {
            let launcher = dir.join("launcher.exe");
            let game = game_exe(&dir)
                .ok_or_else(|| format!("The game was not found in {}\\games", dir.display()))?;

            let running = find_running(&dir);
            if !running.is_empty() {
                set(&status, Status::Working("Closing what is running".to_owned()));
                platform::close(&running);
                // Whatever is left is left alone for a moment to let go.
                std::thread::sleep(Duration::from_millis(1500));
                if !find_running(&dir).is_empty() {
                    return Err("Something is still running and could not be closed".to_owned());
                }
            }

            set(&status, Status::Working("Starting HoYoPlay".to_owned()));
            platform::start_as_user(&launcher).map_err(|e| format!("Unable to start HoYoPlay: {e}"))?;

            set(&status, Status::Working("Waiting for HoYoPlay".to_owned()));
            let started = Instant::now();
            loop {
                if platform::launcher_has_window(&dir) {
                    break;
                }
                if started.elapsed() > Duration::from_secs(60) {
                    return Err("HoYoPlay did not come up".to_owned());
                }
                std::thread::sleep(Duration::from_millis(500));
            }
            // It is up, and is given a moment to be ready.
            std::thread::sleep(Duration::from_secs(4));

            set(&status, Status::Working("Starting the game".to_owned()));
            std::process::Command::new(&game)
                .current_dir(game.parent().unwrap_or(&dir))
                .spawn()
                .map_err(|e| format!("Unable to start the game: {e}"))?;
            Ok(())
        })();
        match result {
            Ok(()) => set(&status, Status::Done("The game is starting".to_owned())),
            Err(e) => {
                tracing::warn!("starting the game failed: {e}");
                set(&status, Status::Failed(e));
            }
        }
    });
}

#[cfg(windows)]
mod platform {
    use std::path::Path;
    use std::time::Duration;

    use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, WPARAM};
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_NAME_FORMAT, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE,
        PROCESS_TERMINATE, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetWindowThreadProcessId, IsWindowVisible, PostMessageW, WM_CLOSE,
    };
    use windows::core::{BOOL, PWSTR};

    /// Every program running, as the process id and the name of its file.
    pub fn processes() -> Vec<(u32, String)> {
        let mut list = Vec::new();
        unsafe {
            let Ok(snapshot) = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) else {
                return list;
            };
            let mut entry = PROCESSENTRY32W {
                dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
                ..Default::default()
            };
            if Process32FirstW(snapshot, &mut entry).is_ok() {
                loop {
                    let length = entry
                        .szExeFile
                        .iter()
                        .position(|c| *c == 0)
                        .unwrap_or(entry.szExeFile.len());
                    list.push((
                        entry.th32ProcessID,
                        String::from_utf16_lossy(&entry.szExeFile[..length]),
                    ));
                    if Process32NextW(snapshot, &mut entry).is_err() {
                        break;
                    }
                }
            }
            let _ = CloseHandle(snapshot);
        }
        list
    }

    /// The full path of the program of a process.
    pub fn image_path(pid: u32) -> Option<String> {
        unsafe {
            let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
            let mut buffer = [0u16; 520];
            let mut length = buffer.len() as u32;
            let queried = QueryFullProcessImageNameW(
                handle,
                PROCESS_NAME_FORMAT(0),
                PWSTR(buffer.as_mut_ptr()),
                &mut length,
            )
            .is_ok();
            let _ = CloseHandle(handle);
            queried.then(|| String::from_utf16_lossy(&buffer[..length as usize]))
        }
    }

    unsafe extern "system" fn close_window(window: HWND, data: LPARAM) -> BOOL {
        unsafe {
            let targets = &*(data.0 as *const Vec<u32>);
            let mut pid = 0u32;
            GetWindowThreadProcessId(window, Some(&mut pid));
            if targets.contains(&pid) {
                let _ = PostMessageW(Some(window), WM_CLOSE, WPARAM(0), LPARAM(0));
            }
        }
        BOOL(1)
    }

    struct Search {
        pids: Vec<u32>,
        found: bool,
    }

    unsafe extern "system" fn visible_window(window: HWND, data: LPARAM) -> BOOL {
        unsafe {
            let search = &mut *(data.0 as *mut Search);
            let mut pid = 0u32;
            GetWindowThreadProcessId(window, Some(&mut pid));
            if search.pids.contains(&pid) && IsWindowVisible(window).as_bool() {
                search.found = true;
            }
        }
        BOOL(1)
    }

    /// Asks the programs to close, as the window's own close does, and then,
    /// what is still there after a while is ended.
    pub fn close(list: &[super::ProcessInfo]) {
        let pids: Vec<u32> = list.iter().map(|info| info.pid).collect();
        unsafe {
            let _ = EnumWindows(Some(close_window), LPARAM(&pids as *const Vec<u32> as isize));
            for pid in &pids {
                let Ok(handle) = OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, false, *pid)
                else {
                    continue;
                };
                // Up to 8 seconds to close by itself.
                let waited = WaitForSingleObject(handle, Duration::from_secs(8).as_millis() as u32);
                if waited.0 != 0 {
                    let _ = TerminateProcess(handle, 0);
                    let _ = WaitForSingleObject(handle, 3000);
                }
                let _ = CloseHandle(handle);
            }
        }
    }

    /// Starts a program as the user who is logged on, not with the rights this
    /// program has, which is how a double click on it does it.
    pub fn start_as_user(program: &Path) -> std::io::Result<()> {
        std::process::Command::new("explorer.exe")
            .arg(program)
            .spawn()
            .map(|_| ())
    }

    /// Whether HoYoPlay has a window on the screen.
    pub fn launcher_has_window(dir: &Path) -> bool {
        let pids: Vec<u32> = processes()
            .into_iter()
            .filter(|(pid, name)| {
                let lower = name.to_lowercase();
                (lower == "launcher.exe" || lower == "hyp.exe")
                    && image_path(*pid).is_some_and(|path| super::is_under(&path, dir))
            })
            .map(|(pid, _)| pid)
            .collect();
        if pids.is_empty() {
            return false;
        }
        let mut search = Search { pids, found: false };
        unsafe {
            let _ = EnumWindows(Some(visible_window), LPARAM(&mut search as *mut Search as isize));
        }
        search.found
    }
}

#[cfg(not(windows))]
mod platform {
    use std::path::Path;

    pub fn processes() -> Vec<(u32, String)> {
        Vec::new()
    }
    pub fn image_path(_pid: u32) -> Option<String> {
        None
    }
    pub fn close(_list: &[super::ProcessInfo]) {}
    pub fn start_as_user(_program: &Path) -> std::io::Result<()> {
        Err(std::io::Error::other("only on Windows"))
    }
    pub fn launcher_has_window(_dir: &Path) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_is_under_the_folder_whatever_its_case_and_slashes() {
        let dir = Path::new(r"C:\Program Files\HoYoPlay");
        assert!(is_under(r"c:\program files\hoyoplay\1.19\HYP.exe", dir));
        assert!(is_under("C:/Program Files/HoYoPlay/launcher.exe", dir));
        assert!(!is_under(r"C:\Windows\launcher.exe", dir));
    }

    #[test]
    fn the_games_folder_is_searched_for_its_program() {
        let base = std::env::temp_dir().join(format!("irminsul-launch-test-{}", std::process::id()));
        let game_dir = base.join("games").join("Genshin Impact game");
        std::fs::create_dir_all(&game_dir).unwrap();
        assert!(game_exe(&base).is_none());
        std::fs::write(game_dir.join("GenshinImpact.exe"), b"").unwrap();
        assert_eq!(game_exe(&base), Some(game_dir.join("GenshinImpact.exe")));
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn the_given_folder_is_taken_if_it_has_the_launcher() {
        let base = std::env::temp_dir().join(format!("irminsul-launcher-test-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        assert_ne!(hoyoplay_dir(&base.to_string_lossy()), Some(base.clone()));
        std::fs::write(base.join("launcher.exe"), b"").unwrap();
        assert_eq!(hoyoplay_dir(&base.to_string_lossy()), Some(base.clone()));
        // The program itself can be given too.
        let program = base.join("launcher.exe");
        assert_eq!(hoyoplay_dir(&program.to_string_lossy()), Some(base.clone()));
        let _ = std::fs::remove_dir_all(&base);
    }
}
