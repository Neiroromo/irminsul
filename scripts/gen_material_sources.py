"""Builds src/data/material_sources.json: where materials are got, from Project Amber.

  mobs:    item id -> kinds of marker (labels of the map) of the enemies that drop it
  domains: item id -> [{city, kind, days (0 = Monday), en, ru}] of the daily domains
  points:  "<city><M|F>" -> the marker (point) of the map of that talent (M) / weapon (F) domain

Run from the root of the project:  python scripts/gen_material_sources.py
"""
import json
import re
import sys
import urllib.request
from concurrent.futures import ThreadPoolExecutor

AMBER = "https://gi.yatta.moe/api/v2/"
NOT_MOBS = {202, 104001, 104002, 104003, 102, 105, 100, 101}  # mora, experience, and the like

# The markers of the map of the daily domains: city (Amber) and M(astery: talents) / F(orgery: weapons).
POINTS = {
    "1M": 7149, "1F": 7092,
    "2M": 7116, "2F": 7173,
    "3M": 12784, "3F": 12781,
    "4M": 28753, "4F": 28757,
    "5M": 50067, "5F": 50057,
    "6M": 73159, "6F": 73161,
    "7M": 88122, "7F": 88121,
    "8M": 100705, "8F": 100707,
}


def get(url):
    for _ in range(4):
        try:
            request = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 irminsul-data"})
            return json.load(urllib.request.urlopen(request, timeout=30))["data"]
        except Exception:
            pass
    return None


def norm(name):
    return re.sub(r"[^a-z0-9 ]", "", name.lower()).strip()


def main():
    labels = json.load(open("src/data/map_labels.json", encoding="utf-8"))
    enemy = {norm(x["en"]): x["id"] for x in labels if x["parent"] == 50}
    aliases = {"lawachurl": 53, "stonehide lawachurl": 53, "chieftain": 53}

    parts = [
        ("lawachurl", 53), ("fatui pyro agent", 25), ("cicin mage", 26), ("volkodlak", 843),
        ("rifthound", 265), ("wayob manifestation", 604), ("avatar of lava", 607),
        ("radiant antelope", 736), ("radiant bladehorn", 736), ("radiant lunapod", 736),
        ("wavebinder", 878), ("consecrated", 454), ("black serpent knight", 344), ("phantasm", 495),
    ]

    def label_of(name):
        n = norm(name)
        for part, label in parts:
            if part in n:
                return label
        if n in aliases:
            return aliases[n]
        if n in enemy:
            return enemy[n]
        best = None
        for key, id_ in enemy.items():
            if re.search(r"(^| )" + re.escape(key) + r"( |$)", n) or re.search(r"(^| )" + re.escape(n) + r"( |$)", key):
                if best is None or len(key) > len(best[0]):
                    best = (key, id_)
        return best[1] if best else None

    monsters = get(AMBER + "en/monster")["items"]

    def one(id_):
        return id_, get(AMBER + f"en/monster/{id_}")

    mobs = {}
    unmatched = {}
    with ThreadPoolExecutor(8) as pool:
        for id_, data in pool.map(one, list(monsters)):
            if not data:
                continue
            label = label_of(data["name"])
            items = set()
            for entry in data.get("entries", {}).values():
                for item in (entry.get("reward") or {}):
                    items.add(int(item))
            items -= NOT_MOBS
            if not items:
                continue
            if label is None:
                unmatched[data["name"]] = len(items)
                continue
            for item in items:
                mobs.setdefault(item, set()).add(label)

    # Items no enemy of the list drops (the newest enemies are not in it): the text of where the
    # item is got ("Dropped by Aberrant Chimeric Monsters") names the kind of enemy.
    import gzip
    materials = json.load(gzip.open("src/data/upgrade_data.json.gz"))["materials"]

    def from_text(id_):
        data = get(AMBER + f"en/material/{id_}")
        found = set()
        for source in (data or {}).get("source", []):
            name = source["name"]
            if not name.startswith("Dropped"):
                continue
            name = re.sub(r"^Dropped (uncommonly |rarely |occasionally )?by ", "", name)
            name = re.sub(r"Lv\. \d+\+ ", "", name)
            for part in re.split(r",| and | or ", name):
                part = part.strip()
                for form in (part, part[:-1], part[:-2], part[:-3] + "y"):
                    label = label_of(form) if form else None
                    if label:
                        found.add(label)
                        break
        return id_, found

    missing = [int(id_) for id_ in materials if int(id_) not in mobs]
    with ThreadPoolExecutor(8) as pool:
        for id_, found in pool.map(from_text, missing):
            if found:
                mobs[id_] = found

    en = get(AMBER + "en/dailyDungeon")
    ru = get(AMBER + "ru/dailyDungeon")
    days = ["monday", "tuesday", "wednesday", "thursday", "friday", "saturday", "sunday"]
    domains = {}
    merged = {}
    for number, day in enumerate(days):
        for key, dom in en[day].items():
            kind = "M" if "Mastery" in dom["name"] else "F"
            entry = merged.setdefault(
                dom["id"],
                {"city": dom["city"], "kind": kind, "days": [], "en": dom["name"],
                 "ru": ru[day].get(key, {}).get("name", dom["name"]), "reward": dom["reward"]},
            )
            entry["days"].append(number)
    for entry in merged.values():
        for item in entry["reward"]:
            if item in (102, 202, 105):
                continue
            domains.setdefault(item, []).append(
                {k: entry[k] for k in ("city", "kind", "days", "en", "ru")}
            )

    out = {
        "mobs": {str(k): sorted(v) for k, v in sorted(mobs.items())},
        "domains": {str(k): v for k, v in sorted(domains.items())},
        "points": POINTS,
    }
    with open("src/data/material_sources.json", "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, separators=(",", ":"))
    print(len(mobs), "items from enemies,", len(domains), "from domains", file=sys.stderr)
    print("enemies without a marker kind:", sorted(unmatched.items(), key=lambda x: -x[1])[:60], file=sys.stderr)


main()
