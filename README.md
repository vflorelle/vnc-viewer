# vnc-viewer

Visionneuse VNC pour Windows (et Linux) dans l'esprit de **RealVNC Viewer**, **sans compte** ni
connexion à un service en ligne : carnet d'adresses, plusieurs sessions en parallèle, chacune dans
sa fenêtre, **chiffrement RSA-AES** compatible avec RealVNC Server (« RA2 », aussi géré par
TigerVNC), presse-papiers partagé (Unicode quand le serveur le permet), touches spéciales
envoyées au serveur. Un seul `.exe` autonome, sans DLL ni installation.

## Compiler

```sh
./build.sh                # -> dist/vnc-viewer.exe  (Windows x64, cross-compilé)
./build.sh linux          # -> dist/vnc-viewer      (Linux)
./build.sh test           # tests
./build.sh cargo clippy   # n'importe quelle commande cargo
```

- **Avec Docker** (utilisé s'il est installé) : rien d'autre à installer. Le `Dockerfile` part
  de `rust:latest` et utilise MinGW ; le cache cargo est dans le volume `vnc-viewer-cargo`.
- **Sans Docker** : il faut [rustup](https://rustup.rs) avec la cible
  `x86_64-pc-windows-gnu`, [zig](https://ziglang.org) et
  [cargo-zigbuild](https://github.com/rust-cross/cargo-zigbuild) dans le `PATH` ; zig sert
  d'éditeur de liens pour Windows.

Les tests comprennent des scénarios complets contre un faux serveur VNC (mot de passe VNC,
RSA-AES 256 bits avec signature à accepter et identifiants mémorisés, Apple, mot de passe refusé,
chiffrement exigé mais absent, ancien serveur RFB 3.3 sans sécurité, mises à jour Raw, Hextile,
ZRLE et Tight, curseur, nom du bureau, changement de taille, presse-papiers classique et étendu,
clavier, souris et boutons étendus) et un passage de toute l'interface sans fenêtre.

`cargo deny check` (avec [cargo-deny](https://github.com/EmbarkStudios/cargo-deny), réglé par
`deny.toml`) vérifie les dépendances : licences compatibles, failles connues (base RustSec),
provenance (crates.io uniquement). Une alerte est écartée, avec sa raison dans `deny.toml` :
l'attaque « Marvin » sur `rsa` 0.9 demande des milliers de déchiffrements avec la même clé,
alors que la clé RSA du client ne sert qu'une fois, pour une seule connexion.

## Intégration continue et versions

Sur GitHub, `.github/workflows/ci.yml` vérifie chaque envoi et chaque demande de fusion (format,
clippy pour Linux et Windows, tests, `cargo deny`), puis compile les deux exécutables avec zig,
téléchargeables dans les artefacts de l'exécution. Pour publier une version, au choix :

- **à la demande** : onglet *Actions*, workflow *CI*, *Run workflow*, en indiquant le numéro
  (`1.2.3`, ou `1.2.3-beta.1` pour une préversion) ; l'étiquette `v1.2.3` est créée sur le
  commit compilé ;
- **par une étiquette** : `git tag v1.2.3 && git push origin v1.2.3` ;
- **depuis GitHub** : *Releases*, *Draft a new release*, avec une nouvelle étiquette `v1.2.3` ;
  à la publication, les exécutables sont compilés puis ajoutés à cette version (quelques
  minutes), qui garde le titre, les notes et la case préversion choisis.

Le numéro donné remplace celui de `Cargo.toml` dans les exécutables (fenêtre *À propos*) ; celui
du dépôt ne sert qu'aux compilations locales. La version GitHub contient
`vnc-viewer-windows-x86_64.exe`, `vnc-viewer-linux-x86_64` (à rendre exécutable : `chmod +x`) et
leurs sommes de contrôle (`SHA256SUMS`) ; créée par la CI, elle a des notes tirées des commits.
L'exécutable Linux ne demande que la glibc 2.17 (2012) : il démarre sur toutes les distributions
actuelles, et même bien plus anciennes.

## Licence

Au choix, [MIT](LICENSE-MIT) ou [Apache 2.0](LICENSE-APACHE), comme l'essentiel de l'écosystème
Rust. Les polices d'egui intégrées (Hack, Ubuntu, Noto Emoji…) gardent leurs licences libres
(OFL, Ubuntu Font Licence…).

## Utilisation

- **Se connecter** : taper l'adresse du serveur dans la barre du haut, puis `Entrée` (ou
  *Se connecter*). Syntaxe habituelle des visionneuses : `hôte` (port 5900), `hôte:1` (écran 1,
  port 5901 ; un nombre de 100 ou plus est pris comme un port), `hôte::5999` (port), `[::1]:0`
  (IPv6). Une adresse nouvelle est ajoutée au carnet ; la même barre filtre le carnet (par nom
  ou adresse), et `Entrée` ouvre la seule entrée trouvée.
- **Carnet d'adresses** : double-clic ou `Entrée` pour se connecter ; clic droit : se connecter,
  propriétés, dupliquer, supprimer (`Suppr`, avec confirmation) ; `Ctrl+N` : nouvelle connexion.
  Une entrée déjà ouverte (pastille verte) n'est pas rouverte : sa fenêtre passe au premier plan.
- **Propriétés** d'une entrée : adresse, nom, nom d'utilisateur, mot de passe (vide : demandé à
  chaque connexion), signature du serveur acceptée (bouton *Oublier*), chiffrement, qualité de
  l'image, taille, lecture seule, partage du serveur, touches spéciales, de volume et
  multimédia, presse-papiers, plein écran et barre d'outils à l'ouverture. *Fichier → Options* :
  langue (automatique : celle du système, français ou anglais, et l'anglais pour toute autre
  langue), thème (système, clair, sombre), réglages des nouvelles connexions. Ces fenêtres, comme
  les propriétés d'une session, sont des fenêtres à part (jamais rognées par la fenêtre
  principale) ; `Entrée` les valide, `Échap` les ferme.
- **Session** : une fenêtre par connexion, à la taille de l'écran distant (sans dépasser celui
  de ce PC). La barre d'outils (masquable) montre si la session est **chiffrée** (vert) ou non
  (orange, le détail au survol) et donne : plein écran, *Ajuster* (écran distant ajusté à la
  fenêtre, proportions gardées) ou *100 %* (un pixel distant par pixel de l'écran, centré, barres
  de défilement s'il est plus grand que la fenêtre), qualité, `Ctrl+Alt+Suppr`, lecture seule,
  actualiser, **Informations** (serveur, port, protocole, transport TCP, taille de l'écran,
  sécurité, couleurs, encodages reçus, données reçues et débit, boutons de souris, presse-papiers
  et clavier pris en charge), **Propriétés** (les réglages de l'entrée, dans une fenêtre à part,
  appliqués tout de suite ; le chiffrement et le partage du serveur à la connexion suivante),
  déconnecter. En plein écran, elle apparaît en approchant la souris du haut de l'écran.
  **`F8`** ouvre le même menu (plus *Envoyer F8* et l'affichage de la barre d'outils) ; `Échap`
  le referme. Les changements faits pendant la session sont gardés dans le carnet pour la fois
  suivante.
- **Authentification** : le curseur est dans le premier champ à remplir (nom d'utilisateur s'il
  est demandé et vide, sinon mot de passe) ; `Entrée` valide depuis l'un ou l'autre. Après un
  refus, *Se reconnecter* redemande les identifiants avec ceux déjà tapés, pour corriger sans
  tout retaper (le mot de passe est oublié une fois accepté).
- **Souris** : les trois boutons, la molette (verticale et horizontale), les boutons
  « précédent » et « suivant ». Le bouton « suivant » n'existe pas dans le protocole VNC de base :
  il ne part que vers les serveurs qui gèrent les boutons étendus (TigerVNC récent, QEMU…) ;
  *Informations* indique si c'est le cas. Le curseur du serveur est dessiné à la place de celui de
  ce PC au-dessus de l'écran distant, à l'échelle de l'image et lissé ; ses bords sont doux avec
  les serveurs qui envoient un curseur à transparence progressive (TigerVNC…), en escalier avec
  les autres, qui n'en envoient qu'un masque tout-ou-rien. Un serveur qui ne l'envoie pas à part
  le dessine dans l'image, et ce PC affiche alors une croix. En mode *Ajuster*, une image à peine
  plus petite que la fenêtre est affichée à sa taille réelle, nette, plutôt qu'agrandie de
  quelques pixels.
- **Clavier** : tant que la fenêtre de la session a le focus, les touches vont au serveur :
  caractères tapés (AltGr et touches mortes compris : `@`, `€`, `^` puis `e` donne `ê`),
  `Échap`, touches de fonction, raccourcis avec `Ctrl` ou `Alt` (avec `Maj` en plus, la lettre
  part en majuscule : `Ctrl+Maj+Z`). Seul `F8` reste à la visionneuse (*Envoyer F8* dans son
  menu, où les flèches haut et bas, ou `Tab`, passent d'un élément à l'autre en boucle sans en
  sortir, et `Entrée` valide). Sous Windows, un crochet clavier, dans son propre thread, prend en
  plus pour la session qui a le focus ce que Windows garde pour lui ou que la fenêtre ignore :
  `Alt+F4`, `Menu` (la touche du menu contextuel), `Pause`, `Maj+Suppr`, `Ctrl+Inser` et
  `Maj+Inser` (que la fenêtre changerait en couper, copier, coller) toujours, et selon les
  réglages :
  - *Touches spéciales* (activé par défaut) : touche Windows, avec toute touche appuyée en même
    temps (`Win+L`, `Win+Tab`, `Win+flèche`…), `Impr. écran`, `Alt+Tab`, `Alt+Échap`,
    `Ctrl+Échap` ;
  - *Touches monter, baisser et couper le volume* ;
  - *Touches multimédia* : lecture/pause, arrêt, piste précédente / suivante.

  Le programme ne s'inscrit pas à l'entrée brute (« raw input ») du clavier : tant qu'il l'est,
  Windows n'appelle pas le crochet pour ses propres fenêtres. `Ctrl+Alt+Suppr` ne peut pas être
  intercepté : bouton de la barre ou du menu `F8`. `Win+L` part au serveur, mais Windows verrouille
  quand même ce PC. Les verrouillages (`Verr. Maj`, `Verr. Num`) restent sur ce PC : les
  caractères envoyés en tiennent compte. Sous Linux, tout passe par la fenêtre, selon les mêmes
  réglages. Le bureau (GNOME, KDE…) garde d'ordinaire ses raccourcis
  (touche Windows, `Alt+Tab`, `Alt+F4`, `Impr. écran`, verrouillage…) : avec *Touches spéciales*,
  la session les lui demande tant qu'elle a le focus : sous Wayland par le protocole
  d'inhibition des raccourcis (GNOME demande une fois l'autorisation ; les touches de volume
  et multimédia échappent alors aussi au bureau, et ne partent que si leur réglage le permet),
  sous X11 en prenant le clavier (dès que le bureau le lâche, par exemple au relâchement d'`Alt`
  après un retour par `Alt+Tab`). *Informations* en donne l'état : pris, pas encore
  accordés, repris par le bureau…
- **Presse-papiers** : copié sur le serveur, il arrive sur ce PC ; copié sur ce PC, il part vers
  le serveur quand la fenêtre de la session a le focus. En texte Unicode avec les serveurs qui
  gèrent le presse-papiers étendu (TigerVNC…), sinon en Latin-1, le seul jeu du protocole de
  base (les caractères absents, comme `€` ou `Œ`, deviennent `?`) ; *Informations* indique lequel
  sert. RealVNC Server ne propose que le second aux autres visionneuses que la sienne : le `€` n'y
  passe pas, dans aucun sens (il le remplace lui-même par `?`).
- **Connexion perdue** ou refusée : la raison s'affiche, avec *Se reconnecter* (le mot de passe
  est redemandé s'il avait été refusé) et *Fermer*.
- **Pas de son ni de transfert de fichiers** : le protocole VNC standard ne transporte ni le son
  ni les fichiers. RealVNC les ajoute par des extensions qui lui sont propres, non publiées ;
  d'autres serveurs ont chacun la leur, incompatibles entre elles.
- Fermer la fenêtre principale avec des sessions ouvertes demande confirmation.

### Sécurité

| Méthode | Chiffrement | Serveurs |
|---|---|---|
| RSA-AES 256 bits (« RA2_256 ») | toute la session (AES-EAX) | RealVNC Server, TigerVNC |
| RSA-AES 128 bits (« RA2 ») | toute la session (AES-EAX) | RealVNC Server, TigerVNC |
| RSA-AES sans chiffrement (« RA2ne ») | identifiants seulement | RealVNC Server, TigerVNC |
| Apple (Diffie-Hellman) | identifiants seulement | partage d'écran de macOS (compte macOS) |
| Mot de passe VNC | aucun (défi DES) | presque tous |
| Aucune | aucun | serveurs sans mot de passe |

Réglage *Chiffrement* : **Toujours** (refuse un serveur qui ne chiffre pas), **De préférence**
(par défaut : le plus sûr que le serveur propose) ou **Seulement si imposé**. Avec RSA-AES, la
première connexion montre la **signature** de la clé du serveur (`xx-xx-xx-xx-xx-xx-xx-xx`, au
format affiché par RealVNC Server) à comparer avec celle du serveur ; une fois acceptée, elle est
gardée, et une signature différente plus tard est signalée en rouge (serveur réinstallé, ou
quelqu'un qui se fait passer pour lui). La clé RSA du client est créée pour chaque connexion.

Mots de passe mémorisés (case *Mémoriser le mot de passe*, ou propriétés) : sous Windows,
chiffrés par DPAPI, lisibles seulement par le même compte Windows sur le même PC ; sous Linux,
seulement masqués dans le fichier de configuration.

Comme RealVNC Viewer, le logiciel ne contacte que les serveurs VNC demandés : ni compte, ni
mise à jour, ni statistiques. Pas de connexion par le cloud de RealVNC (qui exige un compte) :
seulement les connexions directes, par l'adresse du serveur.

### Qualité de l'image

| Réglage | Couleurs | Encodages |
|---|---|---|
| Automatique (par défaut) | 16 millions | Tight avec JPEG pour les zones photographiques, ZRLE… |
| Haute | 16 millions, sans perte | Tight sans JPEG, ZRLE, Hextile… |
| Moyenne | 65 536 | Tight, ZRLE… plus compressés |
| Basse | 256 | idem, pour une liaison lente |

Encodages gérés : Tight (JPEG compris), ZRLE, Hextile, Zlib, RRE, CopyRect, Raw ; curseur du
serveur (à transparence progressive si possible), changement de taille de l'écran distant, nom
du bureau, presse-papiers étendu, boutons de souris étendus.

La configuration est un fichier TOML lisible : `%APPDATA%\vnc-viewer\config.toml` (Windows) ou
`~/.config/vnc-viewer/config.toml`. **Mode portable** : un fichier `vnc-viewer.toml` à côté de
l'exécutable est utilisé à la place. Un fichier d'une version précédente reste lisible : les
nouveaux réglages prennent leur valeur par défaut, ceux qui ont disparu sont ignorés.
