# Guide de déploiement HTTPS : `https://ecostat21.applicationrush.fr`

Ce guide installe la version Rust de « Veille emploi France » sur un serveur Linux (Ubuntu ou Debian) qui héberge déjà d'autres sites derrière nginx. Il est écrit pour être suivi **pas à pas, une commande à la fois**, par quelqu'un qui n'est pas administrateur système.

## Comment lire ce guide

- Chaque commande est dans son propre bloc. Copiez-la, exécutez-la, **lisez le résultat avant de passer à la suivante**.
- Chaque étape indique si elle est **LECTURE SEULE** (ne change rien) ou **MODIFIE LE SYSTÈME**, et ce qu'on doit voir si tout va bien.
- Les commandes s'exécutent sur le serveur, connecté en SSH avec votre compte d'administration, celui qui a `sudo` : dans ce guide on l'appelle **le compte administrateur** (variable `UTILISATEUR_ADMIN` dans les commandes « sur votre PC »). Ne vous connectez pas en `root`. Sauf mention « sur votre PC », les commandes s'exécutent sur le serveur.
- Si un résultat ne ressemble pas au « résultat attendu », **arrêtez-vous** et passez à la section « Dépannage » ; ne continuez pas pour voir.
- Les étapes ne touchent que ce qui est propre à ce site : un utilisateur `veille`, deux dossiers, un service systemd, un fichier nginx, plus une chaîne Rust dédiée installée à côté de celle du serveur. Rien d'existant n'est modifié (la chaîne Rust par défaut et les autres applications ne sont pas touchées).

> **Ce qu'il ne faut pas faire**
>
> - **Ne jamais exposer le port interne** (3210 par défaut) sur Internet : le programme refuse d'ailleurs d'écouter ailleurs que sur `127.0.0.1`. Seul nginx doit être joignable de l'extérieur.
> - **Ne jamais modifier les fichiers nginx des autres sites** : on ajoute un fichier à part, on teste avec `sudo nginx -t`, et seulement ensuite on recharge.
> - **Ne jamais lancer la compilation avec `sudo` ni en `root`** : elle se fait avec le compte administrateur. Seule l'installation du binaire demande `sudo`.
> - **Ne jamais écrire la clé France Travail dans une commande, un fichier du dépôt ou un message** : elle se saisit uniquement dans l'éditeur, sur le serveur (étape 4). Les fichiers `.env` et `/etc/veille-emploi.env` ne doivent **jamais** être commités.
> - **Ne jamais définir `VE_DISABLE_SCHEDULER`** en production : cette variable ne sert qu'aux tests et arrête les collectes.

## Vue d'ensemble

| Élément | Valeur |
|---|---|
| Programme | `/opt/veille-emploi/veille_emploi` (un seul exécutable, interface web incluse) |
| Utilisateur du service | `veille` (compte système, sans shell) |
| Données | `/var/lib/veille-emploi/data/` |
| Secrets et réglages | `/etc/veille-emploi.env` (droits `0600`, propriétaire `root`) |
| Service | `veille-emploi.service` |
| Port interne | `127.0.0.1:3210` (modifiable) |
| Reverse proxy | nginx, fichier `/etc/nginx/sites-available/veille-emploi.conf` |
| Journaux | `journalctl -u veille-emploi` |

Le programme écoute uniquement en local. nginx reçoit les visiteurs en HTTPS et transmet vers le port interne. Le programme ajoute lui-même ses en-têtes de sécurité (CSP, `nosniff`, `no-referrer`, `X-Frame-Options`) et la compression gzip : **nginx ne doit pas les répéter**.

Planning des collectes (heure UTC, gérées par le programme, rien à configurer) : `core` (Indeed, Eurostat, DARES) chaque jour à 02:30 ; France Travail (`rome` puis `tech`) le lundi à 03:00. Si plus de 10 % des codes ROME échouent, rien n'est écrit et l'ancien fichier est conservé. Au démarrage, une collecte **en retard** (fichier de données ancien) est lancée **immédiatement** ; une collecte dont le fichier est **absent** démarre environ 30 secondes après le lancement. Après un échec, seul le job en échec est retenté : `core` toutes les heures ; France Travail après 1 h, puis 6 h, puis 24 h (jamais au-delà du lundi suivant), pour ménager le quota de l'API.

---

## 1. Prérequis et contrôles en lecture seule

Toutes les commandes de cette section sont en **LECTURE SEULE**.

### 1.1 Espace disque

**Ce que ça fait** : affiche l'espace libre sur le disque principal.

```bash
df -h /
```

**Résultat attendu** : la colonne `Avail` indique **au moins 3 Go**. La compilation en demande temporairement environ 2 à 3 Go. **S'il y a moins de 3 Go libres, arrêtez-vous ici** et libérez de la place avant de continuer.

### 1.2 Compilateur Rust

**Principe** : le programme nécessite **rustc 1.88 ou plus récent** (une dépendance l'exige) ; il a été compilé et testé avec la version **1.95.0**. Pour ne rien changer aux autres applications Rust du serveur, on **ne touche pas à la chaîne Rust par défaut** : on installe à côté une chaîne dédiée, 1.95.0, et on ne l'utilise que pour ce programme (`cargo +1.95.0 ...`). La chaîne par défaut du serveur et les autres applications ne sont pas affectées.

**Ce que ça fait** : montre la chaîne Rust par défaut et les chaînes installées (LECTURE SEULE).

```bash
rustup show
```

```bash
rustup toolchain list
```

**Résultat attendu** : la liste des chaînes, dont la chaîne par défaut (`default`). Notez-la, vous ne la modifierez pas. Si la commande est introuvable, chargez l'environnement Rust puis recommencez (cette commande ne vaut que pour la session de terminal en cours : à répéter à chaque nouvelle connexion, sauf si `~/.profile` charge déjà `~/.cargo/env`) :

```bash
. "$HOME/.cargo/env"
```

Si `1.95.0` apparaît déjà dans la liste, passez à l'étape 1.3. Sinon, installez-la (**MODIFIE LE SYSTÈME** : environ 600 Mo sous `~/.rustup`, rien d'autre ; réversible avec `rustup toolchain uninstall 1.95.0`) :

