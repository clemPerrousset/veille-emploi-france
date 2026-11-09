# Veille emploi France

Tableau de bord personnel pour suivre le marché de l'emploi en France : offres d'emploi, exposition des métiers à l'IA, tension de recrutement, indicateurs macro et bloc tech, sur une même échelle de temps.

## Ouvrir l'outil

Double-clic sur `index.html` (aucun serveur ni installation). Il faut une connexion Internet seulement pour actualiser les données, pas pour consulter.

**Première utilisation après un clonage du dépôt** : `data/data.js` (Indeed, Eurostat, DARES) est fourni, donc les vues Emploi, Secteurs, Difficultés de recrutement et Mon suivi (catégories) fonctionnent tout de suite. Les fichiers `data/rome.js` et `data/tech.js` (métiers précis, exposition IA par métier, tableau de bord tech) viennent de l'API France Travail et ne sont **pas** versionnés : crée ton `.env` puis lance `python fetch_rome.py` et `python fetch_tech.py` (voir plus bas). Sans eux, ces vues sont simplement masquées.

## Actualiser les données

Python 3.10 ou plus récent, sans aucune dépendance à installer.

```bash
python fetch_data.py    # Indeed, Eurostat, DARES : quelques secondes
python fetch_rome.py    # offres par métier (France Travail) : environ 10 minutes
python fetch_tech.py    # demandeurs, embauches, tension des métiers tech : moins d'une minute
```

`fetch_rome.py` et `fetch_tech.py` ont besoin d'un fichier `.env` (voir plus bas). `fetch_data.py` n'en a pas besoin. Les données France Travail changent au rythme trimestriel : inutile de les relancer plus d'une fois par trimestre. Indeed est quotidien.

## Le fichier `.env` (secret, jamais publié)

