"""Builds src/data/monster_labels.json: which kind of marker of the map a monster of the game is.

  families: "<monster id without its last two digits>" (the whole id for the animals, 28......)
            -> the kind of marker (label of the map)
  names:    "<the same>" -> [English name, Russian name], for telling what was seen

The game sends the monsters around the player by their ids (26310201 is a Chimeric Burrowbeast);
the ids of the variants of one monster differ in the last two digits (25131601, 25131602 ...),
so the table is by what is before them.  The names are from Project Amber, and are matched to
the kinds of marker as in gen_material_sources.py (keep the two alike).

Run from the root of the project:  python scripts/gen_monster_labels.py
"""
import json
import re
import sys
import urllib.request

AMBER = "https://gi.yatta.moe/api/v2/"
ENEMIES = 50


def get(url):
    for _ in range(4):
        try:
            request = urllib.request.Request(url, headers={"User-Agent": "Mozilla/5.0 irminsul-data"})
            return json.load(urllib.request.urlopen(request, timeout=40))["data"]
        except Exception:
            pass
    return None


def norm(name):
    return re.sub(r"[^a-z0-9 ]", "", name.lower()).strip()


def main():
    labels = json.load(open("src/data/map_labels.json", encoding="utf-8"))
    enemy = {norm(x["en"]): x["id"] for x in labels if x["parent"] == ENEMIES and x["en"]}
    # The kinds that are not enemies (animals, birds, fish ...) are taken only by their very name.
    other = {norm(x["en"]): x["id"] for x in labels if x["parent"] not in (0, ENEMIES) and x["en"]}
    aliases = {"lawachurl": 53, "stonehide lawachurl": 53, "chieftain": 53}
    parts = [
        ("lawachurl", 53), ("fatui pyro agent", 25), ("cicin mage", 26), ("volkodlak", 843),
        ("rifthound", 265), ("wayob manifestation", 604), ("avatar of lava", 607),
        ("radiant antelope", 736), ("radiant bladehorn", 736), ("radiant lunapod", 736),
        ("wavebinder", 878), ("consecrated", 454), ("black serpent knight", 344), ("phantasm", 495),
    ]

    # Monsters whose kind of marker has another name than theirs, by a part of the name; looked
    # at after the very names (the Chimeric Winged Lion is a kind of its own, not a chimera).
    families_by_part = [
        ("treasure hoarders", 58), ("kairagi", 209), ("eremite", 389), ("fungus", 221), ("shroom", 221),
        ("oprichniki", 739), (" mek", 494), ("operative", 541), ("shadowy husk", 344), ("abyss lector", 337),
        ("ruin cruiser", 215), ("ruin destroyer", 215), ("ruin defender", 215), ("ruin scout", 215),
        ("leshy", 846), ("tree segment", 846), ("wilderness exile", 740), ("wilderness hunter", 740),
        ("glacial warden", 844), ("radiant glacial wolf", 736), ("chimeric", 845),
        # The warriors of the tribes before the saurians they are named after.
        ("warrior", 606), ("flowing blade", 606), ("forged sand", 606), ("swiftstep", 606),
        ("tepetlisaur", 603), ("koholasaur", 603), ("yumkasaur", 603), ("qucusaur", 603), ("iktomisaur", 603), ("tatankasaur", 603),
        ("hunters ray", 493), ("armored crab", 493), ("blubberbeast", 493), ("hat jellyfish", 493), ("seahorse", 493),
        ("ball octopus", 493), ("sea hare", 493), ("sternshield crab", 493),
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
        if best:
            return best[1]
        for part, label in families_by_part:
            if part in n:
                return label
        return other.get(n)

    en = get(AMBER + "en/monster")["items"]
    ru = get(AMBER + "ru/monster")["items"]
    families = {}
    names = {}
    unmatched = []
    for key, monster in sorted(en.items()):
        # The animals (28......) are told one from another by the whole id: those that differ in
        # the last digits are other animals, not variants.
        family = key if key.startswith("28") else str(int(key) // 100)
        names.setdefault(family, [monster["name"], ru.get(key, {}).get("name", monster["name"])])
        label = label_of(monster["name"])
        if label is None:
            if monster.get("type") not in ("ANIMAL", "AVIARY", "FISH", "CRITTER"):
                unmatched.append(monster["name"])
            continue
        families.setdefault(family, label)
    out = {"families": families, "names": names}
    with open("src/data/monster_labels.json", "w", encoding="utf-8") as f:
        json.dump(out, f, ensure_ascii=False, separators=(",", ":"))
    print(len(families), "of", len(names), "monsters have a kind of marker", file=sys.stderr)
    print("enemies without one:", unmatched, file=sys.stderr)


main()