```bash
rustup toolchain install 1.95.0 --profile minimal
```

**Résultat attendu** : `installed - rustc 1.95.0 ...`. Contrôle :

```bash
rustc +1.95.0 --version
```

Résultat : `rustc 1.95.0 ...`. **Ne lancez jamais `rustup default` ni `rustup update`** pour ce guide : ils changeraient la compilation des autres applications.

### 1.3 Outils annexes

**Ce que ça fait** : vérifie que `git` et `cargo` sont présents (version par défaut, à titre d'information).

```bash
git --version && cargo --version
```

**Résultat attendu** : deux lignes de version.

### 1.4 Le DNS pointe-t-il vers ce serveur ?

**Ce que ça fait** : compare l'adresse enregistrée dans le DNS pour le site avec l'adresse publique de ce serveur. Notons-la `<IP_DU_SERVEUR>` dans la suite : c'est seulement un repère dans le texte, vous n'aurez rien à recopier.

```bash
dig +short ecostat21.applicationrush.fr
```

```bash
curl -s https://ifconfig.me
```

**Résultat attendu** : les deux commandes affichent **la même adresse IP**. Si `dig` n'existe pas, utilisez `getent hosts ecostat21.applicationrush.fr`. Si les adresses diffèrent, **arrêtez-vous** : Let's Encrypt refuserait de délivrer le certificat (voir « Dépannage »).

### 1.5 Sites nginx existants

**Ce que ça fait** : liste les sites déjà actifs, pour vérifier qu'aucun ne porte déjà le nom de fichier que nous allons utiliser.

```bash
ls /etc/nginx/sites-enabled
```

**Résultat attendu** : une liste de fichiers ou de liens. Il ne doit pas y avoir de `veille-emploi.conf`. Ne les ouvrez pas, ne les modifiez pas.

**Ce que ça fait** : cherche si le nom du site est déjà utilisé quelque part dans la configuration nginx. Un nom en double n'est **pas** détecté par `nginx -t` : notre bloc serait ignoré en silence, et certbot pourrait modifier le fichier d'un autre site.

```bash
sudo grep -rn "ecostat21" /etc/nginx/
```

**Résultat attendu** : **aucune sortie**. Si une ligne apparaît, **arrêtez-vous** et signalez-le : ne continuez pas.

### 1.6 certbot déjà installé ?

**Ce que ça fait** : cherche l'outil de certificats.

```bash
which certbot
```

**Résultat attendu** : un chemin (par exemple `/usr/bin/certbot` ou `/snap/bin/certbot`) si certbot est installé ; rien (et un code de sortie 1) sinon.

```bash
sudo certbot certificates
```

**Résultat attendu** (si certbot est installé) : la liste des certificats existants avec leurs `Domains`. Retenez le cas pour l'étape 8 : **(A)** aucun certificat ne couvre `ecostat21.applicationrush.fr` ; **(B)** un certificat générique (`*.applicationrush.fr`) ou contenant déjà ce nom existe. Si certbot n'est pas installé, cette commande échoue : c'est normal, passez à la suite.

### 1.7 Le port interne est-il libre ?

**Ce que ça fait** : cherche un programme qui écouterait déjà sur le port 3210.

```bash
ss -tln | grep -F ':3210 '
```

**Résultat attendu** : **aucune sortie** (le port est libre). Si une ligne apparaît, le port est pris : choisissez un autre numéro (par exemple 3211, à re-vérifier de la même façon avec `grep -F ':3211 '`) et **utilisez-le partout** à la place de 3210 : dans `VE_BIND` (étape 4), dans les `curl` locaux et dans `proxy_pass` (étapes 7 et 8).

### 1.8 L'utilisateur `veille` n'existe pas encore

```bash
id veille
```

**Résultat attendu** : `id: 'veille': no such user`. Si l'utilisateur existe déjà, ne le recréez pas : sautez la création à l'étape 2.

### 1.9 La branche est-elle publiée ?

**Ce que ça fait** : vérifie que la branche à installer existe sur le dépôt public.

```bash
git ls-remote --heads https://github.com/clemPerrousset/veille-emploi-france.git rust-port
```

**Résultat attendu** : une ligne se terminant par `refs/heads/rust-port`. Si la sortie est **vide**, **arrêtez-vous** : la branche n'est pas encore publiée.

---

## 2. Utilisateur et dossiers

**MODIFIE LE SYSTÈME** (crée un compte et trois dossiers, rien d'existant n'est touché).

**Ce que ça fait** : crée un compte système `veille`, sans mot de passe, sans shell, qui exécutera le service.

```bash
sudo useradd --system --home-dir /var/lib/veille-emploi --shell /usr/sbin/nologin veille
```

**Résultat attendu** : aucune sortie. Vérification (LECTURE SEULE) :

```bash
id veille
```

Résultat : une ligne `uid=... (veille) gid=... (veille)`.

**Ce que ça fait** : crée le dossier du programme (propriété de `root`, en lecture seule pour le service).

```bash
sudo install -d -o root -g root -m 755 /opt/veille-emploi
```

**Ce que ça fait** : crée le dossier de travail et le dossier des données, propriété de `veille`.

```bash
sudo install -d -o veille -g veille -m 750 /var/lib/veille-emploi /var/lib/veille-emploi/data
```

**Résultat attendu** : aucune sortie. Contrôle (LECTURE SEULE) :

```bash
sudo ls -ld /opt/veille-emploi /var/lib/veille-emploi /var/lib/veille-emploi/data
```

Résultat : trois lignes ; les deux dernières appartiennent à `veille veille`.

> `/var/lib/veille-emploi` sert de répertoire de travail du service. Il ne doit contenir **aucun fichier `.env`** : le programme lit un `.env` du répertoire courant s'il existe, et nous voulons que seul `/etc/veille-emploi.env` fasse foi.

---

## 3. Compilation sur le serveur

La compilation se fait avec le compte administrateur (jamais `root`), dans un clone **complet** du dépôt : l'interface web (`index.html`, `app.js`, `style.css`, `vendor/`) est incorporée au programme au moment de la compilation depuis la racine du dépôt, pas seulement depuis `server/`.

### 3.1 Re-contrôler l'espace disque

```bash
df -h /
```

**Résultat attendu** : au moins 3 Go libres (`Avail`). Sinon, arrêtez-vous.

### 3.2 Cloner la branche

**MODIFIE LE SYSTÈME** (crée un dossier dans votre répertoire personnel).

```bash
git clone --branch rust-port --single-branch https://github.com/clemPerrousset/veille-emploi-france.git "$HOME/veille-emploi-src"
```

**Résultat attendu** : `Cloning into ...` puis la fin du téléchargement sans erreur. (Quand la branche aura été fusionnée dans `main`, remplacez `rust-port` par `main`.)

### 3.3 Compiler

**MODIFIE LE SYSTÈME** (écrit dans `~/veille-emploi-src/server/target`, de l'ordre de 1 à 2 Go). La compilation est limitée à 2 tâches et à basse priorité pour ne pas gêner les autres applications ; elle peut durer plusieurs minutes.

La commande utilise la chaîne dédiée `+1.95.0` (étape 1.2), jamais la chaîne par défaut. Si la commande `cargo` est introuvable, rechargez l'environnement avec `. "$HOME/.cargo/env"` (à refaire à chaque nouvelle session).

```bash
cd "$HOME/veille-emploi-src/server" && CARGO_BUILD_JOBS=2 nice cargo +1.95.0 build --release --locked
```

**Résultat attendu** : se termine par ``Finished `release` profile [optimized] target(s) in ...``. Si l'erreur mentionne une version de `rustc` trop ancienne, voir « Dépannage ».

### 3.4 Installer le binaire

**MODIFIE LE SYSTÈME** (écrit un fichier dans `/opt/veille-emploi`).

```bash
sudo install -o root -g root -m 755 "$HOME/veille-emploi-src/server/target/release/veille_emploi" /opt/veille-emploi/veille_emploi
```

Contrôle (LECTURE SEULE) :

```bash
ls -l /opt/veille-emploi/veille_emploi
```

**Résultat attendu** : un fichier exécutable (quelques Mo), propriétaire `root`.

### 3.5 Récupérer le disque

**MODIFIE LE SYSTÈME** (supprime uniquement le dossier de compilation, qu'on peut toujours reconstruire).

```bash
rm -rf "$HOME/veille-emploi-src/server/target"
```

```bash
df -h /
```

**Résultat attendu** : l'espace libre est revenu au niveau de l'étape 3.1. (Le dossier `~/veille-emploi-src` lui-même reste : il sert aux mises à jour.)

---

## 4. Fichier d'environnement (secrets)

Le fichier `/etc/veille-emploi.env` contient la clé France Travail. Il est lu par systemd (en tant que `root`) puis transmis au programme : le compte `veille` n'a pas besoin de pouvoir le lire, et la clé n'apparaît jamais dans une ligne de commande ni dans l'historique du shell.

### 4.1 Créer le fichier vide, avec les bons droits

**MODIFIE LE SYSTÈME.** On crée le fichier **avant** d'y mettre quoi que ce soit, pour qu'il n'existe jamais avec des droits trop larges. **Ne relancez pas cette commande si le fichier existe déjà** : elle le viderait et effacerait la clé. Pour modifier le fichier ensuite, utilisez `sudoedit` (étape 4.2).

```bash
sudo install -m 600 -o root -g root /dev/null /etc/veille-emploi.env
```

**Résultat attendu** : aucune sortie.

### 4.2 Saisir le contenu dans l'éditeur

**MODIFIE LE SYSTÈME.** Ouvrez le fichier avec l'éditeur sécurisé de `sudo` (il travaille sur une copie temporaire et réécrit le fichier à l'enregistrement).

```bash
SUDO_EDITOR=nano sudoedit /etc/veille-emploi.env
```

Dans l'éditeur (`nano` : enregistrer avec Ctrl+O puis Entrée, quitter avec Ctrl+X), tapez **ces quatre lignes**, en remplaçant les deux premières valeurs par celles de votre application sur francetravail.io. Pas d'espace autour du `=`, pas de guillemets.

```text
FT_CLIENT_ID=votre_identifiant_PAR_...
FT_CLIENT_SECRET=votre_cle_secrete
VE_BIND=127.0.0.1:3210
VE_DATA_DIR=/var/lib/veille-emploi/data
```

Si vous avez choisi un autre port à l'étape 1.7, mettez-le dans `VE_BIND`. Le programme **refuse** toute adresse autre que locale (`127.0.0.1` ou `::1`) et s'arrête avec une erreur : c'est voulu.

Sans les deux variables `FT_...`, le programme démarre quand même mais désactive les collectes `rome` et `tech` (un message le signale dans les journaux) ; `core` fonctionne.

### 4.3 Vérifier sans afficher le secret

**LECTURE SEULE.** Ces commandes ne montrent pas la clé.

```bash
sudo ls -l /etc/veille-emploi.env
```

**Résultat attendu** : `-rw------- 1 root root ...` (droits `0600`, propriétaire `root`).

```bash
sudo cut -d= -f1 /etc/veille-emploi.env
```

**Résultat attendu** : seulement les **noms** des variables : `FT_CLIENT_ID`, `FT_CLIENT_SECRET`, `VE_BIND`, `VE_DATA_DIR`.

> Ce fichier, comme le `.env` local de votre PC, ne doit **jamais** être commité ni copié dans le dépôt.

---

## 5. Données initiales

Les fichiers `rome.js` et `tech.js` viennent de l'API France Travail et ne sont pas dans le dépôt. Pour que le site soit complet dès le premier démarrage, copiez ceux que vous avez déjà sur votre PC. (`data.js` n'a pas besoin d'être copié : la tâche `core` le collecte au démarrage. `ai_exposure.js` et `ai_rome.js` sont créés au premier démarrage à partir des valeurs par défaut du programme, sans jamais écraser un fichier existant ; vous pouvez ensuite les corriger à la main.)

### 5.1 Envoyer les fichiers vers un dossier temporaire du serveur (sur votre PC)

**Sur votre PC, dans PowerShell.** Ces commandes vous demandent l'adresse du serveur (`<IP_DU_SERVEUR>`, la même qu'à l'étape 1.4), le nom du compte administrateur (celui avec lequel vous vous connectez en SSH) et le chemin du dépôt local sur votre PC (`<chemin_du_dépôt_local>`, le dossier qui contient `data\rome.js` et `data\tech.js`) ; rien n'est modifié. **Faites cette étape et la suivante (`scp`) dans la MÊME fenêtre PowerShell** : les variables n'existent que dans la session où elles ont été définies.

```powershell
$IP_SERVEUR = Read-Host "Adresse du serveur"; $UTILISATEUR_ADMIN = Read-Host "Compte administrateur (SSH)"; $DEPOT_LOCAL = Read-Host "Chemin du dépôt local"
```

**MODIFIE LE SYSTÈME** (écrit deux fichiers dans `/tmp` du serveur). L'option `-p` conserve la date de modification d'origine : le programme s'en sert pour savoir si les données sont à jour.

```powershell
scp -p "$DEPOT_LOCAL\data\rome.js" "$DEPOT_LOCAL\data\tech.js" "${UTILISATEUR_ADMIN}@${IP_SERVEUR}:/tmp/"
```

**Résultat attendu** : une ligne par fichier avec `100%`. (Le premier accès peut demander de confirmer l'empreinte du serveur : répondez `yes` si c'est bien votre serveur.)

### 5.2 Installer les fichiers dans le dossier de données (sur le serveur)

**MODIFIE LE SYSTÈME.**

```bash
sudo install -p -o veille -g veille -m 640 /tmp/rome.js /tmp/tech.js /var/lib/veille-emploi/data/
```

**Résultat attendu** : aucune sortie. Supprimez ensuite les copies temporaires :

```bash
rm /tmp/rome.js /tmp/tech.js
```

Contrôle (LECTURE SEULE) :

```bash
sudo ls -l /var/lib/veille-emploi/data
```

**Résultat attendu** : `rome.js` et `tech.js`, propriétaire `veille veille`, droits `-rw-r-----`.

> Comme les dates d'origine sont conservées (`scp -p`), si ces fichiers sont déjà anciens le programme les juge en retard : la collecte France Travail démarre **dès le lancement du service** (étape 6), sans attendre, et dure environ 10 minutes pour `rome`. C'est le comportement normal. (Si les fichiers sont absents, la collecte démarre environ 30 secondes après le lancement.)

---

## 6. Service systemd

### 6.1 Créer le fichier de service

**MODIFIE LE SYSTÈME** (crée un fichier dans `/etc/systemd/system`). Le bloc est en plusieurs lignes : copiez-le en entier. **Ne relancez pas cette commande si le fichier existe déjà** (elle l'écraserait) : pour le modifier ensuite, utilisez `SUDO_EDITOR=nano sudoedit /etc/systemd/system/veille-emploi.service`.

```bash
sudo tee /etc/systemd/system/veille-emploi.service > /dev/null <<'EOF'
[Unit]
Description=Veille emploi France (serveur Rust)
After=network-online.target
Wants=network-online.target
StartLimitIntervalSec=300
StartLimitBurst=5

[Service]
Type=simple
User=veille
Group=veille
EnvironmentFile=/etc/veille-emploi.env
WorkingDirectory=/var/lib/veille-emploi
ExecStart=/opt/veille-emploi/veille_emploi serve
Restart=on-failure
RestartSec=5

# Durcissement
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/var/lib/veille-emploi/data
ProtectKernelTunables=true
RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6
CapabilityBoundingSet=
LockPersonality=true

[Install]
WantedBy=multi-user.target
EOF
```

**Résultat attendu** : aucune sortie. Contrôle de syntaxe (LECTURE SEULE) :

```bash
systemd-analyze verify /etc/systemd/system/veille-emploi.service
```

**Résultat attendu** : aucune sortie, ou au plus des avertissements sans gravité. Une erreur nommant une ligne du fichier est à corriger avant de continuer.

### 6.2 Activer et démarrer

**MODIFIE LE SYSTÈME.**

```bash
sudo systemctl daemon-reload
```

```bash
sudo systemctl enable --now veille-emploi
```

**Résultat attendu** : `Created symlink ... veille-emploi.service`. Contrôle (LECTURE SEULE) :

```bash
systemctl status veille-emploi --no-pager
```

**Résultat attendu** : `Active: active (running)`. Si le service est en `failed` ou redémarre en boucle, lisez :

```bash
journalctl -u veille-emploi -n 50 --no-pager
```

### 6.3 Test local, avant tout nginx

**LECTURE SEULE.**

```bash
curl -s http://127.0.0.1:3210/health
```

**Résultat attendu** : un petit JSON contenant `version`, `collectors` et `files`. Détails :

- dans `files`, `rome.js`, `tech.js`, `ai_exposure.js` et `ai_rome.js` ont `exists: true` ; `data.js` est à `false` tant que la première collecte `core` n'a pas eu lieu (environ 30 secondes après le démarrage, puis quelques secondes de travail) ;
- `collectors` est **vide** tant qu'aucune collecte réelle n'a tourné dans ce processus : c'est normal juste après le démarrage.

Patientez une minute puis recommencez : `data.js` doit apparaître et `collectors` doit montrer `core` avec un `last_ok`.

```bash
curl -sI http://127.0.0.1:3210/
```

**Résultat attendu** : `HTTP/1.1 200 OK` avec des en-têtes `content-security-policy`, `x-content-type-options`, etc. (posés par le programme).

---

## 7. nginx : d'abord le port 80

On ajoute un **nouveau fichier** : aucun fichier existant n'est ouvert.

### 7.1 Créer le bloc `server`

**MODIFIE LE SYSTÈME** (crée un fichier dans `sites-available`, rien n'est encore actif). Remplacez `3210` par votre port si vous en avez choisi un autre (avec `sudoedit` après coup). Les guillemets autour de `EOF` empêchent le shell de toucher aux `$` de la configuration. **Ne relancez pas cette commande si le fichier existe déjà, surtout après certbot** : elle effacerait le bloc HTTPS que certbot y a ajouté. Pour modifier ce fichier ensuite, utilisez `SUDO_EDITOR=nano sudoedit /etc/nginx/sites-available/veille-emploi.conf`.

```bash
sudo tee /etc/nginx/sites-available/veille-emploi.conf > /dev/null <<'EOF'
server {
    listen 80;
    listen [::]:80;
    server_name ecostat21.applicationrush.fr;

    location / {
        limit_except GET HEAD { deny all; }

        proxy_pass http://127.0.0.1:3210;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
EOF
```

Il n'y a volontairement **aucun `add_header`** ni compression côté nginx : le programme s'en charge.

Si `nginx -t` (étape 7.3) se plaint de `[::]` ou de « Address family not supported » (serveur sans IPv6), supprimez les deux lignes `listen [::]:...` du fichier (avec `sudoedit`) et retestez.

### 7.2 L'activer

**MODIFIE LE SYSTÈME** (crée un lien dans `sites-enabled` ; nginx n'en tient pas compte tant qu'on ne le recharge pas).

```bash
sudo ln -s /etc/nginx/sites-available/veille-emploi.conf /etc/nginx/sites-enabled/veille-emploi.conf
```

### 7.3 Tester la configuration, AVANT tout rechargement

**LECTURE SEULE.**

```bash
sudo nginx -t
```

**Résultat attendu** : `syntax is ok` et `test is successful`, **sans aucun avertissement** `[warn]`.

**Un avertissement `conflicting server name "ecostat21.applicationrush.fr" ... ignored` est à traiter comme une ERREUR** : notre bloc serait ignoré en silence au profit d'un autre site. Dans ce cas, procédez comme ci-dessous (retirer notre lien, retester), puis arrêtez-vous et signalez-le.

**Si `nginx -t` affiche une erreur : NE RECHARGEZ PAS nginx.** Les autres sites continuent de fonctionner tant qu'on ne recharge pas. Retirez simplement notre lien, puis retestez :

```bash
sudo rm /etc/nginx/sites-enabled/veille-emploi.conf
```

```bash
sudo nginx -t
```

Si le test réussit maintenant, l'erreur venait de notre fichier : relisez le bloc de l'étape 7.1 (accolades, points-virgules), puis recommencez à l'étape 7.2. Si le test échoue **encore**, l'erreur ne vient pas de nous : arrêtez-vous et ne touchez à rien d'autre. Ne modifiez jamais les fichiers des autres sites pour « faire passer » le test.

### 7.4 Recharger nginx

**MODIFIE LE SYSTÈME** (un rechargement ne coupe pas les connexions en cours). À ne faire que si `nginx -t` vient de réussir.

```bash
sudo systemctl reload nginx
```

### 7.5 Vérifier en HTTP

**LECTURE SEULE.**

```bash
curl -sI http://ecostat21.applicationrush.fr/
```

**Résultat attendu** : `HTTP/1.1 200 OK` (même réponse qu'en local). Le site fonctionne en HTTP ; l'étape suivante ajoute HTTPS.

---

## 8. HTTPS avec Let's Encrypt

Deux situations sont possibles selon ce que vous avez vu à l'étape 1.6.

### Cas A : aucun certificat ne couvre le site (certbot installé ou à installer)

Si `which certbot` n'avait rien affiché, installez-le d'abord (**MODIFIE LE SYSTÈME**). Mettez d'abord à jour la liste des paquets :

```bash
sudo apt update
```

```bash
sudo apt install certbot python3-certbot-nginx
```

**Lisez la liste de confirmation avant de répondre** : elle ne doit installer que certbot et ses dépendances. Si elle met à jour des paquets sans rapport (nginx, openssl...), répondez `n` pour annuler et signalez-le.

Que certbot soit déjà installé ou non, vérifiez (LECTURE SEULE) que son module nginx est disponible :

```bash
sudo certbot plugins
```

**Résultat attendu** : la liste contient `* nginx`. Si ce n'est pas le cas (certbot installé par snap ou par apt sans le module), **arrêtez-vous** et signalez-le plutôt que de modifier la configuration à la main.

Demandez ensuite le certificat (**MODIFIE LE SYSTÈME** : certbot modifie **notre** fichier `veille-emploi.conf`, ajoute le bloc HTTPS et la redirection, puis recharge nginx après l'avoir testé). Il vous demandera une adresse e-mail et l'acceptation des conditions. L'option `-d` limite l'opération à ce nom : les autres sites ne sont pas concernés.

```bash
sudo certbot --nginx -d ecostat21.applicationrush.fr --redirect
```

**Résultat attendu** : `Successfully received certificate` puis `Successfully deployed certificate`, avec le chemin du certificat sous `/etc/letsencrypt/live/`. L'option `--redirect` ajoute la redirection du port 80 vers 443 (code 301).

### Cas B : un certificat générique ou existant couvre déjà le nom

Si `sudo certbot certificates` liste un certificat dont les `Domains` incluent `*.applicationrush.fr` (ou `ecostat21.applicationrush.fr`), **ne demandez pas un nouveau certificat** : réutilisez-le. Notez le nom du dossier correspondant sous `/etc/letsencrypt/live/` (il apparaît dans `Certificate Path` et `Private Key Path`).

Ouvrez notre fichier (et seulement lui) :

```bash
SUDO_EDITOR=nano sudoedit /etc/nginx/sites-available/veille-emploi.conf
```

Remplacez tout son contenu par le texte suivant, en mettant à la place de `NOM_DU_CERTIFICAT` le nom réel du dossier (ou les chemins exacts indiqués par certbot). Gardez `3210` ou votre port. (Sur les versions de nginx anciennes, par exemple celle d'Ubuntu 22.04, `http2` s'écrit sur la ligne `listen`.)

```text
server {
    listen 80;
    listen [::]:80;
    server_name ecostat21.applicationrush.fr;
    return 301 https://$host$request_uri;
}

server {
    listen 443 ssl http2;
    listen [::]:443 ssl http2;
    server_name ecostat21.applicationrush.fr;

    ssl_certificate     /etc/letsencrypt/live/NOM_DU_CERTIFICAT/fullchain.pem;
    ssl_certificate_key /etc/letsencrypt/live/NOM_DU_CERTIFICAT/privkey.pem;
    include /etc/letsencrypt/options-ssl-nginx.conf;
    ssl_dhparam /etc/letsencrypt/ssl-dhparams.pem;

    location / {
        limit_except GET HEAD { deny all; }

        proxy_pass http://127.0.0.1:3210;
        proxy_http_version 1.1;
        proxy_set_header Host $host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        proxy_set_header X-Forwarded-Proto $scheme;
    }
}
```

Les deux lignes `include .../options-ssl-nginx.conf` et `ssl_dhparam ...` reprennent les réglages TLS de certbot (même configuration que dans le cas A) ; elles ne s'appliquent que si ces fichiers existent. Vérifiez-le (LECTURE SEULE) ; si l'un manque, supprimez la ligne correspondante :

```bash
sudo ls /etc/letsencrypt/options-ssl-nginx.conf /etc/letsencrypt/ssl-dhparams.pem
```

Testez, **et rechargez seulement si le test réussit** :

```bash
sudo nginx -t
```

```bash
sudo systemctl reload nginx
```

Si le test échoue, appliquez la consigne de l'étape 7.3 : ne rechargez pas, remettez dans le fichier le contenu de l'étape 7.1 (avec `sudoedit`) et retestez.

### 8.1 Renouvellement automatique (cas A)

**SIMULATION** : teste le renouvellement **de notre certificat seulement**, sans rien changer de définitif.

```bash
sudo certbot renew --dry-run --cert-name ecostat21.applicationrush.fr
```

**Résultat attendu** : `Congratulations, all simulated renewals succeeded`. **Ne lancez jamais `certbot renew` sans `--cert-name` sur ce serveur** : la commande testerait tous les certificats et exécuterait les commandes avant/après renouvellement des autres sites (arrêt ou redémarrage possibles). Dans le cas B, le renouvellement du certificat existant est déjà géré par la méthode en place : rien à faire ici (et ne lancez pas cette simulation, le certificat n'a pas ce nom).

### 8.2 Vérifier HTTPS

**LECTURE SEULE.**

```bash
curl -sI https://ecostat21.applicationrush.fr
```

**Résultat attendu** : `HTTP/2 200` (ou `HTTP/1.1 200`) et les en-têtes de sécurité du programme (`content-security-policy`, `x-content-type-options`...), **sans** doublons.

```bash
curl -sI http://ecostat21.applicationrush.fr
```

**Résultat attendu** : `HTTP/1.1 301 Moved Permanently` avec `Location: https://ecostat21.applicationrush.fr/`.

Contrôle du certificat (émetteur, dates, nom) :

```bash
echo | openssl s_client -connect ecostat21.applicationrush.fr:443 -servername ecostat21.applicationrush.fr 2>/dev/null | openssl x509 -noout -subject -issuer -dates
```

**Résultat attendu** : `issuer=... Let's Encrypt ...` et `notAfter` dans environ 90 jours (ou la date du certificat existant).

### 8.3 HSTS (facultatif, seulement après que tout fonctionne)

HSTS force les navigateurs à n'utiliser que HTTPS pour ce site. Une erreur est difficile à rattraper, car les navigateurs retiennent la consigne. **N'activez-le qu'après avoir validé l'étape 9**, avec une durée très courte au début. Le programme n'envoie pas cet en-tête : l'ajouter dans nginx ne crée pas de doublon.

Ouvrez notre fichier :

```bash
SUDO_EDITOR=nano sudoedit /etc/nginx/sites-available/veille-emploi.conf
```

Dans le bloc `server` qui contient `listen 443`, ajoutez cette ligne (5 minutes) :

```text
add_header Strict-Transport-Security "max-age=300" always;
```

**N'ajoutez pas `includeSubDomains`** : cela concernerait aussi les autres sites du domaine. Testez, puis rechargez seulement si le test réussit :

```bash
sudo nginx -t
```

```bash
sudo systemctl reload nginx
```

Vérification :

```bash
curl -sI https://ecostat21.applicationrush.fr | grep -i strict-transport
```

**Résultat attendu** : `strict-transport-security: max-age=300`. Si, après quelques jours, tout va bien, vous pourrez passer à `max-age=86400` (un jour), puis plus tard à une durée plus longue, en répétant la même procédure.

---

## 9. Vérifications finales

**LECTURE SEULE.**

```bash
curl -s https://ecostat21.applicationrush.fr/health
```

**Résultat attendu** : le JSON de l'étape 6.3 ; `collectors` contient désormais `core` avec un `last_ok` récent, et tous les fichiers de `files` ont `exists: true`, avec leur `age_seconds`.

Dans un navigateur, ouvrez `https://ecostat21.applicationrush.fr` :

- la page se charge avec le cadenas (certificat valide) ;
- la page se charge **sans erreur dans la console du navigateur** (touche F12, onglet Console) ;
- les **quatre thèmes** (Emploi, Exposition à l'IA, Tech, Mon suivi) fonctionnent : chacun affiche ses graphiques ;
- le **pied de page** affiche les dates de collecte de chaque source ; ce sont les dates `updated` propres aux fichiers de données (`/health` indique, lui, les collectes exécutées depuis le démarrage du programme et l'âge des fichiers : les deux peuvent légitimement différer) ;
- l'écran « Sources, limites et licences » est accessible.

(Le contrôle automatique des 133 combinaisons de l'interface est un test de développement, décrit dans le README, avec `tests/ui/matrix.js` : il n'est pas à faire sur le serveur.)

Si la console du navigateur (touche F12) signale un blocage « Content Security Policy », c'est qu'un en-tête a été ajouté ou répété côté nginx : retirez tout `add_header` autre que HSTS.

---

## 10. Exploitation

### Journaux

**LECTURE SEULE.** Suivre en direct (Ctrl+C pour quitter) :

```bash
journalctl -u veille-emploi -f
```

Les 100 dernières lignes :

```bash
journalctl -u veille-emploi -n 100 --no-pager
```

Les journaux ne contiennent jamais la clé France Travail.

État du service :

```bash
systemctl status veille-emploi --no-pager
```

### Mise à jour

À répéter à chaque nouvelle version du code. **MODIFIE LE SYSTÈME** (sauf les contrôles).

1. Contrôler le disque (au moins 3 Go libres) :

```bash
df -h /
```

2. Récupérer le nouveau code (mise à jour sans conflit uniquement ; si la commande refuse, ne forcez pas) :

```bash
git -C "$HOME/veille-emploi-src" pull --ff-only
```

3. Recompiler avec la chaîne dédiée (comme à l'étape 3.3 ; plusieurs minutes ; résultat attendu : ``Finished `release` profile [optimized] target(s) in ...``) :

```bash
cd "$HOME/veille-emploi-src/server" && CARGO_BUILD_JOBS=2 nice cargo +1.95.0 build --release --locked
```

4. **Garder l'ancien binaire** pour pouvoir revenir en arrière :

```bash
sudo cp -p /opt/veille-emploi/veille_emploi /opt/veille-emploi/veille_emploi.prev
```

5. Installer le nouveau binaire :

```bash
sudo install -o root -g root -m 755 "$HOME/veille-emploi-src/server/target/release/veille_emploi" /opt/veille-emploi/veille_emploi
```

6. Redémarrer :

```bash
sudo systemctl restart veille-emploi
```

7. Vérifier (`version` et `files` doivent être cohérents) :

```bash
curl -s http://127.0.0.1:3210/health
```

8. Récupérer le disque :

```bash
rm -rf "$HOME/veille-emploi-src/server/target"
```

Les données et les fichiers `ai_*.js` corrigés à la main ne sont jamais écrasés par une mise à jour.

### Retour arrière

Si la nouvelle version ne se comporte pas bien, remettez l'ancien binaire (**MODIFIE LE SYSTÈME**) :

```bash
sudo install -o root -g root -m 755 /opt/veille-emploi/veille_emploi.prev /opt/veille-emploi/veille_emploi
```

```bash
sudo systemctl restart veille-emploi
```

```bash
curl -s http://127.0.0.1:3210/health
```

### Forcer une collecte

Le programme collecte seul selon son planning ; ceci sert à relancer à la main. La commande `once` affiche **une seule ligne de résultat en français** (les détails vont dans les journaux) et se termine par le code 0 (succès) ou 1 (échec). Un nom de collecte inconnu affiche l'aide et se termine par 2.

La méthode ci-dessous fait lire le fichier d'environnement **par systemd** : la clé n'apparaît ni dans `ps`, ni dans l'historique du shell, ni à l'écran. Ne la lancez pas pendant qu'une collecte planifiée est en cours : évitez de 02:30 à 02:45 UTC chaque jour, et le lundi de 03:00 à 03:30 UTC ; ou vérifiez que `journalctl -u veille-emploi -n 20 --no-pager` ne montre aucun `collecte : début` sans `réussie` ou `échec` correspondant.

La collecte `core` (aucune clé requise) :

```bash
sudo systemd-run --pipe --wait --collect --uid=veille --gid=veille --property=EnvironmentFile=/etc/veille-emploi.env --property=WorkingDirectory=/var/lib/veille-emploi /opt/veille-emploi/veille_emploi once core
```

**Résultat attendu** : une ligne de résultat. Pour France Travail (clé requise ; `rome` dure environ 10 minutes, `tech` moins d'une minute), remplacez `core` par `rome` ou `tech` en fin de commande.

Si `systemd-run` refuse l'une des propriétés, la collecte `core`, qui n'a pas besoin de clé, peut se lancer ainsi :

```bash
sudo -u veille env -C /var/lib/veille-emploi VE_DATA_DIR=/var/lib/veille-emploi/data /opt/veille-emploi/veille_emploi once core
```

---

## 11. Dépannage

**Le certificat est refusé (certbot échoue).**

- Vérifiez que le DNS pointe vers ce serveur (étape 1.4). Un enregistrement qui pointe ailleurs, ou qui vient d'être changé, fait échouer la validation : attendez et recommencez.
- Vérifiez que le port 80 est joignable depuis Internet (pare-feu, groupe de sécurité du fournisseur) : Let's Encrypt a besoin d'y accéder. Contrôle, depuis un autre réseau que le serveur : `curl -I http://ecostat21.applicationrush.fr`.
- Let's Encrypt limite le nombre de tentatives : après plusieurs échecs, attendez une heure, et testez d'abord avec une simulation, qui ne consomme pas la limite et ne modifie aucun fichier :

```bash
sudo certbot certonly --nginx -d ecostat21.applicationrush.fr --dry-run
```

**nginx répond « 502 Bad Gateway ».** Le service est arrêté, ou le port de `proxy_pass` n'est pas celui de `VE_BIND`.

```bash
systemctl is-active veille-emploi
```

Si ce n'est pas `active`, lisez `journalctl -u veille-emploi -n 50 --no-pager` (cause fréquente : `VE_BIND` refusé car l'adresse n'est pas locale). Sinon, comparez le port affiché par la commande suivante avec celui de `proxy_pass` dans `/etc/nginx/sites-available/veille-emploi.conf` :

```bash
sudo ss -tlnp | grep veille_emploi
```

**Le site s'ouvre mais sans données, ou des vues sont vides.**

```bash
curl -s http://127.0.0.1:3210/health
```

Regardez `files` : un fichier avec `exists: false` explique la vue manquante. `data.js` manquant : la collecte `core` n'a pas encore réussi (voir `journalctl -u veille-emploi -n 100 --no-pager`, ou forcez-la, étape 10). `rome.js` ou `tech.js` manquants : refaites l'étape 5, ou vérifiez que `FT_CLIENT_ID` et `FT_CLIENT_SECRET` figurent bien dans `/etc/veille-emploi.env` (étape 4.3) et que l'API « Marché du travail » est ajoutée à l'application sur francetravail.io. Un `last_error` sous `collectors` donne la cause de la dernière erreur.

**Le service s'arrête définitivement après plusieurs échecs (`start-limit-hit`).** C'est voulu (5 essais en 5 minutes) : lisez la cause dans `journalctl -u veille-emploi -n 50 --no-pager`, corrigez-la, puis `sudo systemctl reset-failed veille-emploi` et `sudo systemctl start veille-emploi`.

**Erreurs réseau ou de résolution de noms dans les journaux (collecte impossible).** Le service autorise `AF_UNIX`, `AF_INET` et `AF_INET6`. Si le problème persiste, vérifiez que `RestrictAddressFamilies` n'a pas été modifié dans `/etc/systemd/system/veille-emploi.service` (avec `sudoedit`), puis :

```bash
sudo systemctl daemon-reload
```

```bash
sudo systemctl restart veille-emploi
```

**Le disque est plein ou presque.** Supprimez le dossier de compilation (`rm -rf "$HOME/veille-emploi-src/server/target"`). Pour voir ce qui occupe la place sans rien supprimer à l'aveugle :

```bash
sudo du -xh --max-depth=1 /var | sort -h | tail
```

Ne supprimez jamais de fichiers appartenant aux autres applications. Pour connaître la place prise par Rust (lecture seule) :

```bash
du -sh ~/.cargo/registry ~/.rustup
```

Ne supprimez pas ces dossiers : ils servent aussi aux autres applications Rust. Si besoin, la chaîne dédiée se retire seule avec `rustup toolchain uninstall 1.95.0` (à refaire ensuite à l'étape 1.2 pour recompiler).

**La compilation échoue en parlant d'une version de `rustc`.** Le compilateur utilisé est trop ancien (il faut 1.88 ou plus) : la commande n'a sans doute pas utilisé `+1.95.0`. Vérifiez avec `rustc +1.95.0 --version` (si la chaîne est absente, refaites l'étape 1.2), puis relancez la compilation avec `cargo +1.95.0`. Ne modifiez pas la chaîne par défaut du serveur.

**La compilation est interrompue (`signal: 9, SIGKILL`).** Mémoire insuffisante. Ne forcez pas : vérifiez la mémoire disponible avec `free -h` et ne recompilez que lorsqu'elle est suffisante (les autres applications ne doivent pas être gênées).

**Après un redémarrage du serveur.** Le service est activé au démarrage (`enable`) et repart seul ; vérifiez avec `systemctl status veille-emploi --no-pager`.
