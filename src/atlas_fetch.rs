//! Fetching what the official interactive map says: the kinds of marker, where
//! each marker is, and the maps there are (Teyvat, Enkanomiya, the Chasm and
//! the rest) with their regions and their underground places and floors.
//!
//! The program uses this to keep its data up to date, and `examples/gen_map_markers.rs`
//! to make what the program is built with (it includes this file as it is, so
//! this file uses nothing of the program).

use std::collections::HashMap;
use std::io::Write;

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};

const API: &str = "https://sg-public-api-static.hoyolab.com/common/map_user/ys_obc";

/// The maps of the official interactive map, the world first.
pub const MAPS: [u32; 7] = [2, 7, 9, 34, 36, 37, 40];

/// What was fetched, as it is kept: the kinds of marker, the maps, and the
/// markers (as a compressed list).
pub struct Fetched {
    pub labels: String,
    pub atlas: String,
    pub points_gz: Vec<u8>,
    pub kinds: usize,
    pub markers: usize,
}

async fn get(client: &reqwest::Client, path: &str, map: u32, lang: &str) -> Result<Value> {
    let url = format!("{API}/{path}?map_id={map}&app_sn=ys_obc&lang={lang}");
    let body: Value = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("request to {url} failed"))?
        .error_for_status()?
        .json()
        .await?;
    if body["retcode"].as_i64().unwrap_or(0) != 0 {
        bail!("{url}: {}", body["message"].as_str().unwrap_or("refused"));
    }
    Ok(body)
}

/// What a kind of marker needs of its place in the tree: its group, and the
/// time its group says if it has none.
fn flatten(nodes: &[Value], parent: u64, inherited: u64, out: &mut Vec<(u64, u64, u64, Value)>) {
    for node in nodes {
        let id = node["id"].as_u64().unwrap_or(0);
        let own = node["rec_refresh_hour"].as_u64().unwrap_or(0);
        let refresh = if own > 0 { own } else { inherited };
        out.push((id, parent, refresh, node.clone()));
        let children = node["children"].as_array().cloned().unwrap_or_default();
        flatten(&children, id, if parent == 0 { own } else { refresh }, out);
    }
}

async fn labels(client: &reqwest::Client) -> Result<Vec<Value>> {
    let en = get(client, "v2/map/label/tree", 2, "en-us").await?;
    let ru = get(client, "v2/map/label/tree", 2, "ru-ru").await?;
    let mut english = Vec::new();
    let mut russian = Vec::new();
    flatten(en["data"]["tree"].as_array().context("no tree")?, 0, 0, &mut english);
    flatten(ru["data"]["tree"].as_array().context("no tree")?, 0, 0, &mut russian);
    let russian: HashMap<u64, &Value> = russian.iter().map(|(id, _, _, node)| (*id, node)).collect();
    let labels: Vec<Value> = english
        .iter()
        .map(|(id, parent, refresh, node)| {
            json!({
                "id": id,
                "parent": parent,
                "en": node["name"],
                "ru": russian.get(id).and_then(|node| node["name"].as_str()).unwrap_or(""),
                "icon": node["icon"],
                "refresh": refresh,
                "item": node["item_id"],
            })
        })
        .collect();
    if labels.len() < 100 {
        bail!("the list of kinds of marker is too short ({})", labels.len());
    }
    Ok(labels)
}

fn number(value: &Value) -> f64 {
    value.as_f64().or_else(|| value.as_str().and_then(|text| text.parse().ok())).unwrap_or(0.)
}

fn rounded(value: f64) -> f64 {
    (value * 10.).round() / 10.
}

fn rect_of(node: &Value, keys: [&str; 4]) -> Value {
    json!([
        rounded(number(&node[keys[0]])),
        rounded(number(&node[keys[1]])),
        rounded(number(&node[keys[2]])),
        rounded(number(&node[keys[3]]))
    ])
}

/// The regions of a map, as a flat list: each with the one it is in.
fn anchors(nodes: &[Value], parent: u64, russian: &HashMap<u64, String>, out: &mut Vec<Value>) {
    for node in nodes {
        let id = number(&node["id"]) as u64;
        out.push(json!({
            "id": id,
            "parent": parent,
            "area": node["area_id"].as_u64().unwrap_or(0),
            "en": node["name"],
            "ru": russian.get(&id).cloned().unwrap_or_default(),
            "rect": rect_of(node, ["l_x", "l_y", "r_x", "r_y"]),
        }));
        let children = node["children"].as_array().cloned().unwrap_or_default();
        anchors(&children, id, russian, out);
    }
}

fn names_of_anchors(nodes: &[Value], out: &mut HashMap<u64, String>) {
    for node in nodes {
        out.insert(number(&node["id"]) as u64, node["name"].as_str().unwrap_or("").to_owned());
        let children = node["children"].as_array().cloned().unwrap_or_default();
        names_of_anchors(&children, out);
    }
}

