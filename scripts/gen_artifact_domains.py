"""Adds the artifact domains to src/data/material_sources.json.

  artifacts: "<marker of the map>" -> [{id, en, ru, icon}] the artifact sets the domain gives

The domains and what they give are from genshin-db (the name of the entrance of each domain and
the artifact sets of its rewards), the artifact sets and their pictures from Project Amber, and the
markers of the map are found by the name of the entrance in their text (HoYoLAB).

Run from the root of the project, after gen_material_sources.py:  python scripts/gen_artifact_domains.py
"""
import gzip
import json
import re
import sys
import urllib.parse
import urllib.request
from concurrent.futures import ThreadPoolExecutor

DB = "https://genshin-db-api.vercel.app/api/v5/domains"
AMBER = "https://gi.yatta.moe/api/v2/"
MAP = "https://sg-public-api-static.hoyolab.com/common/map_user/ys_obc/v1/map/point/info"
DOMAIN_LABEL = 154

# The names of the entrances that the text of a marker does not have the way genshin-db has them.
ALIASES = {
    "Domain of Guyun": ["Guyun"],
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
    return re.sub(r"[^a-z0-9]", "", text.lower())


def main():
    names = get(f"{DB}?query=names&matchCategories=true&resultLanguage=English")
    names = [name for name in names if name.startswith("Domain of Blessing")]

    def one(name):
        return get(f"{DB}?query={urllib.parse.quote(name)}&matchCategories=false&resultLanguage=English")

    entrances = {}
    with ThreadPoolExecutor(8) as pool:
        for domain in pool.map(one, names):
            if not domain or domain.get("domainText") != "Artifacts":
                continue
            sets = {}
            for reward in domain.get("rewardPreview", []):
                if reward["id"] >= 400000:
                    sets[reward["name"]] = max(sets.get(reward["name"], 0), reward.get("rarity", 0))
            entrance = entrances.setdefault(domain["entranceName"], {})
            for name_, rarity in sets.items():
                entrance[name_] = max(entrance.get(name_, 0), rarity)

    # The artifact sets: id, picture, Russian name.
    en = get(AMBER + "en/reliquary")["data"]["items"]
    ru = get(AMBER + "ru/reliquary")["data"]["items"]
    by_name = {norm(item["name"]): (int(key), item["icon"], ru.get(key, {}).get("name", item["name"])) for key, item in en.items()}

    # The markers of the domains and their texts.
    points = [p for p in json.load(gzip.open("src/data/map_points.json.gz")) if p[1] == DOMAIN_LABEL]

    def text_of(point):
        reply = get(f"{MAP}?point_id={point[0]}&map_id=2&app_sn=ys_obc&lang=en-us")
        return point[0], (reply or {}).get("data", {}).get("info", {}).get("content", "")

    texts = {}
    with ThreadPoolExecutor(8) as pool:
        for id_, text in pool.map(text_of, points):
            texts[id_] = text

    result = {}
    unmatched = []
    for entrance, sets in entrances.items():
        keys = [norm(entrance)] + [norm(alias) for alias in ALIASES.get(entrance, [])]
        found = [id_ for id_, text in texts.items() if any(key and key in norm(text) for key in keys)]
        if not found:
            unmatched.append(entrance)
            continue
        five = [name for name, rarity in sets.items() if rarity >= 5] or list(sets)
        listed = []
        for name in sorted(five):
            if norm(name) in by_name:
                set_id, icon, name_ru = by_name[norm(name)]
                listed.append({"id": set_id, "en": name, "ru": name_ru, "icon": icon})
        for id_ in found:
            result[str(id_)] = listed

    # The markers whose text names the sets (the entrance is not in it): found by the sets.
    for entrance, sets in entrances.items():
        five = [name for name, rarity in sets.items() if rarity >= 5] or list(sets)
        listed = []
        for name in sorted(five):
            if norm(name) in by_name:
                set_id, icon, name_ru = by_name[norm(name)]
                listed.append({"id": set_id, "en": name, "ru": name_ru, "icon": icon})
        for id_, text in texts.items():
            if str(id_) in result or "rtifact" not in text:
                continue
            if sum(norm(item["en"]) in norm(text) for item in listed) >= 2:
                result[str(id_)] = listed
                if entrance in unmatched:
                    unmatched.remove(entrance)

    path = "src/data/material_sources.json"
    data = json.load(open(path, encoding="utf-8"))
    data["artifacts"] = result
    with open(path, "w", encoding="utf-8") as f:
        json.dump(data, f, ensure_ascii=False, separators=(",", ":"))
    print(len(result), "markers of artifact domains", file=sys.stderr)
    print("entrances with no marker:", unmatched, file=sys.stderr)


main()
