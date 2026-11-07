//! Characters and weapons the game has not released yet, and the ones released
//! after the data built into this program was made.
//!
//! The built-in data (`upgrade_data`) is a snapshot of the game's files and
//! only changes with a new version of Irminsul.  So that new content shows up
//! without one, this asks the lunaris.moe database, which keeps the data of
//! the versions to come, what is new in it.  It is checked when the program
//! starts and every few hours after, costs two small requests when nothing is
//! new, and what it finds is kept on disk, so it is there without a network.
//!
//! Nothing about the player is sent; these are plain downloads.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;

use crate::APP_ID;
use crate::upgrade_data::{Cost, Material, Step, Text};
use crate::upgrade_store;

const API: &str = "https://api.lunaris.moe/data";
/// How often to look for something new.
const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);
/// What was found is fetched again after this long even if nothing seems to
/// have changed, since the data of a version being prepared still changes.
const REFETCH_AFTER_SECS: i64 = 24 * 3600;
/// How long the texts of a character are kept before they are asked for again.
const DETAILS_FRESH_SECS: i64 = 7 * 24 * 3600;
/// Changes when what is stored for upcoming content gets more in it, so that
/// what was stored before is fetched again.
const FORMAT: u32 = 4;

/// A talent, constellation or passive: its name and what it says.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Named {
    pub name: Text,
    pub description: Text,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NewCharacter {
    pub id: u32,
    pub name: Text,
    pub icon: String,
    pub rarity: u32,
    pub weapon_type: String,
    /// "Fire", "Water" and so on, as the game's own files name the elements.
    pub element: String,
    /// Stats at the highest level, as the database names them.
    pub stats: Vec<(String, f32)>,
    /// Normal attack, skill, burst.
    pub talents: Vec<Named>,
    pub passives: Vec<Named>,
    pub constellations: Vec<Named>,
    /// What all the ascensions take.
    pub ascension: BTreeMap<u32, u32>,
    /// What taking the three talents to the top takes.
    pub talent_costs: BTreeMap<u32, u32>,
    /// The same, one ascension at a time, if it could be worked out.
    #[serde(default)]
    pub ascension_steps: Vec<Step>,
    /// One talent from level 1 to 10, a level at a time.  The three talents
    /// take the same.
    #[serde(default)]
    pub talent_steps: Vec<Step>,
    /// Chains of materials in which three of one make one of the next, the
    /// lowest first.
    #[serde(default)]
    pub chains: Vec<Vec<u32>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct NewWeapon {
    pub id: u32,
    pub name: Text,
    pub icon: String,
    pub rarity: u32,
    pub weapon_type: String,
    pub stats: Vec<(String, f32)>,
    pub passive: Named,
    pub ascension: BTreeMap<u32, u32>,
    #[serde(default)]
    pub ascension_steps: Vec<Step>,
    #[serde(default)]
    pub chains: Vec<Vec<u32>>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Upcoming {
    #[serde(default)]
    pub format: u32,
    /// The version of the data this was taken from.
    pub version: String,
    /// When it was fetched, in seconds since 1970.
    pub fetched_at: i64,
    pub characters: Vec<NewCharacter>,
    pub weapons: Vec<NewWeapon>,
    /// The ids of all the characters and weapons the game has, as the database
    /// lists them.  The game's own files also hold test and trial ones.
    #[serde(default)]
    pub official_characters: Vec<u32>,
    #[serde(default)]
    pub official_weapons: Vec<u32>,
    /// The materials these need that the built-in data does not know.
    pub materials: BTreeMap<u32, Material>,
}

impl Upcoming {
    /// Whether a character is one of the game's, not a test or trial one.
    /// Until the list is known, the ids that real characters have are used.
    pub fn is_official_character(&self, id: u32) -> bool {
        if self.official_characters.is_empty() {
            (10_000_000..10_000_900).contains(&id)
        } else {
            self.official_characters.contains(&id)
        }
    }

    pub fn is_official_weapon(&self, id: u32) -> bool {
        self.official_weapons.is_empty() || self.official_weapons.contains(&id)
    }

    pub fn is_empty(&self) -> bool {
        self.characters.is_empty() && self.weapons.is_empty()
    }
}

fn state() -> &'static RwLock<Arc<Upcoming>> {
    static STATE: OnceLock<RwLock<Arc<Upcoming>>> = OnceLock::new();
    STATE.get_or_init(|| RwLock::new(Arc::new(load_cache().unwrap_or_default())))
}

/// What is known now.
pub fn current() -> Arc<Upcoming> {
    state().read().map(|state| state.clone()).unwrap_or_default()
}

fn cache_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("IRMINSUL_LIVE_DATA") {
        return Some(path.into());
    }
    Some(eframe::storage_dir(APP_ID)?.join("live_data.json"))
}

