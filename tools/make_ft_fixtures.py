#!/usr/bin/env python3
"""Enregistre des réponses RÉELLES de l'API France Travail « Marché du travail » en fixtures réduites,
pour les tests des collecteurs `rome` et `tech` (server/tests/fixtures/ft/).

Ce script lit le fichier .env (via fetch_rome.py) pour s'authentifier ; AUCUN secret n'est écrit dans les
fixtures (il vérifie à la fin qu'elles ne contiennent ni le secret, ni l'identifiant, ni le jeton).
Les réponses sont réduites : la clé `listeValeurParCaract` (détail par caractéristique, jamais utilisée par
les collecteurs) est supprimée de chaque ligne, et seules les nomenclatures de KEEP sont gardées (la valeur
utilisée + des séries voisines qui servent de pièges) ; les lignes gardées sont inchangées.

Usage : python tools/make_ft_fixtures.py        (réseau + .env requis, une seule fois)
"""
import json
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, ROOT)
import fetch_rome as fr  # noqa: E402  (charge .env ; ne rien afficher de ce module)

OUT = os.path.join(ROOT, "server", "tests", "fixtures", "ft")
ROME_CODES = ["M1805", "A1401", "K1303"]   # un métier tech, un très saisonnier, un gros volume
TECH_CODES = ["M1805", "M1856"]
BASE = {"codeTypeTerritoire": "NAT", "codeTerritoire": "FR", "codeTypeActivite": "ROME"}
DROP = "listeValeurParCaract"


# Nomenclatures conservées par indicateur : la valeur utilisée par les collecteurs + des séries VOISINES
# qui servent de pièges (le collecteur doit les ignorer). Les autres lignes sont supprimées pour garder
# des fixtures petites ; les lignes gardées sont inchangées.
KEEP = {
    "offres": {"TOFF", "PE", "TOFF-CUMUL12MOIS"},
    "demandeurs": {"A", "B", "ABC"},
    "embauches": {"TOUT-TOUTE", "ABCDEFG-TOUTE", "TOUT-SUP1M"},
    "tension": {"PERSPECTIVE", "MAIN_OEUVRE", "INT_EMB"},
}


def reduce_resp(resp, kind):
    if not resp:
        return resp
    rows = []
    for row in resp.get("listeValeursParPeriode", []):
        if row.get("codeNomenclature") in KEEP[kind]:
            row.pop(DROP, None)
            rows.append(row)
    resp["listeValeursParPeriode"] = rows
    return resp


def save(name, obj):
    with open(os.path.join(OUT, name), "w", encoding="utf-8", newline="\n") as f:
        json.dump(obj, f, ensure_ascii=False, separators=(",", ":"), sort_keys=False)
        f.write("\n")
    print("écrit", name, os.path.getsize(os.path.join(OUT, name)), "o")


def known_codes_with_data():
    """Codes présents dans data/rome.js (collecte précédente), pour repérer un code SANS donnée."""
    path = os.path.join(ROOT, "data", "rome.js")
    if not os.path.exists(path):
        return set()
    txt = open(path, encoding="utf-8").read().strip()
    return set(json.loads(txt[len("window.ROME="):-1])["items"])


def main():
    os.makedirs(OUT, exist_ok=True)
    acts = fr.call("/v1/referentiel/activites/ROME")["activites"]
    by_code = {a["codeActivite"]: a for a in acts}

    with_data = known_codes_with_data()
    no_data = next((a["codeActivite"] for a in acts if with_data and a["codeActivite"] not in with_data), None)
    chosen = ROME_CODES + ([no_data] if no_data else [])
    save("activites.json", {"activites": [by_code[c] for c in chosen]})

    index = {"rome_codes": ROME_CODES, "no_data_code": no_data, "tech_codes": TECH_CODES, "calls": {}}
    for code in chosen:
        body = dict(BASE, codeActivite=code, codeTypePeriode="TRIMESTRE", codeTypeNomenclature="ORIGINEOFF")
        resp = fr.call("/v1/indicateur/stat-offres", body)
        index["calls"][f"offres_{code}"] = "json" if resp else "empty"
        if resp:
            save(f"offres_{code}.json", reduce_resp(resp, "offres"))

    kinds = [
        ("demandeurs", "/v1/indicateur/stat-demandeurs", "TRIMESTRE", "CATCAND"),
        ("embauches", "/v1/indicateur/stat-embauches", "TRIMESTRE", "CATCANDxDUREEEMP"),
        ("tension", "/v1/indicateur/stat-perspective-employeur", "ANNEE", "TYPE_TENSION"),
    ]
    for code in TECH_CODES:
        for name, path, period, nomenclature in kinds:
            body = dict(BASE, codeActivite=code, codeTypePeriode=period, codeTypeNomenclature=nomenclature)
            resp = fr.call(path, body)
            index["calls"][f"{name}_{code}"] = "json" if resp else "empty"
            if resp:
                save(f"{name}_{code}.json", reduce_resp(resp, name))

    save("index.json", index)

    # Garde-fou : aucun secret dans les fixtures
    secrets = [fr.ENV["FT_CLIENT_SECRET"], fr.ENV["FT_CLIENT_ID"]]
    leaked = []
    for fn in os.listdir(OUT):
        text = open(os.path.join(OUT, fn), encoding="utf-8").read()
        if any(s and s in text for s in secrets) or "Bearer " in text or "access_token" in text:
            leaked.append(fn)
    if leaked:
        sys.exit("ARRÊT : secret ou jeton trouvé dans " + ", ".join(leaked) + " (supprimer ces fichiers)")
    print("OK : aucun secret dans les fixtures,", len(os.listdir(OUT)), "fichiers")


if __name__ == "__main__":
    main()
