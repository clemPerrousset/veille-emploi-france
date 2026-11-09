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

`.gitignore` exclut ce fichier. Ne le partage pas et ne le mets pas sur un serveur public. Si le secret a été exposé, régénère-le sur francetravail.io (« Renouveler ») et remplace la ligne.

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
| Offres, demandeurs, embauches, tension par métier ROME | API « Marché du travail » de France Travail | Compte et conditions d'usage de francetravail.io |
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
| `index.html` | l'application (HTML, CSS et JavaScript) |
| `vendor/chart.umd.min.js` | Chart.js 4.4.1 (MIT), pour les graphiques |
| `fetch_data.py` | collecte Indeed, Eurostat, DARES, écrit `data/data.js` |
| `fetch_rome.py` | collecte les offres des 1 900 métiers ROME, écrit `data/rome.js` |
| `fetch_tech.py` | collecte demandeurs, embauches, tension (18 métiers tech), écrit `data/tech.js`. La liste des codes est modifiable en tête du fichier |
| `data/*.js` | données chargées par `index.html` (valeurs assignées à `window.*`, pour fonctionner sans serveur) |
| `.env` | identifiants France Travail (secret, ignoré par git) |

## Projet : portage futur

Cette version est un prototype local. Un portage en Rust, hébergé sur `https://ecostat21.applicationrush.fr`, est prévu plus tard. Les scripts de collecte sont séparés de l'interface et les données ont des formats simples pour faciliter ce portage. En hébergement, le secret France Travail devra être une variable d'environnement du serveur (jamais un fichier public), et la collecte devra tourner côté serveur.
