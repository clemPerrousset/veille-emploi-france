# Veille emploi France : portage Rust et hébergement

Date : 2026-11-09. Statut : conception à valider avant tout code.

## 1. Intention

Le prototype actuel (une page HTML/JS qui lit des fichiers `data/*.js` produits par trois scripts Python lancés à la main) doit devenir un **service en ligne**, hébergé sur le serveur de l'auteur, à l'adresse **https://ecostat21.applicationrush.fr**, avec des données qui se mettent à jour **toutes seules**. L'auteur veut une application **Rust** qui cohabite avec ses autres applications Rust déjà en production sur la même machine.

Pourquoi : l'outil sert à garder « une vision très claire » de ce qui se passe sur le marché de l'emploi face à l'IA (l'auteur travaille dans un métier exposé). Un outil qu'il faut actualiser à la main finit par ne plus l'être ; un service public propre peut aussi servir à d'autres.

## 2. Objectifs, non-objectifs, anti-objectifs

**Objectifs**
- Reproduire à l'identique ce que fait le prototype : 4 thèmes (Emploi, Exposition à l'IA, Tech, Mon suivi), les vues, la période réglable, l'interrupteur de niveau de détail, la superposition macro, le tableau de bord tech, l'écran « Sources, limites et licences ».
- Collecte automatique : Indeed, Eurostat et DARES chaque jour ; France Travail (métiers ROME, tech) de façon à rester à jour au rythme trimestriel de la source.
- Un seul binaire Rust à déployer, supervisé par `systemd`, derrière le serveur web déjà en place.
- Les données exposées par le service gardent **le même format que les fichiers actuels** (`window.DATA`, `window.ROME`, `window.TECH`, `window.AI`, `window.AIROME`), pour que l'interface JavaScript actuelle fonctionne sans réécriture.

**Non-objectifs** (décidés ou assumés)
- **Interface en Rust/WebAssembly : non.** Décision de l'auteur : « hybride Rust + JS ». L'interface reste en JavaScript avec Chart.js.
- Comptes utilisateurs, base de données, écriture par les visiteurs : non (fichiers de données seulement).
- Application mobile native : non (la page est déjà adaptée au téléphone).

**Anti-objectifs** (échec même si « ça marche »)
- Publier ou journaliser le **secret France Travail** (ni dans le dépôt, ni dans les logs, ni dans les réponses HTTP).
- **Perturber les autres applications du serveur** : disque presque plein à cause d'une compilation, port déjà pris, rechargement du serveur web en erreur qui coupe d'autres sites.
- Afficher des données **périmées ou cassées sans le dire** (une collecte qui échoue ne doit ni vider le site ni mentir sur la date).
- Exposer le port interne du service directement sur Internet.

## 3. Contraintes

- **Serveur** : Ubuntu 22.04, ~20 Go de disque dont environ 9 Go libres, mémoire peu utilisée. Le serveur web est **nginx** (reverse proxy, HTTPS sur 443). Les autres applications tournent comme **services `systemd`** ; **Docker**, PostgreSQL et Redis sont aussi présents mais ne servent pas ici. La chaîne Rust (`cargo`, `rustc`) est installée pour l'utilisateur `ubuntu`. Plusieurs ports locaux sont déjà utilisés : le port du nouveau service (3210 par défaut) sera vérifié libre au déploiement.
- **DNS** : l'enregistrement `ecostat21` (type A) pointe déjà vers le serveur.
- **API France Travail** : application « Ecostat21 », API « Marché du travail v1 » autorisée, limite de **10 appels/seconde** ; droits `api_stats-offres-demandes-emploiv1 offresetdemandesemploi`. Le jeton dure environ 25 minutes. L'identifiant et le secret viennent de variables d'environnement.
- **Licences** : Indeed Hiring Lab (CC BY 4.0, citer), DARES (Licence Ouverte), Eurostat (mention). **API France Travail « Marché du travail » : accès libre sous licence Etalab (Licence Ouverte)**, vérifié le 2026-11-09 sur la page de l'API et dans les CGU de francetravail.io (article 5.2 a) : reproduction, modification et usage commercial autorisés, avec mention de la source (voir section 8). L'API « Offres d'emploi », elle, relève d'une autre licence et est actuellement suspendue : **on ne l'utilise pas**.
- L'auteur garde **la même clé API** qu'aujourd'hui, y compris en production (décision explicite, ne pas la remettre en cause).

## 4. Ce qui est décidé par l'auteur

- Rust pour le serveur et la collecte ; JavaScript pour l'interface.
- Hébergement chez lui, sur `https://ecostat21.applicationrush.fr`.
- Le prototype Python/HTML reste la référence tant que la version Rust n'est pas validée.
- Même clé France Travail.

## 5. Architecture proposée (choix du concepteur, à contester)

**Un crate Rust, un binaire**, `veille-emploi`, avec :

