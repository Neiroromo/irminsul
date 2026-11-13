//! The background log of a run of the program: everything the program says while it runs (the
//! debug timeline of the capture among it), written to a file of its own, `log/sessions/
//! session-<time>.log`.
//!
//! It is always written (unless turned off in Power Tools, which holds from the next start),
//! and thrown away when the program ends, unless it was marked to be kept (a button in Power
//! Tools): so that when something goes wrong while playing, what happened is there to look at,
//! without having to make it happen again.  A log that was not marked and is still there when
//! the program starts (the program did not end by itself) is thrown away then.  Only the files
//! of this kind are ever removed: the other logs are left as they are.

use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

use chrono::Local;

/// What the background log takes, whatever the level chosen for the usual log is.
pub const FILTER: &str = "warn,irminsul=debug,timeline=info";
/// The target of the lines of the debug timeline.
pub const TIMELINE: &str = "timeline";

struct Session {
    file: File,
    path: PathBuf,
    kept: bool,
}

fn session() -> &'static Mutex<Option<Session>> {
    static SESSION: OnceLock<Mutex<Option<Session>>> = OnceLock::new();
    SESSION.get_or_init(Default::default)
}

fn dir() -> Option<PathBuf> {
    crate::log_dir().ok().map(|dir| dir.join("sessions"))
}

fn switch_path() -> Option<PathBuf> {
    dir().map(|dir| dir.join("off"))
}

/// Whether the background log is written from the start of the program.
pub fn enabled() -> bool {
    switch_path().is_none_or(|path| !path.exists())
}

/// Turns the background log on or off for the next start of the program.
pub fn set_enabled(on: bool) {
    let Some(path) = switch_path() else {
        return;
    };
    if on {
        let _ = fs::remove_file(path);
    } else {
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        let _ = fs::write(path, "the background log is off\n");
    }
}

fn keep_marker(log: &std::path::Path) -> PathBuf {
    log.with_extension("keep")
}

/// Removes the background logs that were not marked to be kept.
fn remove_unmarked(dir: &std::path::Path) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("session-") && name.ends_with(".log") && !keep_marker(&path).exists() {
            let _ = fs::remove_file(&path);
        }
    }
}

/// Starts the background log of this run (if it is on), after throwing away the ones left
/// that were not marked.
pub fn start() {
    let Some(dir) = dir() else {
        return;
    };
    if fs::create_dir_all(&dir).is_err() {
        return;
    }
    remove_unmarked(&dir);
    if !enabled() {
        return;
    }
    let path = dir.join(format!("session-{}.log", Local::now().format("%Y-%m-%d_%H-%M-%S")));
    let Ok(mut file) = File::create(&path) else {
        return;
    };
    let _ = writeln!(
        file,
        "Irminsul {} background log, started {}",
        env!("CARGO_PKG_VERSION"),
        Local::now().to_rfc3339()
    );
    if let Ok(mut session) = session().lock() {
        *session = Some(Session { file, path, kept: false });
    }
}

/// Whether a background log is being written, and whether it is marked to be kept.
pub fn status() -> Option<(PathBuf, bool)> {
    let session = session().lock().ok()?;
    session.as_ref().map(|session| (session.path.clone(), session.kept))
}

/// Marks the background log of this run to be kept, and writes the mark (with a note) into it,
/// to find the place later.
pub fn keep(note: &str) {
    let Ok(mut session) = session().lock() else {
        return;
    };
    let Some(session) = session.as_mut() else {
        return;
    };
    let stamp = Local::now().format("%Y-%m-%d %H:%M:%S%.3f");
    let _ = writeln!(session.file, "{stamp} ===== MARK: {note} =====");
    let marker = keep_marker(&session.path);
    let mut text = fs::read_to_string(&marker).unwrap_or_default();
    text.push_str(&format!("{stamp} {note}\n"));
    if fs::write(&marker, text).is_ok() {
        session.kept = true;
    }
}

/// Ends the background log: it is removed unless it was marked.
pub fn finish() {
    let Ok(mut session) = session().lock() else {
        return;
    };
    if let Some(session) = session.take() {
        let Session { file, path, kept } = session;
        drop(file);
        if !kept {
            let _ = fs::remove_file(path);
        }
    }
}

/// Writes to the background log, when there is one.
#[derive(Clone, Copy, Default)]
pub struct Writer;

impl Write for Writer {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if let Ok(mut session) = session().lock()
            && let Some(session) = session.as_mut()
        {
            // A log that can not be written is not a reason to stop.
            let _ = session.file.write_all(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Writer {
    type Writer = Writer;

    fn make_writer(&'a self) -> Self::Writer {
        Writer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_unmarked_background_logs_are_removed() {
        let dir = tempfile::tempdir().expect("a folder");
        let at = |name: &str| dir.path().join(name);
        for name in ["session-1.log", "session-2.log", "session-2.keep", "log.2026-11-10", "track-2026-11-10.log", "off"] {
            fs::write(at(name), "x").expect("written");
        }
        remove_unmarked(dir.path());
        assert!(!at("session-1.log").exists());
        for name in ["session-2.log", "session-2.keep", "log.2026-11-10", "track-2026-11-10.log", "off"] {
            assert!(at(name).exists(), "{name}");
        }
    }
}