fn load_cache() -> Option<Upcoming> {
    let bytes = std::fs::read(cache_path()?).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn save_cache(upcoming: &Upcoming) {
    let Some(path) = cache_path() else {
        return;
    };
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let temp = path.with_extension("json.tmp");
    if let Ok(bytes) = serde_json::to_vec(upcoming)
        && std::fs::write(&temp, bytes).is_ok()
    {
        let _ = std::fs::rename(&temp, &path);
    }
}

/// The texts of a character: what its talents, passives and constellations
/// say.  The data built into the program has only their names.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Details {
    pub fetched_at: i64,
    pub talents: Vec<Named>,
    pub passives: Vec<Named>,
    pub constellations: Vec<Named>,
}

fn details_state() -> &'static RwLock<HashMap<u32, Arc<Details>>> {
    static STATE: OnceLock<RwLock<HashMap<u32, Arc<Details>>>> = OnceLock::new();
    STATE.get_or_init(|| {
        let loaded: HashMap<u32, Details> = details_path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        RwLock::new(
            loaded
                .into_iter()
                .map(|(id, details)| (id, Arc::new(details)))
                .collect(),
        )
    })
}

fn details_path() -> Option<PathBuf> {
    Some(cache_path()?.with_file_name("character_details.json"))
}

fn save_details() {
    let Some(path) = details_path() else {
        return;
    };
    let Ok(state) = details_state().read() else {
        return;
    };
    let plain: HashMap<u32, &Details> = state.iter().map(|(id, d)| (*id, d.as_ref())).collect();
    let temp = path.with_extension("json.tmp");
    if let Ok(bytes) = serde_json::to_vec(&plain)
        && std::fs::write(&temp, bytes).is_ok()
    {
        let _ = std::fs::rename(&temp, &path);
    }
}

/// What the background thread is asked for.
enum Request {
    /// The texts of a character.
    Details(u32),
    /// The names of all the items.
    Catalog,
}

fn requests() -> &'static OnceLock<mpsc::UnboundedSender<Request>> {
    static REQUESTS: OnceLock<mpsc::UnboundedSender<Request>> = OnceLock::new();
    &REQUESTS
}

/// The names and pictures of the items of the game, all of them and not only
/// the ones upgrades use.  Fetched when one is asked for that is not known.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Catalog {
    fetched_at: i64,
    items: HashMap<u32, Material>,
}

fn catalog_path() -> Option<PathBuf> {
    Some(cache_path()?.with_file_name("item_catalog.json"))
}

fn catalog_state() -> &'static RwLock<Arc<Catalog>> {
    static STATE: OnceLock<RwLock<Arc<Catalog>>> = OnceLock::new();
    STATE.get_or_init(|| {
        let loaded: Catalog = catalog_path()
            .and_then(|path| std::fs::read(path).ok())
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default();
        RwLock::new(Arc::new(loaded))
    })
}

/// What an item is called and looks like, if it is known.  If it is not, the
/// list of items is asked for, once an hour at most, and it is known after it
/// has come.
pub fn item(id: u32) -> Option<Material> {
    use std::sync::atomic::{AtomicI64, Ordering};
    static LAST_ASKED: AtomicI64 = AtomicI64::new(0);
    let catalog = catalog_state().read().ok()?.clone();
    if let Some(material) = catalog.items.get(&id) {
        return Some(material.clone());
    }
    let now = now_secs();
    if now - LAST_ASKED.load(Ordering::Relaxed) > 3600
        && now - catalog.fetched_at > 3600
        && let Some(sender) = requests().get()
    {
        LAST_ASKED.store(now, Ordering::Relaxed);
        let _ = sender.send(Request::Catalog);
    }
    None
}

async fn fetch_catalog(client: &reqwest::Client) -> Result<Catalog> {
    let known = current().version.clone();
    let version = if known.is_empty() {
        get_json(client, &format!("{API}/version.json")).await?["version"]
            .as_str()
            .context("no version")?
            .to_owned()
    } else {
        known
    };
    let list = get_json(client, &format!("{API}/{version}/materiallist.json")).await?;
    let items = list
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, entry)| {
            let id = id_of(key)?;
            let named = entry["enName"].as_str().is_some_and(|name| !name.is_empty());
            named.then(|| (id, material(id, entry)))
        })
        .collect();
    Ok(Catalog {
        fetched_at: now_secs(),
        items,
    })
}