- **Serveur HTTP (`axum`, `tokio`)** : sert `index.html`, `vendor/chart.umd.min.js` et les fichiers de données. Les ressources statiques de l'interface sont **embarquées dans le binaire** (pas de dossier à synchroniser). Les fichiers de données sont lus depuis un répertoire configurable (`DATA_DIR`) pour pouvoir être mis à jour sans redéployer.
- **Routes** : `GET /` (page), `GET /data/<nom>.js` (les 5 fichiers de données actuels, mêmes noms et même format), `GET /health` (JSON : date de dernière réussite de chaque collecte, âge des données, version). Aucune route d'écriture.
- **Collecteurs** (portage des trois scripts Python) : `indeed`, `eurostat`, `dares`, `france_travail` (offres ROME puis indicateurs tech). Un module de planification dans le binaire lance chaque collecteur à son rythme ; une option `--once <collecteur>` permet de les lancer à la main (tests et dépannage).
- **Écriture atomique** : chaque fichier est écrit sous un nom temporaire puis renommé. Si une collecte échoue, **le dernier fichier valide reste servi** et l'échec est visible dans `/health` et les logs.
- **Rythmes** : Indeed, Eurostat, DARES une fois par jour (la nuit). France Travail : une vérification hebdomadaire, avec rafraîchissement complet (environ 1 900 appels, quelques minutes à faible concurrence, bien sous 10 appels/s) quand la source a publié un nouveau trimestre ou au plus une fois par semaine.
- **Fichiers fournis avec l'application** (non collectés) : `ai_exposure.js` et `ai_rome.js` (estimations d'exposition à l'IA, modifiables à la main).
- **Logique d'interface inchangée** : le lissage des pics, le cumul sur 4 trimestres, les classements et les scores restent dans le JavaScript actuel. Ils pourront migrer côté serveur plus tard si nécessaire.
- **Dépendances prévues** : `reqwest` avec `rustls` (pas besoin des bibliothèques OpenSSL de développement), `serde`/`serde_json`, `tracing`, `rust-embed` ou équivalent pour embarquer l'interface.

## 6. Sécurité

- Le service écoute sur **127.0.0.1 uniquement** ; nginx termine HTTPS et transmet. Aucun port public nouveau.
- Secrets : `FT_CLIENT_ID` et `FT_CLIENT_SECRET` lus dans l'environnement du service (fichier d'environnement du serveur, lisible par le seul compte du service). **Jamais** dans le dépôt, les logs, `/health` ou les messages d'erreur (les erreurs d'authentification sont journalisées sans le secret).
- Service `systemd` sous un utilisateur dédié sans droits, avec durcissement standard (`NoNewPrivileges`, `ProtectSystem=strict`, écriture limitée au répertoire de données).
- En-têtes HTTP de sécurité (CSP limitée à `self`, puisque Chart.js est embarqué ; pas de CORS ouvert). Aucune route n'accepte de données du visiteur.
- Le fichier `.env` local reste ignoré par git ; le dépôt est public.

## 7. Déploiement (choix du concepteur)

- **Compilation sur le serveur** en mode `--release`, avec un nombre de tâches limité et `nice`, **après contrôle de l'espace disque** (arrêt si moins de 3 Go libres) ; nettoyage du dossier de compilation si besoin. Alternative plus tard : compiler ailleurs (GitHub Actions) et envoyer le binaire.
- Un service `systemd` dédié, redémarrage automatique en cas d'échec.
- Un bloc nginx pour `ecostat21.applicationrush.fr` qui transmet vers le port local ; test de la configuration (`nginx -t`) **avant** tout rechargement, pour ne pas affecter les autres sites.
- Certificat HTTPS gratuit par la méthode déjà utilisée pour les autres sites du serveur (à confirmer sur place : certbot ou certificat générique).
- Mise en ligne en deux temps : d'abord vérifier le service en local sur le serveur (`curl` sur le port interne), puis exposer via nginx.

## 8. Points à traiter avant la mise en ligne publique

- **Réutilisation des données de l'API France Travail : réglée.** L'API « Marché du travail » est en accès libre sous licence Etalab : le site peut les afficher publiquement. Obligations à respecter : **mentionner la source** (« France Travail, API Marché du travail, Licence Ouverte Etalab ») **et la date de mise à jour** des données sur le site, ne pas laisser croire à un soutien officiel. Les CGU évoquent aussi le maintien ouvert des bases redistribuées : les données traitées servies par le site restent donc librement accessibles (elles le sont déjà via les fichiers `/data/*.js`). Revérifier si France Travail modifie ses CGU.
- Le pied de page du site doit afficher la date de dernière collecte de chaque source (disponible dans `/health`).
- Attribution des sources : l'écran « Sources, limites et licences » existe déjà ; il doit rester accessible et à jour.
- Le serveur indique « redémarrage système requis » : pas bloquant, mais un redémarrage coupera brièvement les services (le nôtre reprendra seul).

## 9. Tests

