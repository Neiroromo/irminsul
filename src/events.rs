//! The events of the game for the timeline: what the official announcements of the game say
//! (the same list the game shows in its notice window, with the text of each one), the versions
//! of the game, the links to the articles of the official site, and the periods that repeat by
//! the calendar (the Spiral Abyss, the Imaginarium Theater).
//!
//! Everything is fetched in the background now and then and kept in a file, so the timeline
//! shows the past of the events too, and works without the network.
//!
//! About the times: the list of the announcements says when an announcement is shown, which is
//! not when its event begins.  The real period is in the text ("After the Version 7.2 update –
//! 2026/11/24 17:59"), in the time of UTC+8, which is what the servers go by.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock, RwLock};
use std::time::Duration;

use anyhow::{Context as _, Result};
use chrono::{Datelike, FixedOffset, NaiveDate, NaiveDateTime, TimeZone};
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::APP_ID;

const API: &str = "https://sg-hk4e-api-static.hoyoverse.com/common/hk4e_global/announcement/api";
const QUERY: &str = "game=hk4e&game_biz=hk4e_global&auth_appid=announcement&authkey_ver=1&bundle_id=hk4e_global&channel_id=1&level=60&platform=pc&region=os_euro&sdk_presentation_style=fullscreen&sdk_screen=0&uid=700000000";
/// The articles of the official site: all the news, newest first.
const SITE: &str = "https://sg-public-api-static.hoyoverse.com/content_v2_user/app/a1b1f9d3315447cc/getContentList?iAppId=32&iChanId=395&iPageSize=50&sLangKey=en-us";
/// Where the articles are (the number is the id of an article).
pub const ARTICLE: &str = "https://genshin.hoyoverse.com/en/news/detail/";
pub const NEWS: &str = "https://genshin.hoyoverse.com/en/news";
/// The list is fetched again when it is this old.
const MAX_AGE: Duration = Duration::from_secs(6 * 3600);
/// What ended longer ago than this is forgotten.
const KEEP_DAYS: i64 = 75;
/// What lasts longer than this is not an event but a standing page.
const LONGEST_DAYS: i64 = 400;
/// The groups of the announcements: events, and the Astral Plane (the notices are left out).
const GROUPS: [u32; 2] = [1, 26];
/// The servers go by UTC+8; a version goes live at 11 o'clock of it, the day changes at 4.
const ASIA: i32 = 8 * 3600;
/// A version lasts six weeks.
const VERSION_DAYS: i64 = 42;
/// The longest text kept of an announcement.
const TEXT_MAX: usize = 4500;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// A wish (banner).
    Banner,
    Event,
    /// The Astral Plane.
    Astral,
    /// The Spiral Abyss.
    Abyss,
    /// The Imaginarium Theater.
    Theater,
}

/// A period of the timeline.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Event {
    /// The id of the announcement; 0 for a period made by the calendar.
    pub id: u64,
    pub en: String,
    pub ru: String,
    /// When it begins and ends, in seconds of UTC.
    pub start: i64,
    pub end: i64,
    pub kind: Kind,
    /// The picture of its announcement.
    #[serde(default)]
    pub banner: String,
    /// What the announcement says, as plain text.
    #[serde(default)]
    pub text_en: String,
    #[serde(default)]
    pub text_ru: String,
    /// The article of the official site about it.
    #[serde(default)]
    pub link: String,
    /// How many primogems the text promises, 0 if it does not say.
    #[serde(default)]
    pub primogems: u32,
}

impl Event {
    pub fn title(&self, russian: bool) -> &str {
        if russian && !self.ru.is_empty() { &self.ru } else { &self.en }
    }

    pub fn text(&self, russian: bool) -> &str {
        if russian && !self.text_ru.is_empty() { &self.text_ru } else { &self.text_en }
    }

    /// The short title for the bar: the full title is for the card.
    pub fn short_title(&self, russian: bool) -> String {
        shorten(self.title(russian), self.kind)
    }
}

