#!/usr/bin/env python3
"""Tension de recrutement et demandeurs d'emploi des métiers ROME les plus volumineux (API « Marché du travail » v1).

Alimente la vue « Où ça recrute » : métiers à la fois nombreux en offres et difficiles à pourvoir, et métiers
où il y a beaucoup plus d'offres que de demandeurs. Sélection : les N métiers qui ont le plus d'offres sur les
4 derniers trimestres de data/rome.js (lancer fetch_rome.py avant). Prérequis : .env (voir fetch_rome.py).
Sortie : data/tension.js -> window.TENSION = {updated, n, items:{CODE:{t:{AAAA:niveau 1-5}, d:{AAAATn:demandeurs cat. A}}}}
Usage : python fetch_tension.py
"""
import json, os
from datetime import date
import fetch_rome as fr  # authentification et appels (avec reprise sur erreur)
import fetch_tech as ft  # même lecture d'une série : ft.series(...)

HERE = os.path.dirname(os.path.abspath(__file__))
N = 300  # nombre de métiers retenus (les plus volumineux)


def top_codes(rome, n=N):
    """Les n codes ROME au plus gros cumul d'offres sur les 4 derniers trimestres (égalité : ordre du code)."""
    vol = {c: sum(x or 0 for x in it["o"][-4:]) for c, it in rome["items"].items()}
    return sorted(vol, key=lambda c: (-vol[c], c))[:n]


def main():
    path_rome = os.path.join(HERE, "data", "rome.js")
    txt = open(path_rome, encoding="utf-8").read().strip()
    rome = json.loads(txt[len("window.ROME="):-1])
    codes = top_codes(rome)
    items = {}
    for i, c in enumerate(codes):
        d, _ = ft.series("/v1/indicateur/stat-demandeurs", c, "TRIMESTRE", "CATCAND", "A")
        t, _ = ft.series("/v1/indicateur/stat-perspective-employeur", c, "ANNEE", "TYPE_TENSION", "PERSPECTIVE")
        items[c] = {"t": t, "d": {p: v for p, v in d.items() if p >= fr.MIN_PERIOD}}
        if i % 25 == 0:
            print(i, "/", len(codes), c, flush=True)
    out = {"updated": date.today().isoformat(), "n": len(items), "items": items}
    path = os.path.join(HERE, "data", "tension.js")
    with open(path, "w", encoding="utf-8") as f:
        f.write("window.TENSION=" + json.dumps(out, ensure_ascii=False, separators=(",", ":")) + ";")
    print("écrit data/tension.js :", len(items), "métiers,", os.path.getsize(path) // 1024, "Ko")


if __name__ == "__main__":
    main()
