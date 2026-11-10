#!/usr/bin/env python3
"""Offres et demandeurs d'emploi par département, pour la carte de France (API « Marché du travail » v1).

Pour les N métiers ROME qui ont le plus d'offres (data/rome.js, lancer fetch_rome.py avant), plus les métiers tech de fetch_tech.py, et pour chacun des
96 départements métropolitains : offres enregistrées sur les 4 derniers trimestres (cumul) et demandeurs
d'emploi de catégorie A au dernier trimestre. Environ 2 x N x 96 appels (N = 100 : près d'une heure).
Reprise : les réponses déjà obtenues sont gardées dans data/carte_cache.json (ignoré par git).
Prérequis : .env (voir fetch_rome.py).
Sortie : data/carte.js -> window.CARTE = {updated, quarter, deps:[codes], items:{CODE:{o:[...], d:[...]}}}
Usage : python fetch_carte.py
"""
import json, os
from concurrent.futures import ThreadPoolExecutor
from datetime import date
import fetch_rome as fr  # authentification et appels (avec reprise sur erreur)
import fetch_tech as ft  # liste des métiers du numérique (TECH_CODES)

HERE = os.path.dirname(os.path.abspath(__file__))
N = 100          # nombre de métiers (les plus volumineux)
EXTRA_CODES = list(ft.TECH_CODES)  # métiers ajoutés même s'ils ne sont pas parmi les N plus volumineux
WORKERS = 6      # appels en parallèle
CACHE = os.path.join(HERE, "data", "carte_cache.json")
# départements métropolitains : 01 à 19, 2A, 2B, 21 à 95
DEPS = [f"{i:02d}" for i in range(1, 20)] + ["2A", "2B"] + [f"{i:02d}" for i in range(21, 96)]
BASE = {"codeTypeTerritoire": "DEP", "codeTypeActivite": "ROME"}


def top_codes(rome, n=N):
    """Les n codes ROME au plus gros cumul d'offres sur les 4 derniers trimestres (égalité : ordre du code)."""
    vol = {c: sum(x or 0 for x in it["o"][-4:]) for c, it in rome["items"].items()}
    return sorted(vol, key=lambda c: (-vol[c], c))[:n]


def selection(rome):
    """Les n plus volumineux, puis les métiers du bloc tech (EXTRA_CODES) qui n'y sont pas, dans l'ordre de la liste."""
    codes = top_codes(rome)
    return codes + [c for c in EXTRA_CODES if c in rome["items"] and c not in codes]


def offres_4t(code, dep):
    """(cumul des 4 derniers trimestres, dernier trimestre) ou (None, None) s'il manque des trimestres."""
    r = fr.call("/v1/indicateur/stat-offres", dict(BASE, codeTerritoire=dep, codeActivite=code,
                                                   codeTypePeriode="TRIMESTRE", codeTypeNomenclature="ORIGINEOFF"))
    rows = {x["codePeriode"]: x["valeurPrincipaleNombre"] for x in (r or {}).get("listeValeursParPeriode", [])
            if x.get("codeNomenclature") == "TOFF" and x.get("valeurPrincipaleNombre") is not None}
    ps = sorted(rows)
    if len(ps) < 4:
        return None, None
    return sum(rows[p] for p in ps[-4:]), ps[-1]


def demandeurs(code, dep):
    """(demandeurs cat. A au dernier trimestre, période) ou (None, None)."""
    r = fr.call("/v1/indicateur/stat-demandeurs", dict(BASE, codeTerritoire=dep, codeActivite=code,
                                                       codeTypePeriode="TRIMESTRE", codeTypeNomenclature="CATCAND"))
    rows = {x["codePeriode"]: x["valeurPrincipaleNombre"] for x in (r or {}).get("listeValeursParPeriode", [])
            if x.get("codeNomenclature") == "A" and x.get("valeurPrincipaleNombre") is not None}
    if not rows:
        return None, None
    p = max(rows)
    return rows[p], p


def cell(args):
    code, dep = args
    o, po = offres_4t(code, dep)
    d, pd = demandeurs(code, dep)
    return code, dep, o, d, po, pd


def main():
    txt = open(os.path.join(HERE, "data", "rome.js"), encoding="utf-8").read().strip()
    rome = json.loads(txt[len("window.ROME="):-1])
    codes = selection(rome)
    cache =json.load(open(CACHE, encoding="utf-8")) if os.path.exists(CACHE) else {}
    todo = [(c, d) for c in codes for d in DEPS if f"{c}|{d}" not in cache]
    print(len(codes), "métiers,", len(DEPS), "départements,", len(todo), "cellules à collecter", flush=True)
    quarters = set()
    with ThreadPoolExecutor(WORKERS) as ex:
        for i, (code, dep, o, d, po, pd) in enumerate(ex.map(cell, todo)):
            cache[f"{code}|{dep}"] = [o, d, po, pd]
            if i % 200 == 0:
                print(i, "/", len(todo), flush=True)
                json.dump(cache, open(CACHE, "w", encoding="utf-8"), separators=(",", ":"))
    json.dump(cache, open(CACHE, "w", encoding="utf-8"), separators=(",", ":"))
    items = {}
    for c in codes:
        row = [cache[f"{c}|{d}"] for d in DEPS]
        items[c] = {"o": [r[0] for r in row], "d": [r[1] for r in row]}
        quarters.update(r[3] for r in row if r[3])
    out = {"updated": date.today().isoformat(), "quarter": max(quarters) if quarters else None,
           "deps": DEPS, "items": items}
    path = os.path.join(HERE, "data", "carte.js")
    with open(path, "w", encoding="utf-8") as f:
        f.write("window.CARTE=" + json.dumps(out, ensure_ascii=False, separators=(",", ":")) + ";")
    print("écrit data/carte.js :", len(items), "métiers,", len(DEPS), "départements,", os.path.getsize(path) // 1024, "Ko")


if __name__ == "__main__":
    main()
