# Plan de réécriture de Spytify en Rust

Spytify enregistre ce que joue le client Spotify pour Windows, découpe le flux en morceaux, ignore les pubs et écrit les tags. La version d'origine est en C# / WinForms (`E:\Workspace\spy-spotify`). Cette réécriture garde le principe (on enregistre la sortie, on ne télécharge rien) et ajoute le **Lossless de Spotify** (FLAC jusqu'à 24 bits / 44,1 kHz).

## Décisions prises

| Sujet | Choix | Raison |
| --- | --- | --- |
| Interface | **Slint 1.18**, rendu logiciel, style Fluent | Mesuré dans `WaveFlow/prototypes/rust_slint_mini` : 10,5 Mo privés contre 78 Mo pour egui. L'appli tourne en fond toute la nuit. Widgets de formulaire prêts, i18n intégrée (`@tr`). |
| Capture | **WASAPI process loopback** sur l'arbre `Spotify.exe` | Seul le son de Spotify est capturé, sans pilote. Mais la capture passe par la chaîne du périphérique où joue Spotify (fréquence, améliorations, surround) : d'où l'option **câble virtuel** (VB-Cable, non fourni), qui envoie Spotify sur le câble avec l'interface COM non documentée de `Router/AudioRouter.cs` (`core::routing`). |
| Encodage | Capture dans un WAV float 32 bits temporaire, encodage à la fin du morceau | Le chemin de capture reste simple ; l'encodeur connaît l'analyse complète du morceau (profondeur FLAC). C'est aussi ce que fait la version C#. |
| Plateforme | Windows uniquement | Spotify desktop + WASAPI. Le code Windows est derrière `#[cfg(windows)]`. |

## Décisions ouvertes