/// What the texts of a character say, if they are known yet.  They are asked
/// for in the background the first time, and again when they are old.
pub fn details(avatar_id: u32) -> Option<Arc<Details>> {
    static ASKED: OnceLock<Mutex<HashSet<u32>>> = OnceLock::new();
    let have = details_state()
        .read()
        .ok()
        .and_then(|state| state.get(&avatar_id).cloned());
    let stale = have
        .as_ref()
        .is_none_or(|details| now_secs() - details.fetched_at > DETAILS_FRESH_SECS);
    if stale
        && let Some(sender) = requests().get()
        && let Ok(mut asked) = ASKED.get_or_init(Default::default).lock()
        && asked.insert(avatar_id)
    {
        let _ = sender.send(Request::Details(avatar_id));
    }
    have
}

/// Starts the checks in the background.  `ctx` is told to redraw when
/// something new arrives.
pub fn start(ctx: &egui::Context) {
    if std::env::var_os("IRMINSUL_NO_LIVE_DATA").is_some() {
        return;
    }
    let (sender, mut receiver) = mpsc::unbounded_channel::<Request>();
    if requests().set(sender).is_err() {
        return;
    }
    let ctx = ctx.clone();
    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(runtime) => runtime,
            Err(e) => {
                tracing::error!("unable to start the game data check: {e}");
                return;
            }
        };
        runtime.block_on(async move {
            let client = match reqwest::Client::builder()
                .user_agent("Irminsul")
                .timeout(Duration::from_secs(90))
                .build()
            {
                Ok(client) => client,
                Err(e) => {
                    tracing::error!("unable to start the game data check: {e}");
                    return;
                }
            };
            let mut next_check = tokio::time::Instant::now();
            loop {
                tokio::select! {
                    _ = tokio::time::sleep_until(next_check) => {
                        match refresh(&client).await {
                            Ok(Some(new)) => {
                                tracing::info!(
                                    "new game data {}: {} characters, {} weapons",
                                    new.version,
                                    new.characters.len(),
                                    new.weapons.len()
                                );
                                save_cache(&new);
                                if let Ok(mut state) = state().write() {
                                    *state = Arc::new(new);
                                }
                                ctx.request_repaint();
                            }
                            Ok(None) => {}
                            Err(e) => tracing::warn!("unable to check for new game data: {e:#}"),
                        }
                        next_check = tokio::time::Instant::now() + CHECK_EVERY;
                    }
                    Some(request) = receiver.recv() => match request {
                        Request::Details(id) => match fetch_details(&client, id).await {
                            Ok(details) => {
                                if let Ok(mut state) = details_state().write() {
                                    state.insert(id, Arc::new(details));
                                }
                                save_details();
                                ctx.request_repaint();
                            }
                            Err(e) => tracing::warn!("no texts for character {id}: {e:#}"),
                        },
                        Request::Catalog => match fetch_catalog(&client).await {
                            Ok(catalog) => {
                                tracing::info!("names of {} items", catalog.items.len());
                                if let Some(path) = catalog_path()
                                    && let Ok(bytes) = serde_json::to_vec(&catalog)
                                {
                                    let _ = std::fs::write(path, bytes);
                                }
                                if let Ok(mut state) = catalog_state().write() {
                                    *state = Arc::new(catalog);
                                }
                                ctx.request_repaint();
                            }
                            Err(e) => tracing::warn!("no names of the items: {e:#}"),
                        },
                    }
                }
            }
        });
    });
}

async fn fetch_details(client: &reqwest::Client, id: u32) -> Result<Details> {
    let known = current().version.clone();
    let version = if known.is_empty() {
        get_json(client, &format!("{API}/version.json")).await?["version"]
            .as_str()
            .context("no version")?
            .to_owned()
    } else {
        known
    };
    let en = get_json(client, &format!("{API}/{version}/en/char/{id}.json")).await?;
    let ru = get_json(client, &format!("{API}/{version}/ru/char/{id}.json"))
        .await
        .unwrap_or(Value::Null);
    Ok(Details {
        fetched_at: now_secs(),
        talents: talents(&en, &ru),
        passives: named_group(&en["passives"], &ru["passives"]),
        constellations: named_group(&en["constellations"], &ru["constellations"]),
    })
}

fn talents(en: &Value, ru: &Value) -> Vec<Named> {
    ["normalattack", "elementalskill", "elementalburst"]
        .iter()
        .map(|kind| named(&en["skills"][kind], &ru["skills"][kind]))
        .collect()
}

// ---------------------------------------------------------------------------
// What one ascension or one level of a talent takes.
//
// Every character takes the same of each kind of item, whatever its rarity;
// only which items they are changes.  Weapons are the same within a rarity.
// The database gives only the totals, so the steps are worked out from these
// templates, and used only when they add up to those totals.

