//! A probe for the game's traffic, to find out how it reports changes.
//!
//! When it is on, every command that looks like it carries items, player
//! properties, characters, or one of the numbers being looked for, is written
//! as a line of text to `probe-<date>.log` in the log folder, with the time.
//! The numbers are not the raw packets, but ids and counts: enough to see which
//! command says what, and to compare with what the game showed.  Nothing is
//! sent anywhere.

use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Write};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use chrono::Local;

static ENABLED: AtomicBool = AtomicBool::new(false);

fn needles() -> &'static Mutex<Vec<u64>> {
    static NEEDLES: OnceLock<Mutex<Vec<u64>>> = OnceLock::new();
    NEEDLES.get_or_init(Default::default)
}

fn file() -> &'static Mutex<Option<(String, BufWriter<File>)>> {
    static FILE: OnceLock<Mutex<Option<(String, BufWriter<File>)>>> = OnceLock::new();
    FILE.get_or_init(Default::default)
}

pub fn set_enabled(on: bool) {
    ENABLED.store(on, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    ENABLED.load(Ordering::Relaxed)
}

/// The numbers to look for, as typed: separated by commas, spaces or semicolons.
/// Digit groups like `23 813 567` are not told apart from separate numbers, so
/// they should be typed without spaces.
pub fn set_needles(text: &str) {
    let list: Vec<u64> = text
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect();
    if let Ok(mut needles) = needles().lock() {
        *needles = list;
    }
}

fn write_line(text: &str) {
    let Some(dir) = crate::log_dir().ok() else {
        return;
    };
    let name = format!("probe-{}.log", Local::now().format("%Y-%m-%d"));
    let Ok(mut file) = file().lock() else {
        return;
    };
    if file.as_ref().is_none_or(|(open, _)| *open != name) {
        let _ = std::fs::create_dir_all(&dir);
        *file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(dir.join(&name))
            .ok()
            .map(|opened| (name, BufWriter::new(opened)));
    }
    if let Some((_, writer)) = file.as_mut() {
        let line = format!("{} {text}", Local::now().format("%H:%M:%S%.3f"));
        if writeln!(writer, "{line}").and_then(|_| writer.flush()).is_err() {
            *file = None;
        }
    }
}

/// For this long after a note, commands of a kind that is not often seen are
/// written too, whatever they look like: what the game says about something
/// that was just done is one of those.
const RARE_WINDOW: Duration = Duration::from_secs(25);

fn rare_window() -> &'static Mutex<Option<Instant>> {
    static WINDOW: OnceLock<Mutex<Option<Instant>>> = OnceLock::new();
    WINDOW.get_or_init(Default::default)
}

/// How often each kind of command was seen lately: when the count began, and
/// the count.
fn seen() -> &'static Mutex<HashMap<u16, (Instant, u32)>> {
    static SEEN: OnceLock<Mutex<HashMap<u16, (Instant, u32)>>> = OnceLock::new();
    SEEN.get_or_init(Default::default)
}

/// A note in the probe's file: what the user was doing at that time.
pub fn note(text: &str) {
    if enabled() {
        write_line(&format!("NOTE {text}"));
        if let Ok(mut window) = rare_window().lock() {
            *window = Some(Instant::now() + RARE_WINDOW);
        }
    }
}

/// Counts the command, and says whether it is a rare one: seen at most three
/// times in the last ten seconds.
fn rare(command_id: u16, now: Instant) -> bool {
    let Ok(mut seen) = seen().lock() else {
        return false;
    };
    let entry = seen.entry(command_id).or_insert((now, 0));
    if now.duration_since(entry.0) > Duration::from_secs(10) {
        *entry = (now, 0);
    }
    entry.1 += 1;
    entry.1 <= 3
}

// ---------------------------------------------------------------------------
// Reading a message without knowing what it is.

enum Value<'a> {
    Varint(u64),
    Fixed(u64),
    Bytes(&'a [u8]),
}

fn varint(data: &[u8], at: &mut usize) -> Option<u64> {
    let mut value = 0u64;
    for shift in (0..70).step_by(7) {
        let byte = *data.get(*at)?;
        *at += 1;
        value |= ((byte & 0x7f) as u64).checked_shl(shift).unwrap_or(0);
        if byte & 0x80 == 0 {
            return Some(value);
        }
    }
    None
}

