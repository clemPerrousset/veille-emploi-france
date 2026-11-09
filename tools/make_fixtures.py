#!/usr/bin/env python3
"""Télécharge une fois les CSV publics Indeed et en écrit un extrait réduit et déterministe
(premières lignes par date, pas les dernières) dans server/tests/fixtures/indeed/."""
import csv, io, os, urllib.request

BASE = "https://raw.githubusercontent.com/hiring-lab/job_postings_tracker/master/FR/"
OUT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "server", "tests", "fixtures", "indeed")
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


def main():
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