- **Version minimale de Windows.** Le process loopback est documenté à partir de Windows 10 build 20348. À vérifier en pratique sur Windows 10 22H2 (build 19045).
- **API Spotify Web.** Voir [ci-dessous](#api-spotify-web). Proposition : Deezer + Last.fm par défaut, API Spotify en option plus tard.

Réglé : le code repris de WaveFlow peut être republié ici sous MIT. Les fichiers concernés (`metadata/deezer.rs`, `lastfm.rs`, `name_match.rs`, `album_match.rs`, `artwork/`, `tagio.rs`) n'ont qu'un seul auteur, le propriétaire des deux dépôts. Revérifier avec `git log --format='%an' -- <fichier>` avant de copier un autre fichier.

## Phases

### Phase 0 — Valider la capture (validée le 1er octobre 2026)

Fait : `spytify-core::capture` (process loopback), `spotify::find_root_pid`, `analysis::BitAnalysis`, `encode::flac`, et l'exemple :

```powershell
cargo run -p spytify-core --example capture_spotify -- 30 test.flac
```

Questions à trancher avec le vrai client :

1. La capture fonctionne avec `include_tree` sur le processus racine (l'audio est rendu par un processus enfant).
2. **Volume** : le process loopback capture-t-il avant ou après le volume de session Windows et le volume de Spotify ? Tester 100 % puis 80 %, et regarder `bit-transparent`.
3. **Fréquence du périphérique** : périphérique par défaut en 48 kHz puis en 44,1 kHz. Spotify ou Windows rééchantillonne-t-il avant nous ?
4. **Coupure du son pour les pubs** : si on coupe la session audio de Spotify (comme `MainAudioSession` en C#), la capture reçoit-elle encore le son ? Cela décide comment ignorer les pubs.
5. Pause : que reçoit la capture ?

Mesures du 1er octobre 2026 (Spotify Microsoft Store, casque Logitech PRO X) :

| Point | Résultat |
| --- | --- |
| 1. Capture de l'arbre de processus | ✅ 30,0 s capturées, aucune perte (`discontinuities 0`) |
| 2. Volume | Le volume du périphérique (12 %, −30,7 dB) **n'est pas** dans la capture : on peut écouter à n'importe quel volume. Curseur Spotify au maximum. |
| 2 bis. Canaux | ✅ Le casque (G HUB, surround virtuel) est un périphérique **7.1**. En stéréo, Windows remixait les 8 canaux : −13 dB et signal recalculé. Corrigé dans le code : capture dans la disposition du périphérique, paire avant gardée (`format::front_stereo`). Les 6 autres canaux sont vides. `SPYTIFY_FORCE_STEREO=1` refait l'ancienne capture. |
| 2 ter. Effets | ✅ **Cause du traitement restant : « Améliorations audio » de Windows** (*Device Default Effects*) sur le casque. Désactivées : `bit-transparent: yes, source is 16-bit` sur un morceau Lossless 16 bits. Désactiver seulement l'égaliseur G HUB ne suffisait pas : G HUB passe probablement par ces effets. Égaliseur et normalisation de Spotify étaient déjà désactivés (non testés séparément). |
| 3. Fréquence du périphérique | ✅ **Le périphérique doit être en 44,1 kHz.** En 48 kHz, le flux est rééchantillonné avant la capture (81 % hors grille, écart moyen 0,25 pas). Spotify Lossless ne dépasse jamais 44,1 kHz, donc rien n'est perdu à l'imposer. |
| 3 bis. Source 24 bits | ⚠ « Heaven » (swim school, Lossless 24 bits) à 44,1 kHz : **presque transparent**. 13,6 % des échantillons sont hors grille 24 bits, avec un écart moyen de 0,02 à 0,09 pas, et la première seconde est parfaite. Analyse (`analyze_wav`) : décalages à **tous les niveaux** (pas un limiteur), **gauche et droite aux mêmes trames** (103 003 contre ~13 600 au hasard), en rafales d'environ 5 trames, d'au plus un demi-pas 24 bits (≈ −144 dBFS). Signature d'un signal quasi nul ajouté à la musique, ou d'un décodage en float. Ce n'est pas un processus enfant à lui seul : exclure chacun ne change rien. Automix désactivé : inchangé. Pas un décodage milieu/côté (21 % seulement de décalages identiques à gauche et à droite, corrélation +0,27). **Conclusion : le traitement 24 bits interne de Spotify.** Le morceau 16 bits était à 0,00 % avec les mêmes réglages et la même chaîne, donc ni Windows ni un flux parasite ne sont en cause. Conséquence : l'export FLAC 24 bits arrondit à l'entier le plus proche, ce qui retrouve exactement la source tant que l'écart reste sous ½ pas. Seuls les écarts d'exactement ½ pas (≈ 4 % des échantillons, passages forts) sont ambigus à 1 pas près. **Statut : 24 bits « quasi transparent ».** Phase 3 : faire distinguer à `BitAnalysis` ce cas (écart max ≤ ½ pas 24 bits) d'un vrai traitement.  À noter : `include_tree: false` de wasapi capture tout **sauf** l'arbre ; Windows n'a pas de mode « ce processus seul ». Un casque limité à 16 bits en sortie n'empêche pas la capture 24 bits. |
| 4. Coupure du son pour les pubs | À tester (`session_volumes` lit l'état, il faut encore pouvoir couper) |
| 5. Pause | ⚠ Spotify Store **continue d'envoyer des paquets silencieux** en pause. Le silence n'est plus compté comme « transparent ». |

Diagnostics ajoutés : `audio_setup::default_output_device` (format du périphérique) et `audio_setup::session_volumes` (volume et coupure des sessions Spotify), affichés par `capture_spotify`.

Critère de sortie, **atteint** : un morceau affiché « Lossless 16-bit » par Spotify est capturé bit-transparent et détecté en 16 bits. Restent à éclaircir sans bloquer la suite : le léger écart des sources 24 bits (point 3 bis) et la coupure du son pour les pubs.

### Phase 1 — Détection de Spotify (en cours)

Fait :

- `spotify::process` : processus racine et liste des PID.
- `spotify::window` : titre de la fenêtre Chromium, même si elle est masquée dans la zone de notification.
- `spotify::smtc` : session SMTC.
- `spotify::title` : analyse du titre, avec les cas des tests C#.
- `spotify::state` : combine titre, SMTC et activité audio, puis calcule les événements.
- `spotify::monitor` : thread qui interroge toutes les 500 ms.
- Exemple `spotify_probe` pour observer Spotify en direct.

Constaté le 1er octobre 2026 avec la version Microsoft Store de Spotify :

- Le processus s'appelle bien `Spotify.exe`. L'identifiant SMTC est `SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify` (`Spotify.exe` attendu pour l'installateur classique, non vérifié).
- En pause, le titre de fenêtre repasse à `Spotify Premium`, mais **SMTC garde le morceau** (titre, artiste, album, n° de piste, durée, position) avec `playing: false`. `state::classify` s'en sert : une pause n'émet que `PlayStateChanged`, plus de faux changement de morceau.
- Au changement de morceau, le titre de fenêtre change en premier ; SMTC suit au poll suivant, et **pas d'un bloc** : la durée du morceau suivant a été vue à côté du titre précédent.

Reste à faire :

- Observer avec `spotify_probe` : une pub (compte Free), un podcast, l'installateur classique (non Store).
- Brancher `audio_active` sur la capture (phase 2).

Comportement initial à porter depuis `Spotify/SpotifyProcess.cs`, `SpotifyStatus.cs` et `SpotifyHandler.cs` :

- Titre de la fenêtre principale de l'arbre Spotify (`EnumWindows` + PID) ; `Spotify`, `Spotify Free` ou `Spotify Premium` = en veille ou en pub, sinon `Artiste - Titre`.
- Événements : changement de morceau, lecture/pause, position.
- Étudier SMTC (`GlobalSystemMediaTransportControlsSessionManager`) : artiste, titre, album et durée sans analyser le titre. Garder le titre de fenêtre en secours, en particulier pour reconnaître les pubs.
- Reprendre les cas de `EspionSpotify.Tests/SpotifyStatusTests.cs` et `SpotifyProcessTests.cs`.
- `EspionSpotify.FakeSpotify` produit un faux `Spotify.exe`. Il est reconnu par `find_root_pid` et reste utile pour tester sans le vrai client.

### Phase 2 — Moteur d'enregistrement (en cours)

Fait, dans `recorder/` :

- `splitter` (pur, testé) : l'audio est retenu 3 s avant d'être écrit, et chaque changement est placé à l'échantillon où il a eu lieu, dans le passé. La coupure est recalée sur le silence le plus proche (±300 ms d'après le titre, ±100 ms quand SMTC a daté le début). Pause : le silence numérique est ignoré et le fichier continue. Pubs et inactivité ne sont pas écrites. Silence coupé aux deux extrémités, gardé à l'intérieur. Détails SMTC reçus moins de 1,5 s avant une coupure ignorés.
- `clock` : correspondance instant ↔ échantillon, sans extrapoler à travers une pause sans paquets.
- `naming` : `Artiste - Titre.flac`, caractères interdits remplacés, noms réservés évités, doublons numérotés.
- `engine` (Windows) : capture + moniteur + découpeur, un WAV temporaire par morceau dans `.spytify-tmp`, encodage FLAC sur un thread à part, profondeur choisie par `analysis::Fidelity`. Reprise automatique de la capture si Spotify redémarre. Morceaux partiels (rejoints en cours, sautés) et trop courts écartés par défaut.
- `monitor` envoie des événements horodatés, plus `TrackStart` : le début du morceau daté par la position SMTC et l'heure de sa mise à jour.
- Exemple `record` : `cargo run -p spytify-core --example record -- <dossier> [minutes] [--keep-partial]`.

Premier essai réel (1er octobre 2026) :

- Enchaînements et morceaux partiels corrects (rejoint en cours, interrompu à l'arrêt).
- **Pause prise pour une pub** : le fondu de sortie de Spotify s'entend encore au poll suivant. Corrigé dans `state::classify` : titre de veille + SMTC en pause sur le même morceau = pause, même avec du son.
- **Tout enregistré « PROCESSED »**, sur toute la durée (99,4 % hors grille 16 bits d'après `analyze_flac`) : les **améliorations audio étaient réactivées** (*Device Default Effects*, effet « Logitech HX2E Surround Sound Effect »), probablement par G HUB, alors qu'une capture précédente les montrait sur Off. **Détection fiable**, vérifiée dans les deux sens le 1er octobre 2026 : lire `MMDevices\Audio\Render\{id}\FxProperties` dans le registre. un effet est installé (`{d04e05a6…},1/2/5/6/7`) et `disable_sysfx` (`{1da5d803…},5`) ≠ 1 ; windows **supprime** cette valeur quand on choisit *device default effects*. l'api `immdevice::openpropertystore` ne voit pas ces valeurs (toujours vt_empty). `recorderevent::capturestarted` porte le périphérique et ses `lossless_issues()`.
- Deuxième essai, effets désactivés : **pause correcte** (un seul fichier) et durées justes à 1 s près (silence retiré aux extrémités). Slow et Heaven (24 bits) : quasi transparents. PLAIINS (16 bits) : bit-perfect. Tell Me I'm Wrong (16 bits) : propre sauf le fondu de la pause (0:21-0:22) et trois rafales de ~200 échantillons. **Modern Dinosaur (16 bits) : limiteur** : 35,5 % d'échantillons modifiés là où le niveau local est à moins de 3 dB du maximum, aucun en dessous (`analyze_flac`). Tell Me, aussi fort, n'est pas touché. À trancher : effet du morceau précédent (Slow, 24 bits, enchaîné) ou propre au morceau.
- **Modern Dinosaur, lancé à la main** : toujours limité (2 premières secondes exactes, puis jusqu'à 58 % d'échantillons modifiés dans les passages forts). **C'est propre au morceau**, pas à l'enchaînement. Nouvelle classe `Fidelity::PeakLimited` : écarts uniquement dans les blocs de 10 ms à moins de 3 dB du maximum, et au moins 1 s de passages plus calmes intacts. Écrit en 24 bits.
- Troisième essai (phase 4) : tags Deezer trouvés pour les trois morceaux complets. **Le registre s'est contredit** : `Disable_SysFx` absent (donc « actif ») alors que Windows affichait Off, et Tell Me I'm Wrong sortait intact (610 échantillons sur 17,8 M). La lecture du registre n'est donc qu'un **indice** (« peuvent être actives ») ; le verdict, c'est la `Fidelity` de chaque morceau. `BitPerfect` tolère désormais 1 échantillon sur 10 000 modifié (micro-fondus de Spotify) et les compte (`touched`) ; `PeakLimited` exige ≥ 0,1 % des crêtes modifiées.
- À faire : revérifier ce réglage pendant une session (g hub peut le réactiver en cours de route), et vérifier si le changer s'applique au flux déjà ouvert par spotify ou s'il faut le redémarrer.
- À traiter : un fondu de pause ou de reprise modifie quelques centaines d'échantillons. Un morceau 16 bits mis en pause ne sera plus « bit-perfect » et sera écrit en 24 bits. Piste : tolérer une infime part d'échantillons hors grille pour le choix de la profondeur.

À vérifier avec le vrai client : la précision des coupures (début et fin de chaque fichier), pause/reprise, saut de morceau, fermeture de Spotify pendant l'enregistrement, changement de périphérique de sortie en cours de session (la disposition des canaux est lue au démarrage de la capture).

Plan initial :

Porter `Watcher.cs`, `Recorder.cs` et `AudioSessions/AudioThrottler.cs` :

- Machine à états : morceau courant, changement → finaliser le morceau précédent et démarrer le suivant.
- **Pause / reprise** : le moniteur n'émet que `PlayStateChanged` (le contenu reste le même morceau). Le recorder continue le même fichier. Une pause suivie d'un saut de morceau donne bien un changement.
- **Métadonnées SMTC** : garder les `DetailsUpdated` restés stables (au moins deux polls), pas les derniers reçus juste avant le changement de morceau, qui peuvent mélanger deux morceaux.
- `audio_active` du moniteur : vrai quand la capture reçoit des paquets **non silencieux** (Spotify Store envoie du silence en pause). C'est ce qui révèle une pub derrière un titre « Spotify Free ».
- Tampon de pré-écoute (`AudioCircularBuffer`) pour ne pas perdre le début d'un morceau détecté en retard. Le titre change environ 500 ms après le son. Ring buffer `rtrb`, comme le moteur audio de WaveFlow.
- Suppression du silence au début et à la fin (`SilenceAnalyzer`), durée minimale, morceaux déjà enregistrés ignorés, numéros d'ordre, nommage et dossiers (`Native/FileManager.cs`, tests `FileManagerTests.cs`).
- Pubs : selon le point 4 de la phase 0.

### Phase 3 — Formats de sortie (fait)

- `RecorderConfig::format` (`OutputFormat`) : `Flac { depth }`, `Wav { depth }` (PCM entier), `Mp3 { kbps }` (CBR 128/160/192/256/320). `BitDepth::Auto` reprend la profondeur que la capture contient (`Fidelity::lossless_depth`).
- `encode::Quantizer` : arrondi exact à profondeur égale, **dither TPDF** quand on réduit (source 24 bits écrite en 16).
- `encode::mp3` : LAME via `mp3lame-encoder` (compilé depuis les sources, sans DLL), qualité maximale ; l'en-tête **Info** de LAME est réécrit en tête de fichier, pour la lecture sans blanc entre morceaux.
- Tags : Vorbis pour le FLAC, **ID3v2** pour MP3 et WAV (`TrackTags::write`), pochette comprise.
- Exemple `record --format=flac|flac16|flac24|wav|wav16|wav24|mp3|mp3:<kbps>`.

Plan initial :

- WAV : `hound`.
- MP3 CBR 128 à 320 kbps : `mp3lame-encoder` (LAME, comme la version C#).
- FLAC : fait. Ajouter un dither TPDF quand l'utilisateur force 16 bits sur une source 24 bits.

### Phase 4 — Tags et métadonnées (en cours)

Fait, dans `metadata/` :

- `deezer` : client bloquant repris de WaveFlow (gestion des refus dans le corps de la réponse, limite de débit), avec morceau, album et pochette. **La recherche avancée `artist:"…" track:"…"` ne renvoie plus rien** (vérifié le 1er octobre 2026) : recherche en texte libre, puis sélection stricte.
- `lookup` (pur, testé) : un résultat n'est retenu que si le titre (≥ 0,85, featuring ignoré), l'artiste (un de ceux listés par Spotify) et la durée (±3 s) concordent ; l'album départage. Un remix ou un live est rejeté. Mieux vaut des tags incomplets que faux.
- `name_match` : repris de WaveFlow, avec `title_similarity`.
- `tags` : `TrackTags` construit depuis Spotify et SMTC, complété par Deezer (album, artiste de l'album, piste/total, disque, date, genres, ISRC, label, pochette XL). Écrit via le tag concret `VorbisComments` de lofty.
- Moteur : FLAC encodé et tagué dans `.spytify-tmp`, puis déplacé dans le dossier final. `RecorderEvent::Saved` indique l'origine des tags (`TagsOutcome`).
- Exemple `tag_lookup "Artiste" "Titre" ["Album"] [secondes]`.

Vérifié en réel : Tell Me I'm Wrong, Modern Dinosaur, Heaven et Waterfalls (feat.) trouvés, avec ISRC, label, genres et pochette.

Reste : Last.fm en secours (clé API), MP3 (ID3v2) avec la phase 3, réglage pour désactiver la recherche en ligne.

Plan initial :

- `lofty` : ID3v2 pour le MP3, commentaires Vorbis et image pour le FLAC. Écrire via le tag concret, comme la règle de WaveFlow (`edit::patch_file`), sinon des champs non standards disparaissent.
- Reprendre de `WaveFlow/src-tauri/crates/core` : `metadata/deezer.rs`, `metadata/lastfm.rs`, `metadata/name_match.rs`, `album_match.rs`, `artwork/`, `tagio.rs` (écriture fiable sous Windows : nouvelles tentatives si un antivirus verrouille le fichier). Copier les modules plutôt que dépendre de `waveflow-core`, qui embarque sqlx, symphonia, libopus et wasmtime.
- Reprendre les cas de `MapperID3Tests.cs`, `LastFMAPITests.cs`.

### Phase 5 — Interface Slint (en cours)

Choix (1er octobre 2026) : **écran unique** (état de Spotify, morceau en cours, bouton, liste des morceaux de la session avec leur qualité) et **panneau latéral de réglages**. Options reprises de la version C# : organisation des fichiers, morceaux déjà enregistrés, pubs, câble virtuel (rajouté le 2 octobre 2026 : il retire le casque de la chaîne enregistrée). Abandonnées : choix libre du périphérique audio, identifiants API Spotify (Deezer), minuterie.

Fait côté interface (`crates/app`) :

- `ui/app-window.slint` : en-tête, carte « en cours » (état de Spotify, titre, artiste, album, minuteur REC), avertissements, bouton Démarrer/Arrêter, liste de la session avec un badge de qualité par morceau, panneau de réglages qui glisse depuis la droite.
- Textes en anglais dans `@tr()`, traduits par `translations/fr/LC_MESSAGES/spytify.po`, intégré au binaire ; la langue change à chaud.
- `app.rs` : un moniteur dédié à l'affichage (l'état de Spotify est visible même sans enregistrer), les événements du moteur transmis à la boucle Slint, l'arrêt (qui attend l'encodage du dernier morceau) hors du thread de l'interface, réglages enregistrés à chaque modification, choix du dossier avec `rfd`.
- `mapping.rs` (pur, testé) : listes de l'interface ↔ réglages.
- Vérifié le 1er octobre 2026 : fenêtre en français, morceau en cours et changements suivis.
- Câble virtuel (2 octobre 2026) : `core::routing` lit et change le périphérique de sortie choisi pour Spotify (Paramètres → Mélangeur de volume), interfaces 21H2+ et antérieure ; `audio_setup::spotify_output_device` vérifie ce périphérique-là, et non plus le périphérique par défaut. Réglage « Faire passer Spotify par un câble virtuel » : pendant l'enregistrement seulement, Spotify est envoyé sur « CABLE Input » (rendu à son périphérique par `Drop`, même sur erreur ; une session interrompue est réparée à la suivante). Configuration automatique, sans droits administrateur, par `core::device_config` (`IPolicyConfig`, l'interface du panneau Son) : câble passé en 44,1 kHz (VB-Cable s'installe en 48 kHz, vérifié le 2 octobre 2026), et si l'installateur a fait du câble le périphérique par défaut, alerte avec un bouton pour revenir au vrai périphérique. Option « Entendre Spotify pendant l'enregistrement » (`core::playback`) : l'appli rejoue la capture sur le périphérique par défaut, à la place d'« Écouter ce périphérique » ; désactivée, Spotify enregistre en silence et on peut écouter autre chose. Le pilote n'est pas fourni (licence VB-Audio « all rights reserved »), un lien mène au site. Exemple `route_spotify [cable|default]`. **Mesuré le 3 octobre 2026** : par le câble, casque volontairement en améliorations actives, Oishii et Tell Me I'm Wrong (16 bits) sortent bit-perfect 16 bits, coupures nettes ; yes, and? reste « crêtes limitées » (fichier Spotify). Fausse piste du même jour : une mise à jour de Spotify avait réactivé fondu enchaîné, Automix et normalisation. Symptômes : coupures décalées de 0 à 1,25 s, début et fin de morceau hors grille, tout « incomplet ». D'où `BitAnalysis::fades_in` (2 premières secondes hors grille, le reste beaucoup moins) et l'alerte `SpotifyFades`. Exemples ajoutés : `align_flac` (décalage entre deux enregistrements d'un morceau), `record --cable|--no-cable`.

Fait le 4 octobre 2026 : pochette du morceau en cours (vignette SMTC, `Smtc::spotify_thumbnail`, décodée par `image` sur un thread à part, relue 0,5 s puis 2 s après chaque changement) ; icône dans la zone de notification (`tray-icon`, menu Afficher / Démarrer-Arrêter / Quitter, rouge pendant l'enregistrement ; fermer la fenêtre pendant un enregistrement la cache au lieu de quitter). Panneau de réglages testé par l'utilisateur.

Fait côté moteur :

- `settings` : `Settings` en TOML dans `%APPDATA%\Spytify\settings.toml` (écriture atomique, fichier abîmé mis de côté, champs manquants par défaut), `recorder_config()`.
- `naming` : sous-dossiers (`Folders::Artist`, `ArtistAlbum`, d'après l'artiste de l'album et l'album trouvés par les tags), préfixe (`TrackNumber` « 04 », `OrderNumber` « 001 » dans la session).
- `library` : index des morceaux déjà présents (parcours récursif au démarrage, clé `Artiste - Titre` sans préfixe ni « (2) »). `ExistingTracks::Skip` (pas écrit du tout, et en option Spotify passe au suivant via SMTC), `Replace`, `KeepBoth`.
- Pubs : Spotify coupé dans le mélangeur dès qu'une pub est repérée, rétabli ensuite. **À vérifier avec un compte Free** : si la coupure atteint aussi la capture, le début du morceau qui suit la pub pourrait être perdu (le son est rétabli au moment où le changement est remarqué, la coupure est placée plus tôt).

Plan initial :

Porter `frmEspionSpotify` : bouton d'enregistrement, console de logs, réglages (dossier, format, débit, qualité Spotify, profondeur FLAC, durée minimale, options sur les pubs, langue), réduction dans la zone de notification.

- i18n : `@tr()` + gettext, `fr` et `en`. Remplace les `.resx`, `TranslationKeys` et `I18NKeys`.
- Réglages : un fichier TOML dans `%APPDATA%\Spytify`.

### Phase 6 — Distribution

- CI GitHub Actions sur `windows-latest` : `cargo fmt --check`, `clippy -D warnings`, `cargo test`.
- Installateur (MSI ou `cargo-packager`) et mise à jour via les releases GitHub (remplace `EspionSpotify.Updater`).

## Lossless (nouvelle fonctionnalité)

Spotify Lossless : Premium, réglage *Qualité audio → Lossless* dans le client, FLAC jusqu'à 24 bits / 44,1 kHz. Certains morceaux sont des masters 16 bits.

Ce que fait Spytify :

1. Il capture à 44,1 kHz en float 32 bits (`format::CAPTURE_SAMPLE_RATE`), sans rééchantillonnage de notre côté.
2. `analysis::BitAnalysis` vérifie chaque échantillon : si le signal n'a pas été modifié, chaque valeur float est exactement un entier 16 ou 24 bits. Un seul étage de gain ou de rééchantillonnage fait sortir la quasi-totalité des échantillons de cette grille. Une capture non transparente est donc détectée sur un morceau entier.
3. Il écrit en FLAC avec `BitDepth::Auto` : 16 bits si la source l'est, 24 bits sinon.
4. Il indique dans la console, morceau par morceau, si la capture était bit-transparente. Sinon, il donne les réglages à vérifier.

Réglages demandés à l'utilisateur, d'après la phase 0 :

- Spotify : qualité **Lossless**, curseur de volume au maximum, *Normaliser le volume* et **égaliseur** désactivés, **crossfade et Automix désactivés** (sinon les fins de morceaux se mélangent, quel que soit le format).
- Windows : **« Améliorations audio » sur Off** pour le périphérique de sortie (cause vérifiée), **format du périphérique en 44,1 kHz** (vérifié : 48 kHz rééchantillonne), son spatial désactivé, session Spotify à 100 % dans le mélangeur.
- Pas besoin de toucher au volume Windows ni de repasser un casque 7.1 en stéréo : la capture n'en dépend pas.

À vérifier par l'appli elle-même (phase 5) :

- `BitAnalysis` sur chaque morceau, avec le diagnostic de `capture_spotify` (`estimate_gain` pour un volume, `grid_residual` pour un effet) traduit en conseil.
- Lire l'état des améliorations audio avant d'enregistrer (propriété `PKEY_AudioEndpoint_Disable_SysFx` du périphérique) et prévenir l'utilisateur.
- Piste à étudier : si seul un gain constant gêne (curseur Spotify pas au maximum), diviser par le pas estimé redonne les entiers d'origine exactement. À vérifier bloc par bloc, car le gain peut changer en cours de morceau.

Choix d'interface : la qualité Spotify (`format::SpotifyQuality`) est un réglage. *Lossless* propose FLAC Auto par défaut, *Premium* MP3 320, *Free* MP3 160. Les pubs n'existent qu'en Free : la logique pubs ne s'applique pas aux deux autres.

## API Spotify Web

La version C# (`API/SpotifyAPI.cs`) s'en sert pour deux choses :

- **Lecture en cours** (`GET /me/player`) : morceau exact (ID), position, et `currently_playing_type = "ad"` pour reconnaître les pubs.
- **Tags** : titre, artistes, numéro de piste et de disque, album, artistes de l'album, date de sortie, pochette 640 px. Les genres d'album sont presque toujours vides.

Depuis les changements de février 2026, pour les applis en mode développement :

- le propriétaire de l'appli doit avoir **Premium**, sinon l'appli cesse de fonctionner. Chaque utilisateur crée sa propre appli : les utilisateurs Free, ceux qui ont des pubs, ne peuvent plus du tout s'en servir ;
- `external_ids` (l'**ISRC**) a été retiré des pistes et des albums, tout comme `label` et `popularity` ;
- la récupération groupée (`GET /tracks`, `GET /albums`) a été supprimée.

Ce qui reste utile par rapport aux autres sources :

| Besoin | Sans API Spotify |
| --- | --- |
| Artiste, titre, album, position | SMTC (phase 1) |
| N° de piste et de disque, date, pochette HD, genres, label, ISRC | Deezer, recherche artiste + titre + album (`album_match.rs`) |
| Reconnaître les pubs | Titre de fenêtre / SMTC (Free uniquement) |
| Bonne version du morceau (remaster, live, édition) | L'ID Spotify est sans ambiguïté ; Deezer peut se tromper de version. C'est le seul vrai apport. |

Décision proposée : ne pas la porter dans la première version. Si des erreurs de version apparaissent en pratique, l'ajouter en option pour les Premium (OAuth PKCE en local, sans secret client).

## Correspondance C# → Rust

| C# (`spy-spotify`) | Rust (`spytify`) |
| --- | --- |
| `Spotify/SpotifyProcess.cs`, `SpotifyStatus.cs`, `SpotifyHandler.cs` | `core::spotify` (phase 1) |
| `AudioSessions/*`, `Router/*`, `Drivers/*` | `core::capture` (process loopback), `core::routing` (câble virtuel, pilote non fourni) |
| `Watcher.cs`, `Recorder.cs` | `core::recorder` (phase 2) |
| `Native/FileManager.cs` | `core::files` (phase 2) |
| `Recorder.GetMediaFileWriter` (NAudio.Lame) | `core::encode` |
| `API/*`, `MapperID3.cs` | `core::metadata`, `core::tags` (phase 4) |
| `frmEspionSpotify.cs`, `Translations/*` | `app` + `ui/*.slint` (phase 5) |
| `Settings.settings`, `Models/UserSettings.cs` | `core::settings` (TOML) |
| `EspionSpotify.Updater` | phase 6 |
