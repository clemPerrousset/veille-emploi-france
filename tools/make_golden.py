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
    ("age=Y_LT25", "chomage_jeunes"),  # avant « une_rt_m » : même jeu de données que le chômage total
    ("une_rt_m", "chomage"), ("na_item=B1GQ", "pib"), ("na_item=P6", "exports"), ("na_item=P7", "imports"),
    ("namq_10_pe", "emploi"), ("ei_bsse_m_r2", "intentions"), ("irt_st_m", "euribor"),
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
for key in ("chomage", "pib", "exports", "imports", "emploi", "intentions", "euribor", "chomage_jeunes"):
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


# --- ROME, tech, tension et carte (API France Travail) : on exécute main() des prototypes sur les fixtures ----------
import builtins, io, sys, tempfile

FT_FIX = os.path.join(FIXROOT, "ft")
# chemin d'API -> (préfixe de fixture, nomenclature de requête attendue)
FT_BY_PATH = {
    "/v1/indicateur/stat-offres": ("offres", "ORIGINEOFF"),
    "/v1/indicateur/stat-demandeurs": ("demandeurs", "CATCAND"),
    "/v1/indicateur/stat-embauches": ("embauches", "CATCANDxDUREEEMP"),
    "/v1/indicateur/stat-perspective-employeur": ("tension", "TYPE_TENSION"),
}


def load_module_without_env(name):
    """Importe fetch_rome.py / fetch_tech.py sans lire le vrai .env (secret) : `open` est détourné
    pour ce seul fichier, qui reçoit des valeurs bidon."""
    real_open = builtins.open

    def fake_open(path, *a, **k):
        if os.path.basename(str(path)) == ".env":
            return io.StringIO("FT_CLIENT_ID=fake\nFT_CLIENT_SECRET=fake\n")
        return real_open(path, *a, **k)

    builtins.open = fake_open
    try:
        spec = importlib.util.spec_from_file_location(name, os.path.join(ROOT, name + ".py"))
        mod = importlib.util.module_from_spec(spec)
        sys.modules[name] = mod  # fetch_tech fait « import fetch_rome as fr »
        spec.loader.exec_module(mod)
    finally:
        builtins.open = real_open
    return mod


def ft_call(path, body=None, tries=5):
    """Remplace fetch_rome.call : lit les fixtures enregistrées (jamais de réseau)."""
    if path == "/v1/referentiel/activites/ROME":
        fname = "activites.json"
    else:
        prefix, nomenclature = FT_BY_PATH[path]
        if body["codeTypeNomenclature"] != nomenclature:
            raise SystemExit("nomenclature inattendue pour " + path + " : " + body["codeTypeNomenclature"])
        if body["codeTypeTerritoire"] == "DEP":  # collecteur `carte` : une fixture par métier et département
            fname = "carte_" + prefix + "_" + body["codeActivite"] + "_" + body["codeTerritoire"] + ".json"
        else:
            fname = prefix + "_" + body["codeActivite"] + ".json"
    full = os.path.join(FT_FIX, fname)
    if not os.path.exists(full):
        return None  # comme un 404 de l'API
    with open(full, encoding="utf-8") as f:
        return json.load(f)


class FixedDate2:
    @staticmethod
    def today():
        import datetime
        return datetime.date(2026, 11, 9)


def run_main_on_fixtures(mod, out_name, prefix, seed_files=None, setup=None):
    """Exécute mod.main() avec HERE pointant vers un dossier temporaire (jamais le data/ du dépôt).
    `seed_files` : {nom: texte} déposés dans le data/ temporaire avant l'exécution (entrées du module) ;
    `setup(mod, tmp)` : réglages du module qui dépendent du dossier temporaire (ex. son fichier de reprise)."""
    with tempfile.TemporaryDirectory() as tmp:
        os.makedirs(os.path.join(tmp, "data"))
        for name, text in (seed_files or {}).items():
            with open(os.path.join(tmp, "data", name), "w", encoding="utf-8", newline="") as f:
                f.write(text)
        mod.HERE = tmp
        mod.date = FixedDate2
        if setup:
            setup(mod, tmp)
        mod.main()
        with open(os.path.join(tmp, "data", out_name), encoding="utf-8") as f:
            text = f.read()
    assert text.startswith(prefix) and text.endswith(";"), text[:40]
    return json.loads(text[len(prefix):-1])


fr = load_module_without_env("fetch_rome")
fr.call = ft_call
ft = load_module_without_env("fetch_tech")
ft.TECH_CODES = ["M1805", "M1856"]  # les deux codes dont les fixtures sont enregistrées
dump("rome.json", run_main_on_fixtures(fr, "rome.js", "window.ROME="))
dump("tech.json", run_main_on_fixtures(ft, "tech.js", "window.TECH="))

# fetch_tension.main() lit data/rome.js : on lui donne celui que la partie rome vient de produire (mêmes octets
# que fetch_rome.py l'écrit). Les 3 métiers de la fixture sont tous retenus (N = 300 > 3) ; un second jeu
# réduit à N = 2 vérifie la sélection par volume (K1303 puis M1805 devant A1401).
tn = load_module_without_env("fetch_tension")
rome_obj = run_main_on_fixtures(fr, "rome.js", "window.ROME=")
rome_js = "window.ROME=" + json.dumps(rome_obj, ensure_ascii=False, separators=(",", ":")) + ";"
dump("tension.json", run_main_on_fixtures(tn, "tension.js", "window.TENSION=", {"rome.js": rome_js}))
top_codes_full = tn.top_codes
tn.top_codes = lambda rome, n=2: top_codes_full(rome, n)
dump("tension_top2.json", run_main_on_fixtures(tn, "tension.js", "window.TENSION=", {"rome.js": rome_js}))
tn.top_codes = top_codes_full

# fetch_carte.main() : mêmes entrées que le serveur (rome.js du jeu de référence rome), N et DEPS réduits par
# assignation du module (les valeurs de production sont 100 métiers x 96 départements), fichier de reprise
# (CACHE) détourné vers le dossier temporaire : le data/ du dépôt (carte_cache.json, carte.js) n'est jamais lu
# ni écrit. N vaut 2 (plus EXTRA_CODES réduit, voir ci-dessous) ; les départements gardent l'ordre de la liste de
# production (2A entre 19 et 21). `top_codes` a `n=N` en valeur par défaut figée à la définition : on la
# redéfinit comme pour la tension. Date figée au 2026-11-09 (FixedDate2).
cm = load_module_without_env("fetch_carte")
CARTE_N = 2  # K1303 et M1805 : A1401 (3e en volume) n'entre que par EXTRA_CODES
cm.N = CARTE_N
# métiers ajoutés hors top N : A1401 (présent dans rome.js, hors du top réduit : ajouté), K1303 (déjà dans le
# top : pas de doublon), ZZ999 (absent de rome.js : ignoré)
cm.EXTRA_CODES = ["A1401", "K1303", "ZZ999"]
cm.DEPS = ["2A", "48", "75", "90"]
carte_top_codes_full = cm.top_codes
cm.top_codes = lambda rome, n=CARTE_N: carte_top_codes_full(rome, n)
dump("carte.json", run_main_on_fixtures(
    cm, "carte.js", "window.CARTE=", {"rome.js": rome_js},
    setup=lambda mod, tmp: setattr(mod, "CACHE", os.path.join(tmp, "data", "carte_cache.json"))))