/// The fields of a protobuf message, if `data` is one, all of it.
fn fields(data: &[u8]) -> Option<Vec<(u32, Value<'_>)>> {
    let mut at = 0;
    let mut out = Vec::new();
    while at < data.len() {
        let key = varint(data, &mut at)?;
        let (number, kind) = ((key >> 3) as u32, key & 7);
        if number == 0 {
            return None;
        }
        let value = match kind {
            0 => Value::Varint(varint(data, &mut at)?),
            1 => {
                let bytes = data.get(at..at + 8)?;
                at += 8;
                Value::Fixed(u64::from_le_bytes(bytes.try_into().ok()?))
            }
            2 => {
                let length = varint(data, &mut at)? as usize;
                let bytes = data.get(at..at.checked_add(length)?)?;
                at += length;
                Value::Bytes(bytes)
            }
            5 => {
                let bytes = data.get(at..at + 4)?;
                at += 4;
                Value::Fixed(u32::from_le_bytes(bytes.try_into().ok()?) as u64)
            }
            _ => return None,
        };
        out.push((number, value));
    }
    (!out.is_empty()).then_some(out)
}

/// Text, such as a name: shown by its length, not its letters.
fn looks_like_text(bytes: &[u8]) -> bool {
    bytes.len() >= 2
        && std::str::from_utf8(bytes)
            .is_ok_and(|text| text.chars().all(|c| !c.is_control()))
}

#[derive(Default)]
struct Found {
    items: Vec<String>,
    props: Vec<String>,
    avatars: Vec<String>,
    hits: Vec<String>,
}

impl Found {
    fn interesting(&self) -> bool {
        !(self.items.is_empty()
            && self.props.is_empty()
            && self.avatars.is_empty()
            && self.hits.is_empty())
    }
}

fn first_varint(list: &[(u32, Value)], number: u32) -> Option<u64> {
    list.iter().find_map(|(n, value)| match value {
        Value::Varint(v) if *n == number => Some(*v),
        _ => None,
    })
}

fn first_bytes<'a>(list: &[(u32, Value<'a>)], number: u32) -> Option<&'a [u8]> {
    list.iter().find_map(|(n, value)| match value {
        Value::Bytes(b) if *n == number => Some(*b),
        _ => None,
    })
}

/// What a message that was just read looks like it is.
fn classify(list: &[(u32, Value)], found: &mut Found) {
    let first = first_varint(list, 1);
    let second = first_varint(list, 2);
    let has_detail = list
        .iter()
        .any(|(n, v)| matches!(v, Value::Bytes(_)) && (5..=7).contains(n));
    if let (Some(id), Some(guid)) = (first, second)
        && (100..=999_999).contains(&id)
        && guid >= 1_000_000
        && has_detail
    {
        // A material says how many there are in field 5; a weapon or artifact
        // has its details in 6.
        let count = first_bytes(list, 5)
            .and_then(fields)
            .and_then(|inner| first_varint(&inner, 1));
        found.items.push(match count {
            Some(count) => format!("item {id} guid={guid} count={count}"),
            None => format!("item {id} guid={guid} equip"),
        });
    }
    if let Some(key) = first
        && (10_001..=10_999).contains(&key)
        && let Some(value) = first_bytes(list, 2).and_then(fields)
    {
        let number = first_varint(&value, 4).or_else(|| first_varint(&value, 2));
        found.props.push(match number {
            Some(number) => format!("prop {key}={number}"),
            None => format!("prop {key}=?"),
        });
    }
    if let (Some(id), Some(guid)) = (first, second)
        && (10_000_002..=10_000_900).contains(&id)
        && guid >= 1_000_000
        && first_bytes(list, 3).is_some()
    {
        found.avatars.push(format!("avatar {id} guid={guid}"));
    }
}

fn describe(
    data: &[u8],
    depth: u32,
    path: &str,
    needles: &[u64],
    found: &mut Found,
    out: &mut String,
) {
    let Some(list) = fields(data) else {
        return;
    };
    classify(&list, found);
    out.push('{');
    for (index, (number, value)) in list.iter().enumerate() {
        if index > 0 {
            out.push(' ');
        }
        let here = if path.is_empty() {
            number.to_string()
        } else {
            format!("{path}.{number}")
        };
        match value {
            Value::Varint(v) | Value::Fixed(v) => {
                out.push_str(&format!("{number}:{v}"));
                if needles.contains(v) {
                    found.hits.push(format!("needle {v} at {here}"));
                }
            }
            Value::Bytes(bytes) => {
                if looks_like_text(bytes) {
                    out.push_str(&format!("{number}:text({})", bytes.len()));
                } else if depth < 6 && fields(bytes).is_some() {
                    out.push_str(&format!("{number}:"));
                    describe(bytes, depth + 1, &here, needles, found, out);
                } else {
                    out.push_str(&format!("{number}:bytes({})", bytes.len()));
                }
            }
        }
    }
    out.push('}');
}

