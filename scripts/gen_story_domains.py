"""Adds the one-time and trounce (boss) domains to src/data/material_sources.json.

  story: "<marker of the map>" -> {kind, en, ru, region, rewards:[{id,qty}], drops:[id], ...}

The domains and their rewards are from the Genshin Impact wiki (the infobox, "First Time
Completion Rewards" and the rewards of the weekly boss), the items (ids, pictures, Russian names)
from Project Amber, and the markers of the map are found by the name of the domain in their text
(HoYoLAB), in English and in Russian.

Run from the root of the project, after gen_material_sources.py:  python scripts/gen_story_domains.py
"""
import gzip
import json
import re
import sys
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor

WIKI = "https://genshin-impact.fandom.com/api.php?format=json&"
AMBER = "https://gi.yatta.moe/api/v2/"
MAP = "https://sg-public-api-static.hoyolab.com/common/map_user/ys_obc/v1/map/point/info"
DOMAIN_LABEL = 154

# Markers whose text does not name the domain: marker -> the domain.
MANUAL = {
    "92860": "False Moon Institute",
    "46534": "The Realm of Beginnings",
    "28756": "The Coordinates of Sun and Rain",
    "36703": "Red Desert Threshold",
    "28754": "Fragment of Childhood Dreams",
    "12778": "Empty Boat of a Thousand Gates",
    "68347": "Scattered Ruins",
    "98499": "Binding Field of Universal Nirvana",
}


def get(url):
    for _ in range(4):
        try:
            request = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 irminsul-data"})
            return json.load(urllib.request.urlopen(request, timeout=40))
        except Exception:
            pass
    return None


def norm(text):
    return re.sub(r"[^a-z0-9а-яё]", "", text.lower())


def members(category):
    reply = get(WIKI + "action=query&list=categorymembers&cmlimit=500&cmtitle=Category:" + category)
    return [m["title"] for m in reply["query"]["categorymembers"] if "Domain" not in m["title"] or m["title"].startswith("Domain of")]


def page(title):
    reply = get(WIKI + "action=parse&prop=wikitext&redirects=1&page=" + urllib.parse.quote(title))
    return reply["parse"]["wikitext"]["*"] if reply and "parse" in reply else ""


def field(text, name):
    found = re.search(r"^\|" + name + r"\s*=\s*(.*)$", text, re.M)
    return found.group(1).strip() if found else ""


def main():
    once = members("One-Time_Domains")
    trounce = members("Trounce_Domains")
    titles = [(t, "once") for t in once] + [(t, "trounce") for t in trounce]
    with ThreadPoolExecutor(8) as pool:
        texts = list(pool.map(lambda item: page(item[0]), titles))

    # The items of Amber: name -> (icon, Russian name, rarity): materials, food, artifact pieces.
    by_name = {}
    for kind_ in ("material", "food"):
        en = get(AMBER + "en/" + kind_)["data"]["items"]
        ru = get(AMBER + "ru/" + kind_)["data"]["items"]
        for key, item in en.items():
            by_name.setdefault(norm(item["name"]), (item["icon"], ru.get(key, {}).get("name", item["name"]), item.get("rank", 1), int(key) if kind_ == "material" else 0))
            if kind_ == "food":
                by_name.setdefault(norm("Recipe: " + item["name"]), (item["icon"], "Рецепт: " + ru.get(key, {}).get("name", item["name"]), item.get("rank", 1), 0))
    sets_en = get(AMBER + "en/reliquary")["data"]["items"]
    sets_ru = get(AMBER + "ru/reliquary")["data"]["items"]

    def suit(key):
        return key, get(AMBER + "en/reliquary/" + key)["data"].get("suit") or {}, get(AMBER + "ru/reliquary/" + key)["data"].get("suit") or {}

    with ThreadPoolExecutor(8) as pool:
        for key, suit_en, suit_ru in pool.map(suit, list(sets_en)):
            for slot, piece in suit_en.items():
                by_name.setdefault(norm(piece["name"]), (piece["icon"], suit_ru.get(slot, {}).get("name", piece["name"]), 5, 0))
    print("items:", len(by_name), file=sys.stderr)

    def resolve(name, qty=None):
        found = by_name.get(norm(name))
        if not found:
            missing_items.add(name)
            return None
        out = {"en": name, "ru": found[1], "icon": found[0], "rank": found[2], "id": found[3]}
        if qty is not None:
            out["qty"] = qty
        return out

    domains = []
    missing_items = set()
    for (title, kind), text in zip(titles, texts):
        if not text:
            continue
        name_ru = field(text.split("{{Other Languages", 1)[-1], "ru") or field(text.split("{{Other Languages", 1)[-1], "1_ru")
        entry = {
            "kind": kind,
            "en": title,
            "ru": name_ru or title,
            "region": field(text, "region"),
            "rewards": [],
            "drops": [],
        }
        rewards = text.split("==First Time Completion Rewards==", 1)
        if len(rewards) == 2:
            block = rewards[1].split("\n==", 1)[0]
            for item, qty in re.findall(r"\{\{Card\|([^|}]+)\|([^|}]+)", block):
                qty = int(re.sub(r"[^0-9]", "", qty) or 1)
                reward = resolve(item.strip(), qty)
                if reward:
                    entry["rewards"].append(reward)
        if kind == "trounce":
            drops = field(text, "drops") or (re.search(r"\|exclusive=([^\n|]*)", text) or [0, ""])[1]
            for name in [d.strip() for d in drops.split(",") if d.strip()]:
                drop = resolve(name)
                if drop:
                    entry["drops"].append(drop)
            boss = re.search(r"\{\{Card\|([^|}]+)\|type=Enemy", text)
            entry["boss"] = boss.group(1) if boss else ""
        domains.append(entry)
    print("missing items:", sorted(missing_items), file=sys.stderr)

    # The markers of the domains and their texts, in both languages.
    points = [p for p in json.load(gzip.open("src/data/map_points.json.gz")) if p[1] == DOMAIN_LABEL]

    def text_of(point):
        out = []
        for lang in ("en-us", "ru-ru"):
            reply = get(f"{MAP}?point_id={point[0]}&map_id=2&app_sn=ys_obc&lang={lang}")
            out.append((reply or {}).get("data", {}).get("info", {}).get("content", ""))
        return point[0], " ".join(out)

    with ThreadPoolExecutor(8) as pool:
        marker_texts = dict(pool.map(text_of, points))

    path = "src/data/material_sources.json"
    data = json.load(open(path, encoding="utf-8"))
    taken = set(data["points"].values()) | {int(k) for k in data["artifacts"]}
    story = {}
    used = set()
    for id_, text in marker_texts.items():
        if id_ in taken:
            continue
        flat = norm(text)
        chosen = None
        if str(id_) in MANUAL:
            chosen = next((d for d in domains if d["en"] == MANUAL[str(id_)]), None)
        if chosen is None:
            for domain in domains:
                keys = [norm(domain["en"]), norm(domain["ru"])]
                if any(len(key) > 6 and key in flat for key in keys):
                    chosen = domain
                    break
        if chosen:
            story[str(id_)] = chosen
            used.add(chosen["en"])
    data["story"] = story
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, separators=(",", ":"))
    print(len(story), "markers of story domains", file=sys.stderr)
    print("domains with no marker:", [d["en"] for d in domains if d["en"] not in used], file=sys.stderr)
    print("markers with no domain:", {i: t[:60] for i, t in marker_texts.items() if i not in taken and str(i) not in story}, file=sys.stderr)


main()
