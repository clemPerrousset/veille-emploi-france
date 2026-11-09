#!/usr/bin/env python3
"""Indicateurs France Travail pour les métiers du numérique (API « Marché du travail » v1) :
demandeurs d'emploi inscrits (cat. A, fin de trimestre), embauches (trimestre) et niveau de tension (annuel).

Les offres par métier viennent de fetch_rome.py. Prérequis : .env (voir fetch_rome.py).
Sortie : data/tech.js -> window.TECH = {updated, periods:[...], items:{CODE:{l, d:[...], e:[...], t:{AAAA:niveau 1-5}}}}
Usage : python fetch_tech.py
"""
import json, os
from datetime import date
import fetch_rome as fr  # réutilise l'authentification et les appels (avec reprise sur erreur)

HERE = os.path.dirname(os.path.abspath(__file__))
TECH_CODES = ["M1805", "M1855", "M1861", "M1853", "M1827", "M1811", "M1419", "M1405", "M1856", "M1884",
              "M1802", "M1801", "M1810", "M1806", "M1867", "M1803", "I1404", "I1401"]
BASE = {"codeTypeTerritoire": "NAT", "codeTerritoire": "FR", "codeTypeActivite": "ROME"}


def series(path, code, period, nomenclature, wanted):
    r = fr.call(path, dict(BASE, codeActivite=code, codeTypePeriode=period, codeTypeNomenclature=nomenclature))
    out, label = {}, None
    for x in (r or {}).get("listeValeursParPeriode", []):
        if x.get("codeNomenclature") == wanted and x.get("valeurPrincipaleNombre") is not None:
            out[x["codePeriode"]] = x["valeurPrincipaleNombre"]
            label = x.get("libActivite")
    return out, label


def main():
    raw, labels = {}, {}
    for code in TECH_CODES:
        d, l1 = series("/v1/indicateur/stat-demandeurs", code, "TRIMESTRE", "CATCAND", "A")
        e, l2 = series("/v1/indicateur/stat-embauches", code, "TRIMESTRE", "CATCANDxDUREEEMP", "TOUT-TOUTE")
        t, l3 = series("/v1/indicateur/stat-perspective-employeur", code, "ANNEE", "TYPE_TENSION", "PERSPECTIVE")
        raw[code] = (d, e, t)
        labels[code] = l1 or l2 or l3 or code
        print(code, labels[code][:40], "| demandeurs", len(d), "| embauches", len(e), "| tension", len(t), flush=True)
    periods = sorted({p for d, e, _ in raw.values() for p in list(d) + list(e) if p >= fr.MIN_PERIOD})
    items = {}
    for code, (d, e, t) in raw.items():
        items[code] = {"l": labels[code], "d": [d.get(p) for p in periods], "e": [e.get(p) for p in periods], "t": t}
    out = {"updated": date.today().isoformat(), "periods": periods, "items": items}
    path = os.path.join(HERE, "data", "tech.js")
    with open(path, "w", encoding="utf-8") as f:
        f.write("window.TECH=" + json.dumps(out, ensure_ascii=False, separators=(",", ":")) + ";")
    print("écrit data/tech.js :", len(items), "métiers,", len(periods), "trimestres,", os.path.getsize(path) // 1024, "Ko")


if __name__ == "__main__":
    main()