/// A version of the game.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Version {
    /// "7.2".
    pub name: String,
    /// When it goes live, in seconds of UTC.
    pub start: i64,
    /// Whether the date is from an official announcement (else counted from the others).
    pub known: bool,
}

/// "Event Wish "Name" - Boosted ..." becomes "Wish Name"; the others are cut at the first colon.
fn shorten(title: &str, kind: Kind) -> String {
    let quoted = |open: char, close: char| {
        let from = title.find(open)? + open.len_utf8();
        let to = from + title[from..].find(close)?;
        Some(title[from..to].trim().to_owned())
    };
    let name = quoted('"', '"').or_else(|| quoted('«', '»')).or_else(|| quoted('“', '”'));
    if kind == Kind::Banner
        && let Some(name) = name
    {
        let russian = title.chars().any(|c| ('а'..='я').contains(&c.to_ascii_lowercase()));
        return format!("{} {name}", if russian { "Молитва" } else { "Wish" });
    }
    match title.split_once(':') {
        Some((head, _)) if head.chars().count() >= 6 => head.trim().to_owned(),
        _ => title.to_owned(),
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Stored {
    /// When it was fetched, in seconds.
    #[serde(default)]
    stamp: i64,
    #[serde(default)]
    events: BTreeMap<u64, Event>,
    #[serde(default)]
    versions: Vec<Version>,
}

fn file() -> Option<PathBuf> {
    Some(eframe::storage_dir(APP_ID)?.join("events.json"))
}

fn stored() -> &'static RwLock<Stored> {
    static STORED: OnceLock<RwLock<Stored>> = OnceLock::new();
    STORED.get_or_init(|| {
        let loaded = file()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|text| serde_json::from_str(&text).ok())
            .unwrap_or_default();
        RwLock::new(loaded)
    })
}

/// How the fetching is going.
#[derive(Clone, Debug, Default)]
pub struct Status {
    pub running: bool,
    pub error: Option<String>,
}

fn status() -> &'static Mutex<Status> {
    static STATUS: OnceLock<Mutex<Status>> = OnceLock::new();
    STATUS.get_or_init(Default::default)
}

pub fn fetch_status() -> Status {
    status().lock().map(|status| status.clone()).unwrap_or_default()
}

/// When the events were fetched last, in seconds of UTC; 0 if never.
pub fn fetched_at() -> i64 {
    stored().read().map_or(0, |stored| stored.stamp)
}

/// All the periods for the timeline, by their beginning: the announced ones (with the past
/// ones) and the ones of the calendar.
pub fn all(now: i64) -> Vec<Event> {
    let mut events: Vec<Event> = stored().read().map(|stored| stored.events.values().cloned().collect()).unwrap_or_default();
    events.extend(periodic(now));
    events.sort_by_key(|event| (event.start, event.id));
    events
}

fn zone() -> FixedOffset {
    FixedOffset::east_opt(ASIA).unwrap_or_else(|| FixedOffset::east_opt(0).expect("zero offset"))
}

/// The versions from a while ago to a while ahead: the known ones, and the others counted by
/// six weeks from them.  Each lasts until the next one begins.
pub fn versions(now: i64) -> Vec<Version> {
    let known: Vec<Version> = stored().read().map(|stored| stored.versions.clone()).unwrap_or_default();
    extend_versions(known, now)
}

fn next_name(name: &str) -> Option<String> {
    let (major, minor) = name.split_once('.')?;
    let (major, minor): (u32, u32) = (major.parse().ok()?, minor.parse().ok()?);
    Some(if minor >= 8 { format!("{}.0", major + 1) } else { format!("{major}.{}", minor + 1) })
}

fn previous_name(name: &str) -> Option<String> {
    let (major, minor) = name.split_once('.')?;
    let (major, minor): (u32, u32) = (major.parse().ok()?, minor.parse().ok()?);
    if minor == 0 {
        (major > 1).then(|| format!("{}.8", major - 1))
    } else {
        Some(format!("{major}.{}", minor - 1))
    }
}