/// Looks at a command, and writes a line if it is of interest.
pub fn observe(command_id: u16, body: &[u8]) {
    if !enabled() || body.len() > 30_000 {
        return;
    }
    let needles = needles().lock().map(|n| n.clone()).unwrap_or_default();
    let mut found = Found::default();
    let mut tree = String::new();
    describe(body, 0, "", &needles, &mut found, &mut tree);
    let now = Instant::now();
    let is_rare = rare(command_id, now);
    if !found.interesting() {
        let in_window = rare_window()
            .lock()
            .ok()
            .and_then(|window| *window)
            .is_some_and(|until| now < until);
        if in_window && is_rare && body.len() <= 3000 && !tree.is_empty() {
            if tree.len() > 700 {
                tree.truncate(700);
                tree.push('…');
            }
            write_line(&format!("rare cmd={command_id} len={} {tree}", body.len()));
        }
        return;
    }
    let mut reasons: Vec<String> = Vec::new();
    for list in [&found.items, &found.props, &found.avatars, &found.hits] {
        reasons.extend(list.iter().take(12).cloned());
    }
    let more = found.items.len() + found.props.len() + found.avatars.len() + found.hits.len();
    if tree.len() > 700 {
        tree.truncate(700);
        tree.push('…');
    }
    write_line(&format!(
        "cmd={command_id} len={} found={} [{}] {tree}",
        body.len(),
        more,
        reasons.join("; ")
    ));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn put_varint(out: &mut Vec<u8>, mut value: u64) {
        loop {
            let byte = (value & 0x7f) as u8;
            value >>= 7;
            if value == 0 {
                out.push(byte);
                return;
            }
            out.push(byte | 0x80);
        }
    }

    fn field_varint(number: u32, value: u64) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, (number as u64) << 3);
        put_varint(&mut out, value);
        out
    }

    fn field_bytes(number: u32, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        put_varint(&mut out, ((number as u64) << 3) | 2);
        put_varint(&mut out, bytes.len() as u64);
        out.extend_from_slice(bytes);
        out
    }

    /// An item with a count, like the game's own.
    fn material(id: u64, guid: u64, count: u64) -> Vec<u8> {
        [
            field_varint(1, id),
            field_varint(2, guid),
            field_bytes(5, &field_varint(1, count)),
        ]
        .concat()
    }

    #[test]
    fn items_are_recognised_by_their_shape() {
        let message = field_bytes(3, &material(104003, 2_000_000_001, 120));
        let mut found = Found::default();
        let mut tree = String::new();
        describe(&message, 0, "", &[], &mut found, &mut tree);
        assert_eq!(found.items, vec!["item 104003 guid=2000000001 count=120"]);
    }

    #[test]
    fn properties_are_recognised_by_their_keys() {
        let value = [field_varint(1, 10016), field_varint(4, 23_813_567)].concat();
        let entry = [field_varint(1, 10016), field_bytes(2, &value)].concat();
        let message = field_bytes(2, &entry);
        let mut found = Found::default();
        describe(&message, 0, "", &[], &mut found, &mut String::new());
        assert_eq!(found.props, vec!["prop 10016=23813567"]);
    }

    #[test]
    fn a_number_that_is_looked_for_is_found_with_its_place() {
        let inner = field_varint(7, 23_813_567);
        let message = field_bytes(3, &field_bytes(5, &inner));
        let mut found = Found::default();
        describe(&message, 0, "", &[23_813_567], &mut found, &mut String::new());
        assert_eq!(found.hits, vec!["needle 23813567 at 3.5.7"]);
    }

    #[test]
    fn noise_is_not_of_interest() {
        let noise: Vec<u8> = (0..400u32).map(|i| (i * 37 % 251) as u8).collect();
        let mut found = Found::default();
        describe(&noise, 0, "", &[], &mut found, &mut String::new());
        assert!(!found.interesting());
    }

    #[test]
    fn needles_are_read_from_text() {
        set_needles("23813567, 120;5 x");
        assert_eq!(*needles().lock().unwrap(), vec![23_813_567, 120, 5]);
        set_needles("");
    }
}
