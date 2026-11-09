#!/usr/bin/env python3
"""Télécharge les données publiques et produit data/*.json pour l'interface.

Sources :
  - Indeed Hiring Lab (CC BY 4.0) : offres d'emploi France, global + 35 catégories
  - Eurostat : chômage mensuel, PIB trimestriel, exportations/importations trimestrielles (France)
Usage : python fetch_data.py
"""
import csv, io, json, os, urllib.request
from datetime import date

HERE = os.path.dirname(os.path.abspath(__file__))
OUT = os.path.join(HERE, "data")
os.makedirs(OUT, exist_ok=True)

INDEED = "https://raw.githubusercontent.com/hiring-lab/job_postings_tracker/master/FR/"
ESTAT = "https://ec.europa.eu/eurostat/api/dissemination/statistics/1.0/data/"


def get(url):
    req = urllib.request.Request(url, headers={"User-Agent": "suivi-offre-emploi/1.0"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return r.read().decode("utf-8")


def indeed_aggregate():
    rows = list(csv.DictReader(io.StringIO(get(INDEED + "aggregate_job_postings_FR.csv"))))
    out = {}
    for variable in ("total postings", "new postings"):
        sel = [r for r in rows if r["variable"] == variable]
        out[variable] = {
            "dates": [r["date"] for r in sel],
            "values": [float(r["indeed_job_postings_index_SA"]) for r in sel],
        }
    return out


def indeed_sectors():
    rows = csv.DictReader(io.StringIO(get(INDEED + "job_postings_by_sector_FR.csv")))
    series, dates = {}, {}
    for r in rows:
        if r["variable"] != "total postings":
            continue
        name = r["display_name"]
        dates.setdefault(name, []).append(r["date"])
        series.setdefault(name, []).append(float(r["indeed_job_postings_index"]))
    ref = max(dates.values(), key=len)
    for name, d in dates.items():
        assert d == ref, f"dates différentes pour {name}"
    return {"dates": ref, "series": series}


def period_to_date(p):
    if "-Q" in p:
        y, q = p.split("-Q")
        return f"{y}-{(int(q) - 1) * 3 + 2:02d}-15"  # milieu du trimestre
    y, m = p.split("-")
    return f"{y}-{m}-15"


def eurostat(dataset, params):
    q = "&".join(f"{k}={v}" for k, v in params.items())
    js = json.loads(get(f"{ESTAT}{dataset}?{q}&sinceTimePeriod=2019-01"))
    cats = js["dimension"]["time"]["category"]["index"]
    values = js["value"]
    pts = []
    for label, pos in sorted(cats.items(), key=lambda x: x[1]):
        v = values.get(str(pos))
        if v is None:
            continue
        pts.append([period_to_date(label), v])
    return pts


def macro():
    gdp = dict(geo="FR", unit="CLV20_MEUR", s_adj="SCA")
    return {
        "chomage": {
            "label": "Taux de chômage",
            "freq": "mensuel",
            "points": eurostat("une_rt_m", dict(geo="FR", s_adj="SA", age="TOTAL", sex="T", unit="PC_ACT")),
        },
        "pib": {"label": "PIB (volume)", "freq": "trimestriel",
                "points": eurostat("namq_10_gdp", dict(gdp, na_item="B1GQ"))},
        "exports": {"label": "Exportations (volume)", "freq": "trimestriel",
                    "points": eurostat("namq_10_gdp", dict(gdp, na_item="P6"))},
        "imports": {"label": "Importations (volume)", "freq": "trimestriel",
                    "points": eurostat("namq_10_gdp", dict(gdp, na_item="P7"))},
    }


DARES = "https://data.dares.travail-emploi.gouv.fr/api/explore/v2.1/catalog/datasets/"


def dares_rows(dataset):
    txt = get(f"{DARES}{dataset}/exports/csv?use_labels=true").replace("﻿", "")
    return list(csv.DictReader(io.StringIO(txt), delimiter=";"))


def dares_date(p):
    if "-T" in p:  # trimestre, ex. 2026-T2
        y, q = p.split("-T")
        return f"{y}-{(int(q) - 1) * 3 + 2:02d}-15"
    return p + "-15"  # mois


def dares_series():
    """Offres France Travail et demandeurs d'emploi (mensuels, CVS-CJO, France métropolitaine)."""
    offres = [r for r in dares_rows("dares_offres_collectees_satisfaites_france_travail_cvs-cjo_mens")
              if r["Code région"] == "Total France métropolitaine" and r["Type d'emploi"] == "Total"
              and r["Qualification"] == "Total" and r["Type d'offre d'emploi"].startswith("Offres d'emploi collect")]
    dei = [r for r in dares_rows("dares_defm_stock_france_cvs")
           if r["Champ"] == "France métropolitaine" and r["Catégorie"] == "A" and r["Sexe"] == "Total"
           and r["Tranche d'âge"] == "Total" and r["Tranche d'heures travaillées"] == "Total" and r["Ancienneté"] == "Total"]

    def pts(rows, col):
        out = sorted((dares_date(r["Date"]), float(r[col])) for r in rows if r["Date"] >= "2019-01")
        return [list(p) for p in out]
    return {
        "ft_offres": {"label": "Offres France Travail", "freq": "mensuel", "points": pts(offres, "Nombre d'offres d'emploi")},
        "ft_dei": {"label": "Demandeurs d'emploi (cat. A)", "freq": "mensuel", "points": pts(dei, "Nombre de demandeurs d'emploi")},
    }


def vacants():
    """Taux d'emplois vacants par secteur (NAF A10, entreprises de plus de 10 salariés, CVS-CJO, trimestriel)."""
    out = {}
    for r in dares_rows("dares_emploivacants_cvs_emploisvacants"):
        if r["Nomenclature"] != "A10" or r["Champ"] != "Plus de 10 salariés" or r["Date"] < "2019-T1":
            continue
        if not r["Taux d'emplois vacants (en %)"]:
            continue  # taux non publié (secret statistique)
        out.setdefault(r["Code NAF"], {"label": r["Libellé NAF"], "points": []})["points"].append(
            [dares_date(r["Date"]), float(r["Taux d'emplois vacants (en %)"])])
    for v in out.values():
        v["points"].sort()
    return out


def dump(name, obj):
    path = os.path.join(OUT, name)
    with open(path, "w", encoding="utf-8") as f:
        json.dump(obj, f, ensure_ascii=False, separators=(",", ":"))
    print("écrit", name, os.path.getsize(path) // 1024, "Ko")


if __name__ == "__main__":
    data = {
        "aggregate": indeed_aggregate(),
        "sectors": indeed_sectors(),
        "macro": {**macro(), **dares_series()},
        "vacants": vacants(),
        "updated": date.today().isoformat(),
    }
    # data.js : chargeable par double-clic sur index.html (fetch() est bloqué en file://)
    path = os.path.join(OUT, "data.js")
    with open(path, "w", encoding="utf-8") as f:
        f.write("window.DATA=" + json.dumps(data, ensure_ascii=False, separators=(",", ":")) + ";")
    print("écrit data.js", os.path.getsize(path) // 1024, "Ko")
