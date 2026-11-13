//! A timestamped record of what the capture and the sniffer did, together with
//! markers the user adds while going through the game's launch and login, to
//! see at which step data starts or stops coming in.

use std::collections::VecDeque;
use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Local;

use crate::APP_ID;

/// Lines kept for showing in the UI.
const RECENT_LINES: usize = 12;

pub fn debug_dir() -> Result<PathBuf> {
    let mut dir = eframe::storage_dir(APP_ID).context("Storage dir not found")?;
    dir.push("debug");
    Ok(dir)
}

pub struct DebugTimeline {
    file: Option<BufWriter<File>>,
    recent: VecDeque<String>,
}

impl DebugTimeline {
    pub fn new() -> Self {
        let file = Self::open().map_err(|e| {
            tracing::warn!("unable to open the debug timeline file: {e:#}");
        });
        Self {
            file: file.ok(),
            recent: VecDeque::new(),
        }
    }

    fn open() -> Result<BufWriter<File>> {
        let dir = debug_dir()?;
        fs::create_dir_all(&dir)?;
        let name = format!("timeline-{}.log", Local::now().format("%Y-%m-%d"));
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(name))?;
        Ok(BufWriter::new(file))
    }

    pub fn record(&mut self, text: impl AsRef<str>) {
        // The background log of the run has the timeline too.
        tracing::info!(target: "timeline", "{}", text.as_ref());
        let line = format!("{} {}", Local::now().format("%H:%M:%S%.3f"), text.as_ref());
        if let Some(file) = &mut self.file {
            // Flushed per line: the file is read while the app is running.
            if writeln!(file, "{line}").and_then(|_| file.flush()).is_err() {
                self.file = None;
            }
        }
        self.recent.push_back(line);
        while self.recent.len() > RECENT_LINES {
            self.recent.pop_front();
        }
    }

    /// Keeps the bytes of a command that could not be decrypted next to the
    /// timeline, for working out why offline.
    pub fn save_sample(&mut self, bytes: &[u8]) {
        let name = format!("undecrypted-{}.bin", Local::now().format("%Y-%m-%d_%H-%M-%S%.3f"));
        let saved = debug_dir().and_then(|dir| {
            fs::create_dir_all(&dir)?;
            fs::write(dir.join(&name), bytes)?;
            Ok(())
        });
        if let Err(e) = saved {
            tracing::warn!("unable to save {name}: {e:#}");
        }
    }

    /// Keeps a command whose body is packed in a way that is not known: the size it declares
    /// (4 bytes), the length of its header (2 bytes), then the header and the body.
    pub fn save_packed(&mut self, command_id: u16, declared: usize, header_len: u16, bytes: &[u8], login: bool) {
        // One of the login has the token of the account in it: it is named so, to be removed
        // when it was looked at.
        let kind = if login { "packed-login" } else { "packed" };
        let name = format!("{kind}-{command_id}-{}.bin", Local::now().format("%Y-%m-%d_%H-%M-%S%.3f"));
        let mut data = (declared as u32).to_be_bytes().to_vec();
        data.extend_from_slice(&header_len.to_be_bytes());
        data.extend_from_slice(bytes);
        let saved = debug_dir().and_then(|dir| {
            fs::create_dir_all(&dir)?;
            fs::write(dir.join(&name), data)?;
            Ok(())
        });
        if let Err(e) = saved {
            tracing::warn!("unable to save {name}: {e:#}");
        }
    }

    pub fn recent(&self) -> Vec<String> {
        self.recent.iter().cloned().collect()
    }
}