Copie `.env.example` en `.env`, puis renseigne les identifiants de ton application sur [francetravail.io](https://francetravail.io) (API « Marché du travail v1 » à ajouter à l'application) :

```
FT_CLIENT_ID=PAR_...
FT_CLIENT_SECRET=...
```

`.gitignore` exclut ce fichier. Ne le partage pas et ne le mets pas dans un dossier servi publiquement (sur un serveur, le secret va dans un fichier d'environnement protégé : voir « Version Rust »). Si le secret a été exposé, régénère-le sur francetravail.io (« Renouveler ») et remplace la ligne.

## Les thèmes et les vues

| Thème | Vues |
|---|---|
| Emploi | Global, métiers en montée / en descente, secteurs en montée / en descente, difficultés de recrutement |
| Exposition à l'IA | les plus exposés, exposés et en montée, par niveau d'exposition (5 groupes) |
| Tech | vue rapide (3 catégories Indeed), tableau de bord (18 métiers : offres, demandeurs, embauches, tension) |
| Mon suivi | jusqu'à 6 métiers de ton choix, catégories Indeed ou métiers précis |

Un interrupteur « Niveau de détail » bascule entre les **catégories Indeed** (34, quotidien) et les **métiers précis France Travail** (codes ROME, trimestriel). La période est réglable (1 mois à depuis 2020, ou une date de début), et les courbes macro se superposent en base 100.

## Sources et licences

| Donnée | Source | Licence / remarque |
|---|---|---|
| Offres d'emploi, 34 catégories | [Indeed Hiring Lab](https://github.com/hiring-lab/job_postings_tracker) | CC BY 4.0 (citer la source) |
| Chômage, PIB, exportations, importations | Eurostat | Réutilisation libre avec mention |
| Offres collectées, demandeurs (cat. A), emplois vacants par secteur | [DARES, open data](https://data.dares.travail-emploi.gouv.fr) | Licence Ouverte |
| Offres, demandeurs, embauches, tension par métier ROME | API « Marché du travail » de France Travail | Licence Ouverte (Etalab), avec mention de la source |
| Étude de référence sur l'exposition à l'IA | [LIFT WP 2026-02 (Banque de France / HEC)](https://heclift.github.io/website/files/papers/LIFT_WP_2026-02.pdf) | CC BY 4.0 ; leurs scores par métier ne sont pas publiés |

## Ce qui est mesuré, et ses limites

- **Indeed** mesure les offres publiées sur Indeed, pas l'emploi total ni les embauches.
- **Métiers précis (ROME)** : offres enregistrées par trimestre, arrondies à la dizaine. L'outil affiche le **cumul sur 4 trimestres glissants** (supprime la saisonnalité) et **remplace les pics isolés** (au moins 3 fois la médiane des mêmes trimestres des autres années, et +500) par cette médiane. Ce nettoyage est un choix du projet ; les données brutes contiennent de tels pics (par exemple fin 2024). Les courbes réagissent donc avec un décalage de quelques trimestres.
- **Embauches** : publiées avec plusieurs trimestres de retard. L'outil étend la période pour les inclure.
- **Secteurs** : moyenne simple des catégories Indeed d'un groupe, sans pondération par l'emploi.
- Une baisse d'offres n'est pas une preuve que l'IA en est la cause : le cycle économique, les taux d'intérêt et la correction post-Covid de la tech pèsent aussi.

## Exposition à l'IA : des estimations, pas des données officielles

Il n'existe pas de scores d'exposition publiés par métier ROME. Deux jeux d'estimations sont donc fournis, à lire comme des ordres de grandeur :

- `data/ai_exposure.js` : 34 catégories Indeed, estimation éditoriale.
- `data/ai_rome.js` : 578 métiers ROME (environ 94 % des offres), estimés par un modèle de langage (Claude, 9 novembre 2026) d'après l'intitulé du métier. Échelle de 0 à 100 : part des tâches centrales que l'IA générative peut réaliser ou assister. C'est une capacité technique potentielle, pas un usage réel ni une prévision de suppressions d'emplois.

Repères de calibrage utilisés : 85-95 (comptable, assistant administratif, développeur, analyste de données, conseiller clientèle à distance) ; 70-80 (juriste, marketing, conseiller bancaire, chef de projet informatique) ; 50-65 (commercial B2B, architecte) ; 30-45 (technicien de laboratoire, pharmacien) ; 15-25 (infirmier, vendeur en magasin, cuisinier) ; 0-10 (maçon, aide-soignant, serveur, coiffeur, conducteur de poids lourd).

Pour corriger un score, modifie le fichier à la main. Une vérification plus solide demanderait les fiches métiers de l'API ROME 4.0 (non ajoutée à l'application).

## Fichiers

| Fichier | Rôle |
|---|---|
| `index.html`, `app.js`, `style.css` | l'application (page, JavaScript, styles) |
| `vendor/chart.umd.min.js` | Chart.js 4.4.1 (MIT), pour les graphiques |
| `fetch_data.py` | collecte Indeed, Eurostat, DARES, écrit `data/data.js` |
| `fetch_rome.py` | collecte les offres des 1 900 métiers ROME, écrit `data/rome.js` |
| `fetch_tech.py` | collecte demandeurs, embauches, tension (18 métiers tech), écrit `data/tech.js`. La liste des codes est modifiable en tête du fichier |
| `data/*.js` | données chargées par `index.html` (valeurs assignées à `window.*`, pour fonctionner sans serveur) |
| `server/` | version Rust : serveur web et collecteurs (voir « Version Rust ») |
| `docs/deploy/DEPLOIEMENT.md` | guide de déploiement HTTPS sur le serveur |
| `.env` | identifiants France Travail (secret, ignoré par git) |

## Licence

Le code de ce dépôt est publié sous licence **MIT** (voir `LICENSE`). Cette licence couvre le code et les estimations d'exposition à l'IA, **pas les données des sources tierces**, qui gardent leurs propres licences (voir « Sources et licences » : Indeed Hiring Lab en CC BY 4.0, DARES en Licence Ouverte, Eurostat, API France Travail). `vendor/chart.umd.min.js` est Chart.js, sous licence MIT également (© Chart.js Contributors).

## Version Rust

Le dossier `server/` contient un portage en Rust (branche `rust-port`) : **un seul programme** qui sert l'interface (fichiers incorporés au binaire à la compilation), publie les fichiers de données et collecte lui-même les sources à heure fixe, à la place des scripts Python. Il est prévu pour être hébergé sur `https://ecostat21.applicationrush.fr` derrière nginx. La version Python et l'ouverture directe de `index.html` continuent de fonctionner.

Il faut Rust 1.88 ou plus récent. Pour compiler, le dépôt doit être cloné en entier (l'interface est lue à la racine).

```bash
cd server && cargo test
```

Le programme lit un fichier `.env` du **dossier courant**. Le tien est à la racine du dépôt : lance donc le serveur depuis la racine (ou exporte les variables toi-même).

```bash
VE_DATA_DIR=./data cargo run --release --manifest-path server/Cargo.toml -- serve
```

Au démarrage, une collecte en retard (par exemple un `data/data.js` ancien) est relancée **immédiatement au démarrage** (seul un fichier absent attend environ 30 secondes) et **peut réécrire les fichiers de `data/`**. Pour un essai local sans aucune collecte, ajoute `VE_DISABLE_SCHEDULER=1` devant la commande.

Le site est alors servi sur `http://127.0.0.1:3210`. `/health` renvoie l'état des collectes et l'âge des fichiers. Pour lancer une collecte à la main (une ligne de résultat sur la sortie standard, les journaux sur la sortie d'erreur, code de sortie 0 ou 1) :

```bash
VE_DATA_DIR=./data cargo run --release --manifest-path server/Cargo.toml -- once core
```

Les tâches possibles sont `core` (Indeed, Eurostat, DARES), `rome` et `tech` (France Travail, clé requise). Les collectes tournent aussi seules : `core` chaque jour à 02:30 UTC, France Travail le lundi à 03:00 UTC.

| Variable | Rôle | Défaut |
|---|---|---|
| `VE_BIND` | adresse d'écoute ; le programme **refuse** toute adresse non locale | `127.0.0.1:3210` |
| `VE_DATA_DIR` | dossier des fichiers de données (`data.js`, `rome.js`, `tech.js`, `ai_exposure.js`, `ai_rome.js`) | `./data` |
| `FT_CLIENT_ID`, `FT_CLIENT_SECRET` | identifiants France Travail ; sans eux, les collectes `rome` et `tech` sont désactivées (`core` fonctionne) | aucune |
| `VE_DISABLE_SCHEDULER` | `1` ou `true` : désactive les collectes planifiées (**tests uniquement**) | désactivé |

Un fichier `.env` du dossier courant est lu au démarrage (l'environnement du processus a priorité) ; en production, on utilise à la place un fichier d'environnement systemd. `ai_exposure.js` et `ai_rome.js` sont créés au premier démarrage à partir de valeurs par défaut incorporées, sans jamais écraser un fichier existant.

**Test de l'interface (développeurs)** : `tests/ui/matrix.js` parcourt automatiquement les 133 combinaisons de l'interface ; ce n'est pas un contrôle à faire sur le serveur.

**Mise en ligne** : suivez [`docs/deploy/DEPLOIEMENT.md`](docs/deploy/DEPLOIEMENT.md) (compilation sur le serveur, service systemd, nginx, HTTPS avec Let's Encrypt, exploitation).

**Statut** : les scripts Python (`fetch_*.py`) restent la **référence** tant que l'équivalence de la version Rust n'est pas acceptée. Pour `data.js`, la sortie du programme Rust a été vérifiée identique octet pour octet à celle de `fetch_data.py`.