/// The mora, and the items as (kind of item, tier, count).
type TemplateStep = (u32, &'static [(usize, usize, u32)]);

/// Kinds: 0 gems of the element (4 tiers), 1 the boss's drop, 2 the local
/// specialty, 3 the common enemies' drops (3 tiers).
const CHARACTER_ASCENSION: [TemplateStep; 6] = [
    (20000, &[(0, 0, 1), (2, 0, 3), (3, 0, 3)]),
    (40000, &[(0, 1, 3), (1, 0, 2), (2, 0, 10), (3, 0, 15)]),
    (60000, &[(0, 1, 6), (1, 0, 4), (2, 0, 20), (3, 1, 12)]),
    (80000, &[(0, 2, 3), (1, 0, 8), (2, 0, 30), (3, 1, 18)]),
    (100000, &[(0, 2, 6), (1, 0, 12), (2, 0, 45), (3, 2, 12)]),
    (120000, &[(0, 3, 6), (1, 0, 20), (2, 0, 60), (3, 2, 24)]),
];

/// Kinds: 0 the books (3 tiers), 1 the common enemies' drops (3 tiers), 2 the
/// weekly boss's drop, 3 the Crown of Insight.  Levels 2 to 10 of one talent.
const TALENT: [TemplateStep; 9] = [
    (12500, &[(0, 0, 3), (1, 0, 6)]),
    (17500, &[(0, 1, 2), (1, 1, 3)]),
    (25000, &[(0, 1, 4), (1, 1, 4)]),
    (30000, &[(0, 1, 6), (1, 1, 6)]),
    (37500, &[(0, 1, 9), (1, 1, 9)]),
    (120000, &[(0, 2, 4), (1, 2, 4), (2, 0, 1)]),
    (260000, &[(0, 2, 6), (1, 2, 6), (2, 0, 1)]),
    (450000, &[(0, 2, 12), (1, 2, 9), (2, 0, 2)]),
    (700000, &[(0, 2, 16), (1, 2, 12), (2, 0, 2), (3, 0, 1)]),
];

/// Weapons.  Kinds: 0 and 1 the drops of two kinds of enemies (3 tiers each),
/// 2 the drop of the domain (4 tiers).
const WEAPON_5: [TemplateStep; 6] = [
    (10000, &[(0, 0, 3), (1, 0, 5), (2, 0, 5)]),
    (20000, &[(0, 0, 12), (1, 0, 18), (2, 1, 5)]),
    (30000, &[(0, 1, 9), (1, 1, 9), (2, 1, 9)]),
    (45000, &[(0, 1, 14), (1, 1, 18), (2, 2, 5)]),
    (55000, &[(0, 2, 9), (1, 2, 14), (2, 2, 9)]),
    (65000, &[(0, 2, 18), (1, 2, 27), (2, 3, 6)]),
];
const WEAPON_4: [TemplateStep; 6] = [
    (5000, &[(0, 0, 2), (1, 0, 3), (2, 0, 3)]),
    (15000, &[(0, 0, 8), (1, 0, 12), (2, 1, 3)]),
    (20000, &[(0, 1, 6), (1, 1, 6), (2, 1, 6)]),
    (30000, &[(0, 1, 9), (1, 1, 12), (2, 2, 3)]),
    (35000, &[(0, 2, 6), (1, 2, 9), (2, 2, 6)]),
    (45000, &[(0, 2, 12), (1, 2, 18), (2, 3, 4)]),
];
const WEAPON_3: [TemplateStep; 6] = [
    (5000, &[(0, 0, 1), (1, 0, 2), (2, 0, 2)]),
    (10000, &[(0, 0, 5), (1, 0, 8), (2, 1, 2)]),
    (15000, &[(0, 1, 4), (1, 1, 4), (2, 1, 4)]),
    (20000, &[(0, 1, 6), (1, 1, 8), (2, 2, 2)]),
    (25000, &[(0, 2, 4), (1, 2, 6), (2, 2, 4)]),
    (30000, &[(0, 2, 8), (1, 2, 12), (2, 3, 3)]),
];

/// The steps of a template with the items of `kinds` in place, if all are
/// there.
fn build_steps(template: &[TemplateStep], kinds: &[Vec<u32>]) -> Option<Vec<Step>> {
    template
        .iter()
        .map(|(mora, items)| {
            let items: Option<Vec<Cost>> = items
                .iter()
                .map(|(kind, tier, count)| {
                    Some(Cost {
                        id: *kinds.get(*kind)?.get(*tier)?,
                        count: *count,
                    })
                })
                .collect();
            Some(Step {
                mora: *mora,
                items: items?,
            })
        })
        .collect()
}

/// What the items of the steps add up to, `times` over, without the mora.
fn item_totals(steps: &[Step], times: u32) -> BTreeMap<u32, u32> {
    let mut totals = BTreeMap::new();
    for cost in steps.iter().flat_map(|step| &step.items) {
        *totals.entry(cost.id).or_default() += cost.count * times;
    }
    totals
}

/// The ids of a group of items, `{"id": count}`, lowest first.
fn ids_of(group: &Value) -> Vec<u32> {
    let mut ids: Vec<u32> = group
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, _)| id_of(key))
        .collect();
    ids.sort();
    ids
}

