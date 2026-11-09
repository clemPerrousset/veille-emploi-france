#!/usr/bin/env python3
"""Télécharge une fois les données publiques et en écrit des fixtures dans server/tests/fixtures/ :
  - indeed/   : extrait réduit et déterministe des CSV Indeed (premières lignes par date) ;
  - eurostat/ : réponses JSON-stat réelles, non réduites (une par requête de macro()) ;
  - dares/    : CSV réduits (octets d'origine conservés : BOM, séparateur, fins de ligne)
                avec les lignes retenues par chaque filtre ET des lignes écartées (distractors).
Usage : python tools/make_fixtures.py [indeed|eurostat|dares ...]  (tout par défaut)"""
import csv, io, os, sys, urllib.request

BASE = "https://raw.githubusercontent.com/hiring-lab/job_postings_tracker/master/FR/"
FIXT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "server", "tests", "fixtures")
OUT = os.path.join(FIXT, "indeed")
ESTAT = "https://ec.europa.eu/eurostat/api/dissemination/statistics/1.0/data/"
DARES = "https://data.dares.travail-emploi.gouv.fr/api/explore/v2.1/catalog/datasets/"
# Mêmes requêtes que macro() dans fetch_data.py : nom de fixture -> (jeu de données, paramètres).
GDP = dict(geo="FR", unit="CLV20_MEUR", s_adj="SCA")
ESTAT_QUERIES = {
    "chomage": ("une_rt_m", dict(geo="FR", s_adj="SA", age="TOTAL", sex="T", unit="PC_ACT")),
    "pib": ("namq_10_gdp", dict(GDP, na_item="B1GQ")),
    "exports": ("namq_10_gdp", dict(GDP, na_item="P6")),
    "imports": ("namq_10_gdp", dict(GDP, na_item="P7")),
}
DARES_SETS = {
    "offres": "dares_offres_collectees_satisfaites_france_travail_cvs-cjo_mens",
    "dei": "dares_defm_stock_france_cvs",
    "vacants": "dares_emploivacants_cvs_emploisvacants",
}
N = 40
CATS = ["Accounting", "Banking & Finance", "IT Infrastructure, Operations & Support"]


