#!/usr/bin/env python3
"""Produit les jeux de référence en exécutant le prototype Python sur les fixtures."""
import importlib.util, json, os

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
FIX = os.path.join(ROOT, "server", "tests", "fixtures", "indeed")
GOLD = os.path.join(ROOT, "server", "tests", "golden")

spec = importlib.util.spec_from_file_location("fetch_data", os.path.join(ROOT, "fetch_data.py"))
fd = importlib.util.module_from_spec(spec)
spec.loader.exec_module(fd)


def fake_get(url):
    name = "aggregate.csv" if url.endswith("aggregate_job_postings_FR.csv") else "sectors.csv"
    with open(os.path.join(FIX, name), encoding="utf-8", newline="") as f:
        return f.read()


fd.get = fake_get
os.makedirs(GOLD, exist_ok=True)
for fname, fn in (("indeed_aggregate.json", fd.indeed_aggregate), ("indeed_sectors.json", fd.indeed_sectors)):
    with open(os.path.join(GOLD, fname), "w", encoding="utf-8") as f:
        json.dump(fn(), f, ensure_ascii=False)