fn without_mora(costs: &BTreeMap<u32, u32>) -> BTreeMap<u32, u32> {
    let mut costs = costs.clone();
    costs.remove(&202);
    costs
}

/// The steps of a character's ascensions and of one of its talents, when the
/// templates fit what the database says is needed in total.
fn character_steps(
    en: &Value,
    ascension: &BTreeMap<u32, u32>,
    talent_costs: &BTreeMap<u32, u32>,
) -> (Vec<Step>, Vec<Step>) {
    let asc = &en["ascension"];
    let ascension_kinds = [
        ids_of(&asc["elemental"]),
        ids_of(&asc["elitemonster"]),
        ids_of(&asc["speciality"]),
        ids_of(&asc["worldmonster"]),
    ];
    let leveling = &en["skills"]["leveling"];
    let talent_kinds = [
        ids_of(&leveling["books"]),
        ids_of(&leveling["worldmonster"]),
        ids_of(&leveling["weekly"]),
        ids_of(&leveling["crown"]),
    ];
    let ascension_steps = build_steps(&CHARACTER_ASCENSION, &ascension_kinds)
        .filter(|steps| item_totals(steps, 1) == without_mora(ascension))
        .unwrap_or_default();
    let talent_steps = build_steps(&TALENT, &talent_kinds)
        .filter(|steps| item_totals(steps, 3) == without_mora(talent_costs))
        .unwrap_or_default();
    (ascension_steps, talent_steps)
}

/// The steps of a weapon's ascensions.  `items` are the ids in the order the
/// database lists them: 3, 3 and 4 of them.
fn weapon_steps(rarity: u32, items: &[u32], ascension: &BTreeMap<u32, u32>) -> Vec<Step> {
    let template = match rarity {
        5 => &WEAPON_5,
        4 => &WEAPON_4,
        3 => &WEAPON_3,
        _ => return Vec::new(),
    };
    if items.len() != 10 {
        return Vec::new();
    }
    let kinds = [items[0..3].to_vec(), items[3..6].to_vec(), items[6..10].to_vec()];
    build_steps(template, &kinds)
        .filter(|steps| item_totals(steps, 1) == without_mora(ascension))
        .unwrap_or_default()
}

async fn get_json(client: &reqwest::Client, url: &str) -> Result<Value> {
    let response = client
        .get(url)
        .send()
        .await
        .with_context(|| format!("request to {url} failed"))?
        .error_for_status()
        .with_context(|| format!("request to {url} failed"))?;
    response
        .json()
        .await
        .with_context(|| format!("{url} is not what was expected"))
}

fn now_secs() -> i64 {
    chrono::Utc::now().timestamp()
}

fn id_of(key: &str) -> Option<u32> {
    key.parse().ok()
}

/// The entries of a list that the built-in data does not have.
fn missing_ids(list: &Value, known: impl Fn(u32) -> bool) -> BTreeSet<u32> {
    list.as_object()
        .into_iter()
        .flatten()
        .filter_map(|(key, entry)| {
            let id = id_of(key)?;
            let named = entry["enName"].as_str().is_some_and(|name| !name.is_empty());
            (named && !known(id)).then_some(id)
        })
        .collect()
}