fn extend_versions(mut known: Vec<Version>, now: i64) -> Vec<Version> {
    known.sort_by_key(|version| version.start);
    known.dedup_by(|a, b| a.name == b.name);
    let (Some(first), Some(last)) = (known.first().cloned(), known.last().cloned()) else {
        return known;
    };
    let mut result = known;
    // Ahead: until 4 months from now.
    let (mut name, mut start) = (last.name, last.start);
    while start < now + 120 * 86400 {
        let Some(next) = next_name(&name) else { break };
        start += VERSION_DAYS * 86400;
        result.push(Version { name: next.clone(), start, known: false });
        name = next;
    }
    // Behind: until KEEP_DAYS ago.
    let (mut name, mut start) = (first.name, first.start);
    while start > now - KEEP_DAYS * 86400 {
        let Some(previous) = previous_name(&name) else { break };
        start -= VERSION_DAYS * 86400;
        result.push(Version { name: previous.clone(), start, known: false });
        name = previous;
    }
    result.sort_by_key(|version| version.start);
    result
}

/// The Spiral Abyss (from the 1st and from the 16th) and the Imaginarium Theater (from the 1st)
/// as the calendar has them, from a while ago to a while ahead.  The days change at 4 o'clock of
/// UTC+8.
fn periodic(now: i64) -> Vec<Event> {
    let zone = zone();
    let at = |year: i32, month: u32, day: u32| -> Option<i64> {
        let time = NaiveDate::from_ymd_opt(year, month, day)?.and_hms_opt(4, 0, 0)?;
        Some(zone.from_local_datetime(&time).single()?.timestamp())
    };
    let today = chrono::DateTime::from_timestamp(now, 0).map(|time| time.with_timezone(&zone).date_naive());
    let Some(today) = today else {
        return Vec::new();
    };
    let (from, to) = (now - KEEP_DAYS * 86400, now + 120 * 86400);
    let mut events = Vec::new();
    let mut starts = Vec::new();
    for shift in -3i32..=5 {
        let index = today.year() * 12 + today.month0() as i32 + shift;
        starts.push((index.div_euclid(12), index.rem_euclid(12) as u32 + 1));
    }
    let mut halves: Vec<i64> = Vec::new();
    for (year, month) in &starts {
        for day in [1, 16] {
            if let Some(time) = at(*year, *month, day) {
                halves.push(time);
            }
        }
    }
    let periodic_event = |en: &str, ru: &str, start: i64, end: i64, kind: Kind| Event {
        id: 0,
        en: en.to_owned(),
        ru: ru.to_owned(),
        start,
        end,
        kind,
        banner: String::new(),
        text_en: String::new(),
        text_ru: String::new(),
        link: String::new(),
        primogems: 0,
    };
    for pair in halves.windows(2) {
        if pair[1] >= from && pair[0] <= to {
            events.push(periodic_event("Spiral Abyss", "Витая Бездна", pair[0], pair[1], Kind::Abyss));
        }
    }
    let months: Vec<i64> = starts.iter().filter_map(|(year, month)| at(*year, *month, 1)).collect();
    for pair in months.windows(2) {
        if pair[1] >= from && pair[0] <= to {
            events.push(periodic_event("Imaginarium Theater", "Театр воображариума", pair[0], pair[1], Kind::Theater));
        }
    }
    events
}

/// Fetches the announcements again if they are old (or `force`), in the background.
pub fn refresh_in_background(ctx: &egui::Context, force: bool) {
    let now = chrono::Utc::now().timestamp();
    if !force && now - fetched_at() < MAX_AGE.as_secs() as i64 {
        return;
    }
    {
        let Ok(mut state) = status().lock() else {
            return;
        };
        if state.running {
            return;
        }
        state.running = true;
        state.error = None;
    }
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(anyhow::Error::from)
            .and_then(|runtime| runtime.block_on(fetch_and_keep()));
        if let Ok(mut state) = status().lock() {
            state.running = false;
            if let Err(e) = result {
                tracing::warn!("the events could not be fetched: {e:#}");
                state.error = Some(format!("{e:#}"));
            }
        }
        ctx.request_repaint();
    });
}