def get(name):
    req = urllib.request.Request(BASE + name, headers={"User-Agent": "suivi-offre-emploi/1.0"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read().decode("utf-8")


def write(name, header, rows):
    os.makedirs(OUT, exist_ok=True)
    with open(os.path.join(OUT, name), "w", encoding="utf-8", newline="") as f:
        w = csv.writer(f, lineterminator="\n")
        w.writerow(header)
        w.writerows(rows)


def get_bytes(url):
    req = urllib.request.Request(url, headers={"User-Agent": "suivi-offre-emploi/1.0"})
    with urllib.request.urlopen(req, timeout=120) as r:
        return r.read()


def write_bytes(sub, name, data):
    d = os.path.join(FIXT, sub)
    os.makedirs(d, exist_ok=True)
    with open(os.path.join(d, name), "wb") as f:  # octets : Windows ne doit rien modifier
        f.write(data)


def eurostat_fixtures():
    for name, (dataset, params) in ESTAT_QUERIES.items():
        q = "&".join(f"{k}={v}" for k, v in params.items())
        write_bytes("eurostat", name + ".json", get_bytes(f"{ESTAT}{dataset}?{q}&sinceTimePeriod=2019-01"))


def reduce_dares(raw, keep_rules):
    """Garde l'en-tête et, pour chaque règle (prédicat, premiers, derniers), les premières/dernières
    lignes (ordre du fichier) qui la satisfont. Les lignes sont réécrites telles quelles (octets)."""
    eol = b"\r\n" if b"\r\n" in raw[:2000] else b"\n"
    lines = raw.split(eol)
    if lines and lines[-1] == b"":
        lines.pop()
    head = lines[0]
    bom = "\ufeff".encode("utf-8")
    body = head
    while body.startswith(bom):
        body = body[len(bom):]
    names = next(csv.reader(io.StringIO(body.decode("utf-8")), delimiter=";"))
    recs = [(i, dict(zip(names, next(csv.reader(io.StringIO(l.decode("utf-8")), delimiter=";")))))
            for i, l in enumerate(lines[1:], 1)]
    chosen = set()
    for pred, nfirst, nlast in keep_rules:
        hits = [i for i, r in recs if pred(r)]
        if not hits:
            raise SystemExit("aucune ligne pour une règle de sélection : fichier réel inattendu")
        chosen.update(hits[:nfirst])
        if nlast:
            chosen.update(hits[-nlast:])
    return eol.join([head] + [lines[i] for i in sorted(chosen)]) + eol


def dares_fixtures():
    def offres_ok(r):
        return (r["Code région"] == "Total France métropolitaine" and r["Type d'emploi"] == "Total"
                and r["Qualification"] == "Total" and r["Type d'offre d'emploi"].startswith("Offres d'emploi collect"))
    rules = {
        "offres": [
            (lambda r: offres_ok(r) and r["Date"] < "2019-01", 0, 2),   # avant 2019 : écartées
            (lambda r: offres_ok(r) and r["Date"] >= "2019-01", 6, 3),  # retenues
            (lambda r: r["Code région"] != "Total France métropolitaine" and r["Type d'emploi"] == "Total"
             and r["Date"] == "2019-01" and r["Type d'offre d'emploi"].startswith("Offres d'emploi collect"), 2, 0),
            (lambda r: r["Code région"] == "Total France métropolitaine" and r["Type d'emploi"] != "Total"
             and r["Date"] == "2019-01", 1, 0),  # autre type d'emploi
            (lambda r: r["Code région"] == "Total France métropolitaine" and r["Type d'emploi"] == "Total"
             and r["Type d'offre d'emploi"].startswith("Offres d'emploi satisfaites"), 1, 0),
            (lambda r: r["Code région"] == "Total France" and r["Type d'emploi"] == "Total" and r["Date"] == "2019-01", 1, 0),
        ],
        "dei": [],
        "vacants": [],
    }
    def dei_base(r):
        return (r["Sexe"] == "Total" and r["Tranche d'âge"] == "Total"
                and r["Tranche d'heures travaillées"] == "Total" and r["Ancienneté"] == "Total")
    rules["dei"] = [
        (lambda r: r["Champ"] == "France métropolitaine" and r["Catégorie"] == "A" and dei_base(r) and r["Date"] < "2019-01", 0, 2),
        (lambda r: r["Champ"] == "France métropolitaine" and r["Catégorie"] == "A" and dei_base(r) and r["Date"] >= "2019-01", 6, 3),
        (lambda r: r["Champ"] == "France métropolitaine" and r["Catégorie"] == "B" and dei_base(r) and r["Date"] == "2019-01", 1, 0),
        (lambda r: r["Champ"] == "France métropolitaine" and r["Catégorie"] == "A" and r["Sexe"] == "Femmes"
         and r["Tranche d'âge"] == "Total" and r["Date"] == "2019-01", 1, 0),
        (lambda r: r["Champ"] == "France métropolitaine" and r["Catégorie"] == "A" and r["Sexe"] == "Total"
         and r["Tranche d'âge"] == "Moins de 25 ans" and r["Date"] == "2019-01", 1, 0),
        (lambda r: r["Champ"] == "France" and r["Catégorie"] == "A" and dei_base(r) and r["Date"] == "2019-01", 1, 0),
    ]
    quarters = {"2018-T4", "2019-T1", "2019-T2", "2020-T1", "2026-T2"}  # 2020-T1 contient des taux vides
    rules["vacants"] = [
        (lambda r: r["Nomenclature"] == "A10" and r["Champ"] == "Plus de 10 salariés" and r["Date"] in quarters, 999, 0),
        (lambda r: r["Nomenclature"] == "A4" and r["Champ"] == "Plus de 10 salariés" and r["Date"] == "2019-T1", 1, 0),
        (lambda r: r["Nomenclature"] == "A10" and r["Champ"] == "Ensemble" and r["Date"] == "2019-T1", 1, 0),
    ]
    for key, dataset in DARES_SETS.items():
        raw = get_bytes(f"{DARES}{dataset}/exports/csv?use_labels=true")
        write_bytes("dares", key + ".csv", reduce_dares(raw, rules[key]))


def main():
    what = sys.argv[1:] or ["indeed", "eurostat", "dares"]
    if "eurostat" in what:
        eurostat_fixtures()
    if "dares" in what:
        dares_fixtures()
    if "indeed" in what:
        indeed_fixtures()


def indeed_fixtures():
    rd = csv.reader(io.StringIO(get("aggregate_job_postings_FR.csv")))
    header = next(rd)
    di, vi = header.index("date"), header.index("variable")
    rows = list(rd)
    keep = []
    for var in ("total postings", "new postings"):
        keep += sorted((r for r in rows if r[vi] == var), key=lambda r: r[di])[:N]
    write("aggregate.csv", header, keep)

    rd = csv.reader(io.StringIO(get("job_postings_by_sector_FR.csv")))
    header = next(rd)
    di, vi, ni = header.index("date"), header.index("variable"), header.index("display_name")
    rows = list(rd)
    names = {r[ni] for r in rows}
    missing = [c for c in CATS if c not in names]
    if missing:
        raise SystemExit(f"catégories absentes du fichier réel : {missing}")
    keep = []
    for var, n in (("total postings", N), ("new postings", 3)):
        for c in CATS:
            keep += sorted((r for r in rows if r[vi] == var and r[ni] == c), key=lambda r: r[di])[:n]
    write("sectors.csv", header, keep)


if __name__ == "__main__":
    main()