async fn refresh(client: &reqwest::Client) -> Result<Option<Upcoming>> {
    let have = current();
    let version = get_json(client, &format!("{API}/version.json")).await?["version"]
        .as_str()
        .context("no version")?
        .to_owned();

    let characters = get_json(client, &format!("{API}/{version}/charlist.json")).await?;
    let weapons = get_json(client, &format!("{API}/{version}/weaponlist.json")).await?;

    let built_in = upgrade_store::data();
    let new_characters = missing_ids(&characters, |id| built_in.avatars.contains_key(&id));
    let new_weapons = missing_ids(&weapons, |id| built_in.weapons.contains_key(&id));

    let same_content = have.version == version
        && have.format == FORMAT
        && have.characters.iter().map(|c| c.id).collect::<BTreeSet<_>>() == new_characters
        && have.weapons.iter().map(|w| w.id).collect::<BTreeSet<_>>() == new_weapons;
    if same_content && now_secs() - have.fetched_at < REFETCH_AFTER_SECS {
        return Ok(None);
    }

    let ids = |list: &Value| -> Vec<u32> {
        list.as_object()
            .into_iter()
            .flatten()
            .filter_map(|(key, _)| id_of(key))
            .collect()
    };
    let mut upcoming = Upcoming {
        format: FORMAT,
        version: version.clone(),
        fetched_at: now_secs(),
        official_characters: ids(&characters),
        official_weapons: ids(&weapons),
        ..Default::default()
    };
    for id in &new_characters {
        match character(client, &version, *id, &characters[id.to_string()]).await {
            Ok(character) => upcoming.characters.push(character),
            Err(e) => tracing::warn!("no data for character {id}: {e:#}"),
        }
    }
    for id in &new_weapons {
        match weapon(client, &version, *id, &weapons[id.to_string()]).await {
            Ok(weapon) => upcoming.weapons.push(weapon),
            Err(e) => tracing::warn!("no data for weapon {id}: {e:#}"),
        }
    }
    // The rarest first.
    upcoming
        .characters
        .sort_by_key(|c| (std::cmp::Reverse(c.rarity), std::cmp::Reverse(c.id)));
    upcoming
        .weapons
        .sort_by_key(|w| (std::cmp::Reverse(w.rarity), std::cmp::Reverse(w.id)));

    // Names and pictures of the materials that are new too.
    let mut needed: BTreeSet<u32> = BTreeSet::new();
    for costs in upcoming
        .characters
        .iter()
        .flat_map(|c| [&c.ascension, &c.talent_costs])
        .chain(upcoming.weapons.iter().map(|w| &w.ascension))
    {
        needed.extend(costs.keys().copied());
    }
    needed.retain(|id| !built_in.materials.contains_key(id));
    if !needed.is_empty() {
        match get_json(client, &format!("{API}/{version}/materiallist.json")).await {
            Ok(list) => {
                for id in needed {
                    if let Some(entry) = list.get(id.to_string()) {
                        upcoming.materials.insert(id, material(id, entry));
                    }
                }
            }
            Err(e) => tracing::warn!("no names for the new materials: {e:#}"),
        }
    }
    Ok(Some(upcoming))
}

fn rarity(quality: &str) -> u32 {
    match quality {
        "QUALITY_ORANGE" | "QUALITY_ORANGE_SP" => 5,
        "QUALITY_PURPLE" => 4,
        "QUALITY_BLUE" => 3,
        "QUALITY_GREEN" => 2,
        _ => 1,
    }
}

fn text(en: Option<&str>, ru: Option<&str>) -> Text {
    let en = en.unwrap_or_default().to_owned();
    let ru = ru.map_or_else(|| en.clone(), str::to_owned);
    Text { en, ru }
}

fn material(id: u32, entry: &Value) -> Material {
    Material {
        name: text(entry["enName"].as_str(), entry["ruName"].as_str()),
        icon: entry["icon"]
            .as_str()
            .map_or_else(|| format!("UI_ItemIcon_{id}"), str::to_owned),
        category: Text {
            en: entry["type"].as_str().unwrap_or_default().to_owned(),
            ru: entry["type"].as_str().unwrap_or_default().to_owned(),
        },
        rarity: rarity(entry["qualityType"].as_str().unwrap_or_default()),
    }
}

/// The game's name of an element, from how the database writes it.
fn game_element(name: &str) -> &'static str {
    match name {
        "Pyro" => "Fire",
        "Hydro" => "Water",
        "Anemo" => "Wind",
        "Electro" => "Electric",
        "Dendro" => "Grass",
        "Cryo" => "Ice",
        "Geo" => "Rock",
        _ => "",
    }
}

fn stats(value: &Value) -> Vec<(String, f32)> {
    let mut stats: Vec<(String, f32)> = value
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(name, value)| Some((name.clone(), value.as_f64()? as f32)))
        .collect();
    // HP, ATK, DEF first, then the one that ascensions add.
    let order = |name: &str| match name {
        "hp" => 0,
        "atk" => 1,
        "def" => 2,
        _ => 3,
    };
    stats.sort_by_key(|(name, _)| order(name));
    stats
}

/// What the database writes in descriptions, as plain text.
pub fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(['<', '{']) {
        out.push_str(&rest[..start]);
        let closing = if rest[start..].starts_with('<') { '>' } else { '}' };
        match rest[start..].find(closing) {
            Some(end) => rest = &rest[start + end + 1..],
            None => {
                rest = "";
                break;
            }
        }
    }
    out.push_str(rest);
    out.replace("\\n", "\n")
}