/// What one map is: how it is drawn, where its regions and underground places
/// are, and its markers (appended to `rows`).
async fn map(client: &reqwest::Client, id: u32, rows: &mut Vec<Value>) -> Result<Value> {
    let info = get(client, "v1/map/info", id, "en-us").await?;
    let info_ru = get(client, "v1/map/info", id, "ru-ru").await?;
    let detail = &info["data"]["info"]["detail_v2"];
    let pair = |key: &str| -> [f64; 2] {
        let list = detail[key].as_array().cloned().unwrap_or_default();
        [list.first().map_or(0., number), list.get(1).map_or(0., number)]
    };
    let version = detail["map_version"].as_str().context("no version")?;
    let (origin, size) = (pair("origin"), pair("total_size"));
    if version.is_empty() || size[0] <= 0. {
        bail!("the map {id} has no picture");
    }

    let anchors_en = get(client, "v1/map/map_anchor/list", id, "en-us").await?;
    let anchors_ru = get(client, "v1/map/map_anchor/list", id, "ru-ru").await?;
    let list_en = anchors_en["data"]["list"].as_array().cloned().unwrap_or_default();
    let list_ru = anchors_ru["data"]["list"].as_array().cloned().unwrap_or_default();
    let mut names = HashMap::new();
    names_of_anchors(&list_ru, &mut names);
    let mut regions = Vec::new();
    anchors(&list_en, 0, &names, &mut regions);

    // The underground places, with their floors.
    let groups_en = get(client, "v2/map/point_group", id, "en-us").await?;
    let groups_ru = get(client, "v2/map/point_group", id, "ru-ru").await?;
    let mut floor_names_ru: HashMap<u64, String> = HashMap::new();
    for group in groups_ru["data"]["list"].as_array().cloned().unwrap_or_default() {
        for floor in group["floors"].as_array().cloned().unwrap_or_default() {
            floor_names_ru.insert(
                number(&floor["id"]) as u64,
                floor["floor_name"].as_str().unwrap_or("").to_owned(),
            );
        }
    }
    let mut sites = Vec::new();
    let mut floor_of: HashMap<u64, u64> = HashMap::new();
    for group in groups_en["data"]["list"].as_array().cloned().unwrap_or_default() {
        let mut floors = Vec::new();
        for floor in group["floors"].as_array().cloned().unwrap_or_default() {
            let floor_id = number(&floor["id"]) as u64;
            if floor_id == 0 {
                continue;
            }
            for point in floor["point_ids"].as_array().cloned().unwrap_or_default() {
                floor_of.insert(number(&point) as u64, floor_id);
            }
            floors.push(json!({
                "id": floor_id,
                "en": floor["floor_name"],
                "ru": floor_names_ru.get(&floor_id).cloned().unwrap_or_default(),
                "short": floor["floor_name_short"],
                "url": floor["overlay"]["url"],
                "rect": rect_of(&floor["overlay"], ["l_x", "l_y", "r_x", "r_y"]),
            }));
        }
        if floors.is_empty() {
            continue;
        }
        let first = group["default_floor_index"].as_u64().unwrap_or(0).min(floors.len() as u64 - 1);
        sites.push(json!({
            "id": number(&group["id"]) as u64,
            "default": first,
            "entrance": [
                rounded(number(&group["underground_entrance_x_pos"])),
                rounded(number(&group["underground_entrance_y_pos"])),
            ],
            "floors": floors,
        }));
    }

    // The markers: on the surface, or on a floor.
    let points = get(client, "v3/map/point/list", id, "en-us").await?;
    for point in points["data"]["point_list"].as_array().context("no points")? {
        let (Some(point_id), Some(label)) = (point["id"].as_u64(), point["label_id"].as_u64()) else {
            continue;
        };
        let floor = floor_of.get(&point_id).copied().unwrap_or(0);
        // Other layers that are on no floor have nowhere to be shown.
        if floor == 0 && point["z_level"].as_i64().unwrap_or(0) != 0 {
            continue;
        }
        let (x, y) = (number(&point["x_pos"]), number(&point["y_pos"]));
        rows.push(json!([point_id, label, rounded(x), rounded(y), id, floor]));
    }

    Ok(json!({
        "id": id,
        "en": info["data"]["info"]["name"],
        "ru": info_ru["data"]["info"]["name"],
        "version": version,
        "origin": origin,
        "size": size,
        "max_level": detail["min_zoom"].as_i64().map_or(4, |zoom| zoom.unsigned_abs()),
        "anchors": regions,
        "sites": sites,
    }))
}

/// Everything, as it is kept.  The world is needed; the other maps are left out
/// if they can't be had.
pub async fn fetch_all(client: &reqwest::Client) -> Result<Fetched> {
    let labels = labels(client).await?;
    let mut rows: Vec<Value> = Vec::new();
    let mut maps = Vec::new();
    for id in MAPS {
        match map(client, id, &mut rows).await {
            Ok(map) => maps.push(map),
            Err(e) if id != 2 => eprintln!("the map {id} is left out: {e:#}"),
            Err(e) => return Err(e),
        }
    }
    if rows.len() < 10_000 {
        bail!("the list of markers is too short ({})", rows.len());
    }
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::best());
    encoder.write_all(serde_json::to_string(&rows)?.as_bytes())?;
    Ok(Fetched {
        kinds: labels.len(),
        markers: rows.len(),
        labels: serde_json::to_string(&labels)?,
        atlas: serde_json::to_string(&json!({ "maps": maps }))?,
        points_gz: encoder.finish()?,
    })
}
