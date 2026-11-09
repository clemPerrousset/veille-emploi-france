#!/usr/bin/env python3
"""Récupère, par métier ROME, le nombre d'offres enregistrées par trimestre (API France Travail
« Marché du travail » v1, indicateur OFF_1, série « Toutes offres », niveau France).

Prérequis : fichier .env (jamais publié, cf. .gitignore) avec FT_CLIENT_ID et FT_CLIENT_SECRET.
Sortie : data/rome.js  ->  window.ROME = {updated, periods:[...], items:{CODE:{l:libellé, o:[...]}}}
Usage : python fetch_rome.py
"""
import json, os, sys, time, threading, urllib.parse, urllib.request, urllib.error
from concurrent.futures import ThreadPoolExecutor
from datetime import date

HERE = os.path.dirname(os.path.abspath(__file__))
BASE = "https://api.francetravail.io/partenaire/stats-offres-demandes-emploi"
TOKEN_URL = "https://entreprise.francetravail.fr/connexion/oauth2/access_token?realm=%2Fpartenaire"
SCOPE = "api_stats-offres-demandes-emploiv1 offresetdemandesemploi"
WORKERS = 4          # l'application est limitée à 10 appels/seconde ; on reste très en dessous
MIN_PERIOD = "2021T1"


def load_env():
    env = {}
    with open(os.path.join(HERE, ".env"), encoding="utf-8") as f:
        for line in f:
            line = line.strip()
            if line and not line.startswith("#") and "=" in line:
                k, v = line.split("=", 1)
                env[k.strip()] = v.strip()
    for k in ("FT_CLIENT_ID", "FT_CLIENT_SECRET"):
        if not env.get(k):
            sys.exit(f"{k} manquant dans .env")
    return env


ENV = load_env()
_tok = {"v": None, "exp": 0}
_lock = threading.Lock()


def token():
    with _lock:
        if _tok["v"] and time.time() < _tok["exp"] - 60:
            return _tok["v"]
        data = urllib.parse.urlencode({
            "grant_type": "client_credentials", "client_id": ENV["FT_CLIENT_ID"],
            "client_secret": ENV["FT_CLIENT_SECRET"], "scope": SCOPE}).encode()
        req = urllib.request.Request(TOKEN_URL, data=data, headers={"Content-Type": "application/x-www-form-urlencoded"})
        r = json.load(urllib.request.urlopen(req, timeout=30))
        _tok["v"], _tok["exp"] = r["access_token"], time.time() + int(r.get("expires_in", 1400))
        return _tok["v"]


def call(path, body=None, tries=5):
    for i in range(tries):
        req = urllib.request.Request(
            BASE + path, data=json.dumps(body).encode() if body is not None else None,
            headers={"Authorization": "Bearer " + token(), "Content-Type": "application/json", "Accept": "application/json"})
        try:
            return json.load(urllib.request.urlopen(req, timeout=60))
        except urllib.error.HTTPError as e:
            if e.code == 429 or e.code >= 500:
                time.sleep(1.5 * (i + 1)); continue
            if e.code in (204, 404):
                return None
            raise
        except (urllib.error.URLError, TimeoutError):
            time.sleep(1.5 * (i + 1))
    return None


def offers_for(code):
    body = {"codeTypeTerritoire": "NAT", "codeTerritoire": "FR", "codeTypeActivite": "ROME",
            "codeActivite": code, "codeTypePeriode": "TRIMESTRE", "codeTypeNomenclature": "ORIGINEOFF"}
    r = call("/v1/indicateur/stat-offres", body)
    if not r:
        return {}
    return {x["codePeriode"]: x["valeurPrincipaleNombre"] for x in r.get("listeValeursParPeriode", [])
            if x.get("codeNomenclature") == "TOFF" and x.get("codePeriode", "") >= MIN_PERIOD
            and x.get("valeurPrincipaleNombre") is not None}


def main():
    acts = call("/v1/referentiel/activites/ROME")["activites"]
    print(len(acts), "métiers ROME à interroger")
    results, done = {}, [0]

    def work(a):
        try:
            res = offers_for(a["codeActivite"])
        except Exception as e:  # un métier en échec ne doit pas bloquer les autres
            res = {}
            print("échec", a["codeActivite"], e)
        with _lock:
            results[a["codeActivite"]] = (a["libelleActivite"], res)
            done[0] += 1
            if done[0] % 100 == 0:
                print(done[0], "/", len(acts), flush=True)

    with ThreadPoolExecutor(WORKERS) as ex:
        list(ex.map(work, acts))

    periods = sorted({p for _, r in results.values() for p in r})
    items = {}
    for code, (label, r) in results.items():
        if r:
            items[code] = {"l": label, "o": [r.get(p) for p in periods]}
    out = {"updated": date.today().isoformat(), "periods": periods, "items": items}
    path = os.path.join(HERE, "data", "rome.js")
    with open(path, "w", encoding="utf-8") as f:
        f.write("window.ROME=" + json.dumps(out, ensure_ascii=False, separators=(",", ":")) + ";")
    print("écrit data/rome.js :", len(items), "métiers,", len(periods), "trimestres,", os.path.getsize(path) // 1024, "Ko")


if __name__ == "__main__":
    main()