/// Adds up the items of `value`, which is either `{"id": count}` or a group of
/// those, into `costs`.
fn add_costs(costs: &mut BTreeMap<u32, u32>, key: &str, value: &Value) {
    match value {
        Value::Number(count) => {
            if let (Some(id), Some(count)) = (id_of(key), count.as_u64()) {
                *costs.entry(id).or_default() += count as u32;
            }
        }
        Value::Object(group) => {
            for (key, value) in group {
                add_costs(costs, key, value);
            }
        }
        _ => {}
    }
}

fn named(en: &Value, ru: &Value) -> Named {
    Named {
        name: text(en["name"].as_str(), ru["name"].as_str()),
        description: Text {
            en: clean(en["description"].as_str().unwrap_or_default()),
            ru: clean(
                ru["description"]
                    .as_str()
                    .or(en["description"].as_str())
                    .unwrap_or_default(),
            ),
        },
    }
}

fn named_group(en: &Value, ru: &Value) -> Vec<Named> {
    let mut group: Vec<(&String, &Value)> = en
        .as_object()
        .into_iter()
        .flatten()
        .collect();
    // p1, p2 ... const1, const2 ...: by the number, not as text.
    group.sort_by_key(|(key, _)| {
        key.trim_start_matches(|c: char| !c.is_ascii_digit())
            .parse::<u32>()
            .unwrap_or(0)
    });
    group
        .into_iter()
        .map(|(key, value)| named(value, &ru[key.as_str()]))
        .collect()
}