**Exigence de l'auteur : des tests unitaires sur tout le code Rust.** Chaque module a ses tests dans le même crate (`cargo test` doit passer en entier avant tout déploiement, et un build qui échoue aux tests n'est pas déployé). Le développement suit l'ordre test d'abord (écrire le test qui échoue, puis le code).

| Module | Ce que les tests unitaires vérifient |
|---|---|
| Analyse Indeed (CSV) | colonnes attendues, filtre « total postings », 34 catégories, dates alignées, ligne mal formée rejetée |
| Analyse Eurostat (JSON-stat) | correspondance position/date, valeurs manquantes ignorées, conversion trimestre/mois en date |
| Analyse DARES (CSV avec BOM, séparateur `;`) | filtres France métropolitaine, taux vide ignoré, trimestres `AAAA-Tn` |
| Client France Travail | obtention et renouvellement du jeton, reprise sur 429/5xx, **le secret n'apparaît jamais dans un message d'erreur ni un log**, limitation à 10 appels/s |
| Collecteurs ROME et tech | périmètre des périodes, valeurs masquées, séries vides, un métier en échec ne bloque pas les autres |
| Écriture des fichiers de données | écriture atomique, ancien fichier conservé si l'écriture échoue, format `window.X=…;` identique au prototype |
| Planificateur | déclenchement aux bons moments (horloge simulée), pas de lancement en double, échec enregistré dans `/health` |
| Routes HTTP | `/`, `/data/*.js` (types de contenu, en-têtes de sécurité), `/health`, 404 sur tout le reste, aucune route d'écriture |
| Configuration | variables d'environnement manquantes ou invalides refusées au démarrage avec un message clair, sans afficher le secret |

Les tests utilisent des **extraits figés des vraies réponses** (dossier `tests/fixtures/`), jamais le réseau. Ils sont complétés par :

- **Parité avec le prototype** : pour chaque collecteur Rust, comparer sa sortie aux fichiers produits par les scripts Python (mêmes séries, mêmes valeurs) avant d'abandonner Python.
- Tests unitaires des analyses de réponses (Indeed CSV, Eurostat JSON-stat, DARES CSV, API France Travail) avec des extraits figés.
- Test de non-régression de l'interface : le parcours automatisé de toutes les combinaisons (vues, détails, indicateurs, périodes) déjà utilisé, exécuté contre le service Rust.
- Test de panne : couper le réseau ou fausser le secret doit laisser le site servir les dernières données valides et signaler l'échec dans `/health`.

## 10. Laissé au constructeur

Organisation interne des modules, choix précis des bibliothèques, détails de la planification (une tâche `tokio` interne ou `systemd timer`), format exact des logs, mise en place de l'intégration continue. Le dépôt reste le même (public, MIT) ; les scripts Python restent jusqu'à validation de la parité, puis sont retirés ou conservés comme outils de référence.

## 11. Décisions complémentaires (choix du concepteur, à contester)

Suite à la relecture indépendante du document :

1. **Mise en ligne** : pas de protection par mot de passe nécessaire, la licence Etalab de l'API étant vérifiée (section 8). Le site peut être public dès que la version Rust est validée.
2. **Politique de sécurité du contenu (CSP)** : le prototype contient du JavaScript et du CSS écrits directement dans `index.html`. On les **extrait dans deux fichiers statiques** (`app.js`, `style.css`) **sans changer leur logique**, ce qui permet `script-src 'self'`. Les styles écrits dans des attributs et ceux posés par Chart.js obligent à garder `style-src 'self' 'unsafe-inline'` (risque faible).
3. **Premier démarrage** : `data/data.js` (déjà dans le dépôt) amorce la collecte quotidienne. Les fichiers `rome.js` et `tech.js` sont amorcés au premier déploiement par copie des fichiers locaux, puis la collecte Rust prend le relais ; la licence Etalab permet aussi de les versionner (décision de l'auteur, voir ci-dessous). `ai_exposure.js` et `ai_rome.js` vivent dans `DATA_DIR` (modifiables à la main), copiés depuis des valeurs par défaut embarquées s'ils manquent, **jamais écrasés ensuite**.
4. **Parité** : on compare Python et Rust sur **les mêmes extraits figés** de réponses (`tests/fixtures/`), ce qui rend la comparaison déterministe ; un diff en direct avec tolérance sert de contrôle ponctuel avant la mise en ligne.
5. **Source de vérité** : en production, **le serveur est le seul à écrire `DATA_DIR`**. Le prototype local garde son propre dossier `data/` jusqu'à son retrait.

**Question encore ouverte, qui appartient à l'auteur** : qui exécute les opérations sur le serveur (compilation, `systemd`, `nginx`, certificat) ? Le livrable est soit un dépôt avec un guide pas à pas que l'auteur exécute lui-même, soit un déroulé que l'assistant exécute dans une session SSH ouverte par l'auteur, commande par commande, avec son accord.
