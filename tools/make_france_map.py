#!/usr/bin/env python3
"""Produit data/france.js : contours simplifiés des départements métropolitains, en chemins SVG.

Source : https://github.com/gregoiredavid/france-geojson (départements, version simplifiée), dérivé d'IGN
Admin Express (Licence Ouverte Etalab 2.0). Projection plate (longitude corrigée par cos de la latitude
moyenne), puis simplification de Douglas-Peucker, coordonnées entières dans une boîte de 1000 de large.
Sortie : data/france.js -> window.FRANCE = {w, h, deps:[{c, n, d}]}
Usage : python tools/make_france_map.py   (réseau requis, une seule fois)
"""
import json, math, os, urllib.request

ROOT = os.path.join(os.path.dirname(os.path.abspath(__file__)), "..")
URL = "https://raw.githubusercontent.com/gregoiredavid/france-geojson/master/departements-version-simplifiee.geojson"
WIDTH = 1000
TOL = 0.9      # tolérance de simplification, en unités de la boîte (1000 de large)
MIN_AREA = 6   # les îlots plus petits (en unités^2) sont supprimés


def fetch():
    req = urllib.request.Request(URL, headers={"User-Agent": "suivi-offre-emploi/1.0"})
    with urllib.request.urlopen(req, timeout=60) as r:
        return json.loads(r.read().decode("utf-8"))


def rings(geom):
    polys = geom["coordinates"] if geom["type"] == "MultiPolygon" else [geom["coordinates"]]
    return [ring for poly in polys for ring in poly]


def dp(pts, tol):
    """Douglas-Peucker (version itérative)."""
    if len(pts) < 3:
        return pts
    keep = [False] * len(pts)
    keep[0] = keep[-1] = True
    stack = [(0, len(pts) - 1)]
    while stack:
        a, b = stack.pop()
        (x1, y1), (x2, y2) = pts[a], pts[b]
        dx, dy = x2 - x1, y2 - y1
        norm = math.hypot(dx, dy)
        far, idx = 0.0, None
        for i in range(a + 1, b):
            x, y = pts[i]
            d = abs(dy * (x - x1) - dx * (y - y1)) / norm if norm else math.hypot(x - x1, y - y1)
            if d > far:
                far, idx = d, i
        if idx is not None and far > tol:
            keep[idx] = True
            stack += [(a, idx), (idx, b)]
    return [p for p, k in zip(pts, keep) if k]


def area(pts):
    return abs(sum(pts[i][0] * pts[(i + 1) % len(pts)][1] - pts[(i + 1) % len(pts)][0] * pts[i][1] for i in range(len(pts)))) / 2


def main():
    gj = fetch()
    feats = [f for f in gj["features"] if f["properties"]["code"] in {*(f"{i:02d}" for i in range(1, 96) if i != 20), "2A", "2B"}]
    lons = [x for f in feats for r in rings(f["geometry"]) for x, _ in r]
    lats = [y for f in feats for r in rings(f["geometry"]) for _, y in r]
    lon0, lon1, lat0, lat1 = min(lons), max(lons), min(lats), max(lats)
    k = math.cos(math.radians((lat0 + lat1) / 2))
    scale = WIDTH / ((lon1 - lon0) * k)
    height = round((lat1 - lat0) * scale)

    def proj(x, y):
        return ((x - lon0) * k * scale, (lat1 - y) * scale)

    deps = []
    for f in feats:
        parts = []
        for ring in rings(f["geometry"]):
            pts = dp([proj(x, y) for x, y in ring], TOL)
            if len(pts) < 4 or area(pts) < MIN_AREA:
                continue
            parts.append("M" + "L".join(f"{round(x)} {round(y)}" for x, y in pts[:-1]) + "Z")
        deps.append({"c": f["properties"]["code"], "n": f["properties"]["nom"], "d": "".join(parts)})
    deps.sort(key=lambda d: (len(d["c"]), d["c"]))
    out = {"w": WIDTH, "h": height, "deps": deps}
    path = os.path.join(ROOT, "data", "france.js")
    with open(path, "w", encoding="utf-8") as f:
        f.write("window.FRANCE=" + json.dumps(out, ensure_ascii=False, separators=(",", ":")) + ";")
    print("écrit data/france.js :", len(deps), "départements,", os.path.getsize(path) // 1024, "Ko, boîte", WIDTH, "x", height)


if __name__ == "__main__":
    main()