async fn character(
    client: &reqwest::Client,
    version: &str,
    id: u32,
    entry: &Value,
) -> Result<NewCharacter> {
    let en = get_json(client, &format!("{API}/{version}/en/char/{id}.json")).await?;
    let ru = get_json(client, &format!("{API}/{version}/ru/char/{id}.json"))
        .await
        .unwrap_or(Value::Null);

    let mut ascension = BTreeMap::new();
    for (key, value) in en["ascension"].as_object().into_iter().flatten() {
        // What the levels cost is not counted, like elsewhere in Irminsul.
        if key != "exp" {
            add_costs(&mut ascension, key, value);
        }
    }
    let mut talent_costs = BTreeMap::new();
    for (key, value) in en["skills"]["leveling"].as_object().into_iter().flatten() {
        add_costs(&mut talent_costs, key, value);
    }

    let talents = talents(&en, &ru);
    let (ascension_steps, talent_steps) = character_steps(&en, &ascension, &talent_costs);
    let mut chains: Vec<Vec<u32>> = Vec::new();
    for group in [
        &en["ascension"]["elemental"],
        &en["ascension"]["worldmonster"],
        &en["skills"]["leveling"]["worldmonster"],
    ] {
        let ids = ids_of(group);
        if ids.len() >= 2 && !chains.contains(&ids) {
            chains.push(ids);
        }
    }

    Ok(NewCharacter {
        id,
        name: text(entry["enName"].as_str(), entry["ruName"].as_str()),
        icon: en["icons"]["forward"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        rarity: rarity(entry["qualityType"].as_str().unwrap_or_default()),
        weapon_type: entry["weaponType"].as_str().unwrap_or_default().to_owned(),
        element: game_element(entry["element"].as_str().unwrap_or_default()).to_owned(),
        stats: stats(&entry["ascensionStats"]),
        talents,
        passives: named_group(&en["passives"], &ru["passives"]),
        constellations: named_group(&en["constellations"], &ru["constellations"]),
        ascension,
        talent_costs,
        ascension_steps,
        talent_steps,
        chains,
    })
}

async fn weapon(
    client: &reqwest::Client,
    version: &str,
    id: u32,
    entry: &Value,
) -> Result<NewWeapon> {
    let en = get_json(client, &format!("{API}/{version}/en/weapon/{id}.json")).await?;
    let ru = get_json(client, &format!("{API}/{version}/ru/weapon/{id}.json"))
        .await
        .unwrap_or(Value::Null);

    let mut ascension = BTreeMap::new();
    let mut listed = Vec::new();
    for item in en["ascension"].as_array().into_iter().flatten() {
        // The items are named by their pictures, "UI_ItemIcon_<id>".
        let id = item["icon"]
            .as_str()
            .and_then(|icon| icon.strip_prefix("UI_ItemIcon_"))
            .and_then(id_of);
        if let (Some(id), Some(count)) = (id, item["count"].as_u64()) {
            *ascension.entry(id).or_default() += count as u32;
            if id != 202 {
                listed.push(id);
            }
        }
    }
    let rarity = rarity(entry["qualityType"].as_str().unwrap_or_default());
    let ascension_steps = weapon_steps(rarity, &listed, &ascension);
    let chains = if listed.len() == 10 {
        vec![
            listed[0..3].to_vec(),
            listed[3..6].to_vec(),
            listed[6..10].to_vec(),
        ]
    } else {
        Vec::new()
    };

    // The passive as it reads at the first refinement.
    let passive = |value: &Value| {
        (
            value["passive"]["name"].as_str().map(str::to_owned),
            value["passive"]["refinements"]["1"]["description"]
                .as_str()
                .map(clean),
        )
    };
    let (en_name, en_text) = passive(&en);
    let (ru_name, ru_text) = passive(&ru);
    let passive = Named {
        name: text(en_name.as_deref(), ru_name.as_deref()),
        description: text(en_text.as_deref(), ru_text.as_deref()),
    };

    // The stats at the highest level.
    let top = en["stats"]
        .as_object()
        .into_iter()
        .flatten()
        .filter_map(|(level, value)| Some((level.parse::<u32>().ok()?, value)))
        .max_by_key(|(level, _)| *level)
        .map(|(_, value)| stats(value))
        .unwrap_or_default();

    Ok(NewWeapon {
        id,
        name: text(entry["enName"].as_str(), entry["ruName"].as_str()),
        icon: entry["weaponIcon"].as_str().unwrap_or_default().to_owned(),
        rarity,
        weapon_type: entry["weaponType"].as_str().unwrap_or_default().to_owned(),
        stats: top,
        passive,
        ascension,
        ascension_steps,
        chains,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptions_lose_their_markup() {
        let text = "Deals <color=#99FFFFFF>Cryo DMG</color> to {LINK#N1}<color=#FFD780FF>foes</color>{/LINK}.\\nNext";
        assert_eq!(clean(text), "Deals Cryo DMG to foes.\nNext");
        assert_eq!(clean("a < b"), "a ");
    }

    #[test]
    fn costs_are_added_up_across_groups() {
        let value: Value = serde_json::json!({
            "202": 100,
            "exp": {"104001": 2},
            "elemental": {"104161": 1, "104162": 9},
            "worldmonster": {"104162": 1}
        });
        let mut costs = BTreeMap::new();
        for (key, value) in value.as_object().unwrap() {
            add_costs(&mut costs, key, value);
        }
        assert_eq!(costs[&202], 100);
        assert_eq!(costs[&104162], 10);
    }

    #[test]
    fn templates_add_up_like_the_database_says() {
        // The items of Anastasya, as the database lists them.
        let en = serde_json::json!({
            "ascension": {
                "202": 1,
                "elemental": {"104161": 1, "104162": 9, "104163": 9, "104164": 6},
                "speciality": {"101273": 168},
                "worldmonster": {"112149": 18, "112150": 30, "112151": 36},
                "elitemonster": {"113093": 46}
            },
            "skills": {"leveling": {
                "crown": {"104319": 3},
                "books": {"104365": 9, "104366": 63, "104367": 114},
                "worldmonster": {"112149": 18, "112150": 66, "112151": 93},
                "weekly": {"113095": 18}
            }}
        });
        let mut ascension = BTreeMap::new();
        for (key, value) in en["ascension"].as_object().unwrap() {
            if key != "202" {
                add_costs(&mut ascension, key, value);
            }
        }
        let mut talents = BTreeMap::new();
        for (key, value) in en["skills"]["leveling"].as_object().unwrap() {
            add_costs(&mut talents, key, value);
        }
        let (ascension_steps, talent_steps) = character_steps(&en, &ascension, &talents);
        assert_eq!(ascension_steps.len(), 6);
        assert_eq!(talent_steps.len(), 9);
        // One talent to level 10: 3, 21 and 38 books, 6 of the weekly drop.
        let one = item_totals(&talent_steps, 1);
        assert_eq!((one[&104365], one[&104366], one[&104367]), (3, 21, 38));
        assert_eq!(one[&113095], 6);
    }

    #[test]
    fn weapon_templates_add_up() {
        // Aquila Favonia's totals.
        let items = [
            (112011, 15), (112012, 23), (112013, 27), (112014, 23), (112015, 27),
            (112016, 41), (114001, 5), (114002, 14), (114003, 14), (114004, 6),
        ];
        let totals: BTreeMap<u32, u32> = items.into_iter().collect();
        let ids: Vec<u32> = items.iter().map(|(id, _)| *id).collect();
        assert_eq!(weapon_steps(5, &ids, &totals).len(), 6);
        assert!(weapon_steps(4, &ids, &totals).is_empty());
    }

    #[test]
    fn only_missing_entries_are_new() {
        let list = serde_json::json!({
            "1": {"enName": "Known"},
            "2": {"enName": "New"},
            "3": {"enName": ""}
        });
        assert_eq!(missing_ids(&list, |id| id == 1), BTreeSet::from([2]));
    }
}
