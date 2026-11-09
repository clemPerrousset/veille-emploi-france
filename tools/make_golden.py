#!/usr/bin/env python3
"""Produit les jeux de référence en exécutant le prototype Python sur les fixtures."""
import importlib.util, json, os

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
FIXROOT = os.path.join(ROOT, "server", "tests", "fixtures")
FIX = os.path.join(FIXROOT, "indeed")
GOLD = os.path.join(ROOT, "server", "tests", "golden")

spec = importlib.util.spec_from_file_location("fetch_data", os.path.join(ROOT, "fetch_data.py"))
fd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fd)


ESTAT_BY_QUERY = (  # fragment d'URL -> fixture Eurostat (mêmes requêtes que fd.macro())
    ("une_rt_m", "chomage"), ("na_item=B1GQ", "pib"), ("na_item=P6", "exports"), ("na_item=P7", "imports"),
)
DARES_BY_DATASET = {
    "dares_offres_collectees_satisfaites_france_travail_cvs-cjo_mens": "offres.csv",
    "dares_defm_stock_france_cvs": "dei.csv",
    "dares_emploivacants_cvs_emploisvacants": "vacants.csv",
}


def read_text(path):
    with open(path, "rb") as f:  # octets : on garde les BOM et fins de ligne d'origine
        return f.read().decode("utf-8")


def fake_get(url):
    if url.startswith(fd.ESTAT):
        for frag, name in ESTAT_BY_QUERY:
            if frag in url:
                return read_text(os.path.join(FIXROOT, "eurostat", name + ".json"))
        raise SystemExit("requête Eurostat sans fixture : " + url)
    if url.startswith(fd.DARES):
        dataset = url[len(fd.DARES):].split("/")[0]
        return read_text(os.path.join(FIXROOT, "dares", DARES_BY_DATASET[dataset]))
    name = "aggregate.csv" if url.endswith("aggregate_job_postings_FR.csv") else "sectors.csv"
    with open(os.path.join(FIX, name), encoding="utf-8", newline="") as f:
        return f.read()


fd.get = fake_get
os.makedirs(GOLD, exist_ok=True)
for fname, fn in (("indeed_aggregate.json", fd.indeed_aggregate), ("indeed_sectors.json", fd.indeed_sectors)):
    with open(os.path.join(GOLD, fname), "w", encoding="utf-8") as f:
        json.dump(fn(), f, ensure_ascii=False)

def dump(fname, obj):
    with open(os.path.join(GOLD, fname), "w", encoding="utf-8") as f:
        json.dump(obj, f, ensure_ascii=False)


macro = fd.macro()  # appelle fd.eurostat() pour chaque requête de macro()
for key in ("chomage", "pib", "exports", "imports"):
    dump(f"eurostat_{key}.json", macro[key]["points"])
dares = fd.dares_series()
dump("dares_offres.json", dares["ft_offres"]["points"])
dump("dares_dei.json", dares["ft_dei"]["points"])
dump("dares_vacants.json", fd.vacants())


class FixedDate:  # remplace datetime.date dans le prototype : date.today() figée
    @staticmethod
    def today():
        import datetime
        return datetime.date(2026, 11, 9)


fd.date = FixedDate  # la charge utile de data.js est assemblée comme dans fetch_data.py (__main__)
dump("data.json", {
    "aggregate": fd.indeed_aggregate(),
    "sectors": fd.indeed_sectors(),
    "macro": {**fd.macro(), **fd.dares_series()},
    "vacants": fd.vacants(),
    "updated": fd.date.today().isoformat(),
})