#[derive(Deserialize)]
struct Reply<T> {
    retcode: i32,
    data: Option<T>,
}

#[derive(Deserialize)]
struct ListData {
    /// The hours of the time zone the times of the list are in.
    #[serde(default)]
    timezone: i32,
    #[serde(default)]
    list: Vec<Group>,
}

#[derive(Deserialize)]
struct Group {
    type_id: u32,
    #[serde(default)]
    list: Vec<Announcement>,
}

#[derive(Deserialize)]
struct Announcement {
    ann_id: u64,
    #[serde(default)]
    title: String,
    #[serde(default)]
    banner: String,
    start_time: String,
    end_time: String,
}

#[derive(Deserialize)]
struct ContentData {
    #[serde(default)]
    list: Vec<Content>,
}

#[derive(Deserialize)]
struct Content {
    ann_id: u64,
    #[serde(default)]
    content: String,
    #[serde(default)]
    banner: String,
}

#[derive(Deserialize)]
struct SiteData {
    #[serde(default)]
    list: Vec<Article>,
}

#[derive(Deserialize)]
struct Article {
    #[serde(rename = "iInfoId", deserialize_with = "number_or_text")]
    id: u64,
    #[serde(rename = "sTitle", default)]
    title: String,
    #[serde(rename = "dtStartTime", default)]
    time: String,
}

fn number_or_text<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<u64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Either {
        Number(u64),
        Text(String),
    }
    match Either::deserialize(deserializer)? {
        Either::Number(number) => Ok(number),
        Either::Text(text) => text.parse().map_err(serde::de::Error::custom),
    }
}

async fn get<T: for<'de> Deserialize<'de>>(client: &reqwest::Client, url: &str) -> Result<T> {
    let text = client.get(url).send().await?.error_for_status()?.text().await?;
    let reply: Reply<T> = serde_json::from_str(&text).with_context(|| format!("{url} can not be read"))?;
    if reply.retcode != 0 {
        anyhow::bail!("refused: code {}", reply.retcode);
    }
    reply.data.context("no data")
}

/// Seconds of UTC of a time as the announcements write it (`2026-11-02 05:00:00`), in a zone.
fn parse_time(text: &str, hours: i32) -> Option<i64> {
    let time = NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S").ok()?;
    let zone = FixedOffset::east_opt(hours * 3600)?;
    Some(zone.from_local_datetime(&time).single()?.timestamp())
}

/// Seconds of UTC of a time as the texts write it (`2026/11/24 17:59`, UTC+8).
fn parse_text_time(year: &str, month: &str, day: &str, hour: &str, minute: &str) -> Option<i64> {
    let date = NaiveDate::from_ymd_opt(year.parse().ok()?, month.parse().ok()?, day.parse().ok()?)?;
    let time = date.and_hms_opt(hour.parse().ok()?, minute.parse().ok()?, 0)?;
    Some(zone().from_local_datetime(&time).single()?.timestamp())
}

fn kind_of(group: u32, title: &str) -> Kind {
    let lower = title.to_lowercase();
    if group == 26 {
        Kind::Astral
    } else if lower.starts_with("event wish") || lower.starts_with("молитва") {
        Kind::Banner
    } else {
        Kind::Event
    }
}

