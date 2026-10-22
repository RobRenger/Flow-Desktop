# Fork van Rob Renger: Flow Desktop met Google Cast

Deze fork (github.com/RobRenger/Flow-Desktop) voegt casten naar Google Cast-speakers en speakergroepen toe aan Flow Desktop. Upstream is github.com/Flow-Tube/Flow-Desktop.

## Branches
- `main` volgt upstream één op één. Hier komen geen eigen commits.
- `chromecast` is `main` plus de eigen wijzigingen. Hiervan bouw je de app.

Lokaal staat `chromecast` als worktree in `~/Developer/flow-desktop-cast` (buiten OneDrive). De clone in de OneDrive-projectmap houdt `main` vast.

## Upstream binnenhalen
Vanuit de clone in de projectmap (branch `main`):

```bash
git fetch upstream
git merge --ff-only upstream/main
git push origin main
```

Lukt `--ff-only` niet, dan staat er per ongeluk een eigen commit op `main`. Verplaats die naar `chromecast` en zet `main` terug op `upstream/main`.

Daarna in de worktree:

```bash
cd ~/Developer/flow-desktop-cast
git merge main
```

Gebruik merge, geen rebase. `chromecast` staat gepubliceerd op GitHub.

## Waar conflicten te verwachten zijn
| Bestand | Eigen wijziging |
|---|---|
| `src-tauri/src/lib.rs` | registratie van de cast-commando's en het `cast`-module |
| `src-tauri/Cargo.toml`, `Cargo.lock` | crate `tokio-rustls` voor de TLS-verbinding met speakers |
| `src/store/useMusicPlayerStore.ts` | één regel voor de koppeling met de cast-status |
| `src/lib/audio/musicAudioEngine.ts` | `RemoteOutput` voor afspelen op een speaker |
| `src/components/music/GlobalMusicDock.tsx`, `GlobalMusicAudio.tsx` | cast-knop en koppeling met `useCastStore` |
| `src/lib/i18n/strings.json` | teksten voor casten en `settings_updates_fork` |
| `src-tauri/src/update_support.rs`, `src/lib/updateSupport.ts` | reden `fork`, updater blijft uit |
| `src-tauri/tauri.conf.json` | updater-endpoint wijst naar RobRenger/Flow-Desktop |

Alleen nieuw in de fork, dus zonder conflicten: `src-tauri/src/cast/`, `src/lib/cast/`, `src/store/useCastStore.ts`, `src/components/music/CastPopover.tsx`.

Bij een conflict in een lockbestand: neem de versie van upstream en laat de tool hem opnieuw opbouwen (`cargo check`, `pnpm install`).

## Na elke merge controleren
1. De updater blijft uit. In `src-tauri/src/update_support.rs` staat `FORK_BUILD: bool = true`, en `tauri.conf.json` heeft geen Flow-Tube-endpoint. Anders kan de app zichzelf vervangen door de officiële versie zonder casten.
2. Testen en bouwen:

```bash
cd ~/Developer/flow-desktop-cast && source ~/Developer/fd-env.sh && pnpm install && pnpm exec tsc --noEmit && pnpm exec vitest run && (cd src-tauri && cargo test --lib)
```

```bash
cd ~/Developer/flow-desktop-cast && source ~/Developer/fd-env.sh && pnpm tauri:build:local --bundles app
```

`fd-env.sh` is nodig omdat `~/.local/bin/cc` de C-compiler overschaduwt. De app staat daarna in `src-tauri/target/release/bundle/macos/Flow.app`.

3. Kort testen: cast naar een speakergroep, volgend nummer, pauze, doorspoelen, volume, stoppen.
4. Pushen: `git push origin chromecast`.