/// An announcement as plain text: the paragraphs are lines, the markup is gone.
pub fn plain(html: &str) -> String {
    let tags = Regex::new(r"<[^>]*>").expect("regex");
    let breaks = Regex::new(r"(?i)</p>|<br\s*/?>|</li>|</div>").expect("regex");
    let blank = Regex::new(r"\n\s*\n+").expect("regex");
    let with_breaks = breaks.replace_all(html, "\n");
    let once = tags.replace_all(&with_breaks, "").replace("&lt;", "<").replace("&gt;", ">").replace("&amp;", "&").replace("&nbsp;", " ").replace("&quot;", "\"").replace("&#39;", "'");
    // Times are written inside escaped tags: they are tags after the first pass.
    let twice = tags.replace_all(&once, "");
    blank.replace_all(twice.trim(), "\n").into_owned()
}

/// The text cut to `TEXT_MAX` characters.
fn cut(text: &str) -> String {
    if text.chars().count() <= TEXT_MAX {
        return text.to_owned();
    }
    let mut cut: String = text.chars().take(TEXT_MAX).collect();
    cut.push('…');
    cut
}

/// How many primogems a text promises: "Primogems ×300", "100 Primogems"; what is "per hour" is
/// a rate, not a reward.
pub fn primogems(text: &str) -> u32 {
    let after = Regex::new(r"(?i)primogems?\s*[×x]\s*(\d[\d,]*)").expect("regex");
    let before = Regex::new(r"(?i)(\d[\d,]*)\s*primogems?(\s+per\s+hour)?").expect("regex");
    let number = |text: &str| text.replace(',', "").parse::<u32>().unwrap_or(0);
    let mut total = 0;
    for found in after.captures_iter(text) {
        total += number(&found[1]);
    }
    for found in before.captures_iter(text) {
        if found.get(2).is_none() {
            total += number(&found[1]);
        }
    }
    total
}

/// When an event is, by its text.
#[derive(Debug, Default, PartialEq, Eq)]
struct Schedule {
    /// "After the Version 7.2 update": the version it begins with.
    after_version: Option<String>,
    /// The dates in the part about the duration.
    dates: Vec<i64>,
}

fn schedule(text: &str) -> Schedule {
    let heading = Regex::new(r"(?i)(duration|period)").expect("regex");
    let date = Regex::new(r"(\d{4})/(\d{2})/(\d{2}) (\d{2}):(\d{2})").expect("regex");
    let version = Regex::new(r"(?i)version (\d+\.\d+) update|entirety of version (\d+\.\d+)").expect("regex");
    let Some(found) = heading.find(text) else {
        return Schedule::default();
    };
    let window: String = text[found.start()..].chars().take(500).collect();
    let dates: Vec<(usize, i64)> = date
        .captures_iter(&window)
        .filter_map(|c| Some((c.get(0)?.start(), parse_text_time(&c[1], &c[2], &c[3], &c[4], &c[5])?)))
        .collect();
    let first_date = dates.first().map_or(usize::MAX, |(at, _)| *at);
    let after_version = version
        .captures(&window)
        .filter(|c| c.get(0).is_some_and(|m| m.start() < first_date))
        .and_then(|c| c.get(1).or_else(|| c.get(2)).map(|name| name.as_str().to_owned()));
    Schedule { after_version, dates: dates.into_iter().map(|(_, time)| time).collect() }
}

/// The period of an event, in seconds of UTC: from its text, or from what the list says.
fn period(
    text: &str,
    listed: (i64, i64),
    version_start: impl Fn(&str) -> Option<i64>,
) -> (i64, i64) {
    let plan = schedule(text);
    let (start, end) = match (&plan.after_version, plan.dates.as_slice()) {
        (Some(version), dates) => (version_start(version), dates.first().copied()),
        (None, [from, to, ..]) => (Some(*from), Some(*to)),
        (None, [only]) => (None, Some(*only)),
        (None, []) => (None, None),
    };
    let (start, end) = (start.unwrap_or(listed.0), end.unwrap_or(listed.1));
    if end > start { (start, end) } else { listed }
}

async fn fetch_and_keep() -> Result<()> {
    let client = reqwest::Client::builder()
        .user_agent("Irminsul")
        .timeout(Duration::from_secs(60))
        .build()?;
    let list = |lang: &str| format!("{API}/getAnnList?{QUERY}&lang={lang}");
    let content = |lang: &str| format!("{API}/getAnnContent?{QUERY}&lang={lang}");
    let english: ListData = get(&client, &list("en")).await?;
    let russian: Option<ListData> = get(&client, &list("ru")).await.ok();
    let texts_en: Option<ContentData> = get(&client, &content("en")).await.ok();
    let texts_ru: Option<ContentData> = get(&client, &content("ru")).await.ok();

    // The official site: the dates of the versions, and the articles to link to.
    let mut articles: Vec<Article> = Vec::new();
    for page in 1..=4 {
        match get::<SiteData>(&client, &format!("{SITE}&iPage={page}")).await {
            Ok(data) if !data.list.is_empty() => articles.extend(data.list),
            _ => break,
        }
    }
    let mut versions = Vec::new();
    let version_title = Regex::new(r"Version (\d+\.\d+) Update Details").expect("regex");
    for article in &articles {
        if let Some(found) = version_title.captures(&article.title)
            && let Some(date) = article.time.get(..10)
            && let Some(start) = parse_time(&format!("{date} 11:00:00"), 8)
        {
            versions.push(Version { name: found[1].to_owned(), start, known: true });
        }
    }
    let known_versions = versions.clone();
    let version_start = |name: &str| {
        extend_versions(known_versions.clone(), chrono::Utc::now().timestamp())
            .into_iter()
            .find(|version| version.name == name)
            .map(|version| version.start)
    };

    let by_id = |data: &Option<ContentData>| -> BTreeMap<u64, String> {
        data.iter().flat_map(|data| &data.list).map(|content| (content.ann_id, plain(&content.html_or_text()))).collect()
    };
    let (text_en, text_ru) = (by_id(&texts_en), by_id(&texts_ru));
    let titles_ru: BTreeMap<u64, String> = russian
        .iter()
        .flat_map(|data| &data.list)
        .flat_map(|group| &group.list)
        .map(|announcement| (announcement.ann_id, announcement.title.clone()))
        .collect();
    let covers: BTreeMap<u64, String> = texts_en
        .iter()
        .flat_map(|data| &data.list)
        .map(|content| (content.ann_id, content.banner.clone()))
        .collect();

    let mut fetched = Vec::new();
    for group in english.list.iter().filter(|group| GROUPS.contains(&group.type_id)) {
        for announcement in &group.list {
            let (Some(listed_start), Some(listed_end)) = (
                parse_time(&announcement.start_time, english.timezone),
                parse_time(&announcement.end_time, 8),
            ) else {
                continue;
            };
            let text = text_en.get(&announcement.ann_id).cloned().unwrap_or_default();
            let (start, end) = period(&text, (listed_start, listed_end), version_start);
            if end <= start || end - start > LONGEST_DAYS * 86400 {
                continue;
            }
            let banner = if announcement.banner.is_empty() {
                covers.get(&announcement.ann_id).cloned().unwrap_or_default()
            } else {
                announcement.banner.clone()
            };
            fetched.push(Event {
                id: announcement.ann_id,
                en: announcement.title.clone(),
                ru: titles_ru.get(&announcement.ann_id).cloned().unwrap_or_default(),
                start,
                end,
                kind: kind_of(group.type_id, &announcement.title),
                banner,
                primogems: primogems(&text),
                text_en: cut(&text),
                text_ru: cut(text_ru.get(&announcement.ann_id).map_or("", String::as_str)),
                link: String::new(),
            });
        }
    }
    let all_versions = extend_versions(known_versions.clone(), chrono::Utc::now().timestamp());
    for event in &mut fetched {
        event.link = article_for(event, &articles, &all_versions);
    }

    let now = chrono::Utc::now().timestamp();
    let count = fetched.len();
    let snapshot = {
        let mut stored = stored().write().map_err(|_| anyhow::anyhow!("the events are locked"))?;
        for event in fetched {
            stored.events.insert(event.id, event);
        }
        stored.events.retain(|_, event| event.end > now - KEEP_DAYS * 86400);
        if !versions.is_empty() {
            stored.versions = versions;
        }
        stored.stamp = now;
        serde_json::to_string(&*stored)?
    };
    if let Some(path) = file() {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        std::fs::write(path, snapshot)?;
    }
    tracing::info!("the events were fetched: {count} announcements");
    Ok(())
}

impl Content {
    fn html_or_text(&self) -> String {
        self.content.clone()
    }
}

/// The article of the official site about an event: the one with the same title, else the
/// overview of the version it is in (the notice of the wishes for a wish).
fn article_for(event: &Event, articles: &[Article], versions: &[Version]) -> String {
    let normal = |text: &str| text.to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect::<String>();
    let wanted = normal(&event.en);
    if let Some(article) = articles.iter().find(|article| normal(&article.title) == wanted) {
        return format!("{ARTICLE}{}", article.id);
    }
    let version = versions.iter().rev().find(|version| version.start <= event.start.max(event.end - 1)).map(|version| version.name.clone());
    if let Some(version) = version {
        let (needle, alt) = if event.kind == Kind::Banner {
            (format!("Version {version} Event Wishes Notice"), String::new())
        } else {
            (format!("Version {version} Events Overview"), format!("Version {version} Update Details"))
        };
        // The newest article of the version that fits (the list is newest first).
        let pick = |needle: &str| {
            (!needle.is_empty())
                .then(|| articles.iter().find(|article| article.title.contains(needle)))
                .flatten()
        };
        if let Some(article) = pick(&needle).or_else(|| pick(&alt)) {
            return format!("{ARTICLE}{}", article.id);
        }
    }
    NEWS.to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc8(text: &str) -> i64 {
        parse_time(text, 8).expect("time")
    }

    #[test]
    fn times_are_read_in_a_zone() {
        // 05:00 in UTC+1 is 04:00 UTC.
        assert_eq!(parse_time("2026-11-02 05:00:00", 1), Some(utc8("2026-11-02 12:00:00")));
        assert_eq!(parse_time("nonsense", 1), None);
    }

    #[test]
    fn titles_are_shortened() {
        let wish = r#"Event Wish "Myriad Patterns Unfold" - Boosted drop rate of Mitya"#;
        assert_eq!(shorten(wish, Kind::Banner), "Wish Myriad Patterns Unfold");
        assert_eq!(shorten("Stygian Onslaught Event: Disturbance-affected Ley Line", Kind::Event), "Stygian Onslaught Event");
        assert_eq!(shorten("Short", Kind::Event), "Short");
    }

    #[test]
    fn the_calendar_makes_the_abyss_and_the_theater() {
        let now = 1_794_000_000; // November 2026
        let events = periodic(now);
        let abyss: Vec<&Event> = events.iter().filter(|event| event.kind == Kind::Abyss).collect();
        let theater: Vec<&Event> = events.iter().filter(|event| event.kind == Kind::Theater).collect();
        assert!(abyss.len() >= 6 && theater.len() >= 3);
        for pair in abyss.windows(2) {
            assert_eq!(pair[0].end, pair[1].start);
            let days = (pair[0].end - pair[0].start) / 86400;
            assert!((13..=16).contains(&days), "{days}");
        }
        assert!(abyss.iter().any(|event| event.start <= now && now < event.end));
    }

    #[test]
    fn the_text_tells_when_an_event_is() {
        let banner = "Event Wish Duration\nPromotional Character (5-Star)\nAfter the Version 7.2 update\n—\n2026/11/24 17:59";
        let plan = schedule(banner);
        assert_eq!(plan.after_version.as_deref(), Some("7.2"));
        assert_eq!(plan.dates, vec![utc8("2026-11-24 17:59:00")]);
        let second = "Event Wish Duration\n2026/11/24 18:00 – 2026/12/15 14:59";
        let plan = schedule(second);
        assert_eq!(plan.after_version, None);
        assert_eq!(plan.dates.len(), 2);
        let listed = (1, 2);
        // A version that is not known leaves the list's beginning.
        let (start, end) = period(banner, (utc8("2026-11-02 12:00:00"), utc8("2026-11-24 17:59:00")), |_| Some(utc8("2026-11-04 11:00:00")));
        assert_eq!((start, end), (utc8("2026-11-04 11:00:00"), utc8("2026-11-24 17:59:00")));
        assert_eq!(period("nothing here", listed, |_| None), listed);
    }

    #[test]
    fn primogems_are_counted_from_the_text() {
        let text = "Maintenance Compensation: Primogems ×300 (60 Primogems per hour the servers are down)\nIssue Fix Compensation: Primogems ×300\nYou'll receive 100 Primogems and 10,000 Mora";
        assert_eq!(primogems(text), 700);
        assert_eq!(primogems("rewards including Primogems, Crown of Insight"), 0);
    }

    #[test]
    fn markup_is_taken_out_of_the_text() {
        let html = r#"<p><span>Event Duration: </span>&lt;t class="t_lc"&gt;<span>2026/11/05 09:59</span>&lt;/t&gt;</p><p>Second</p>"#;
        assert_eq!(plain(html), "Event Duration: 2026/11/05 09:59\nSecond");
    }

    #[test]
    fn versions_follow_each_other_by_six_weeks() {
        let seven_one = utc8("2026-09-23 11:00:00");
        let known = vec![
            Version { name: "7.1".into(), start: seven_one, known: true },
            Version { name: "7.2".into(), start: utc8("2026-11-04 11:00:00"), known: true },
        ];
        let all = extend_versions(known, utc8("2026-11-09 12:00:00"));
        let find = |name: &str| all.iter().find(|version| version.name == name).map(|version| version.start);
        assert_eq!(find("7.3"), Some(utc8("2026-12-16 11:00:00")));
        assert_eq!(find("7.0"), Some(utc8("2026-08-12 11:00:00")));
        assert_eq!(next_name("7.8").as_deref(), Some("8.0"));
        assert_eq!(previous_name("7.0").as_deref(), Some("6.8"));
    }

    /// Fetches for real (needs the network) and prints what the timeline would have.
    #[test]
    #[ignore]
    fn live_fetch() {
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().expect("runtime");
        runtime.block_on(fetch_and_keep()).expect("fetched");
        let now = chrono::Utc::now().timestamp();
        let when = |time: i64| chrono::DateTime::from_timestamp(time + 8 * 3600, 0).map(|t| t.format("%m-%d %H:%M").to_string()).unwrap_or_default();
        for event in all(now) {
            println!("{:>6} {:?} {} .. {} pg={} link={} img={} | {}", event.id, event.kind, when(event.start), when(event.end), event.primogems, event.link.rsplit('/').next().unwrap_or(""), !event.banner.is_empty(), event.en.chars().take(50).collect::<String>());
        }
        for version in versions(now) {
            println!("v{} {} known={}", version.name, when(version.start), version.known);
        }
    }

    #[test]
    fn only_events_and_the_astral_plane_are_kept() {
        assert_eq!(kind_of(26, "x"), Kind::Astral);
        assert_eq!(kind_of(1, "Event Wish \"X\""), Kind::Banner);
        assert_eq!(kind_of(1, "Something"), Kind::Event);
        assert_eq!(kind_of(1, "Across the Frozen Wilds Event: New Travelers may make a Wish"), Kind::Event);
        let plan = schedule("Event Duration
Available throughout the entirety of Version 7.2
Eligibility");
        assert_eq!(plan.after_version.as_deref(), Some("7.2"));
    }
}
