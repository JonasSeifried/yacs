# Release checklist

Checks after a release, on the real apps, instead of building on every platform first. Copy the release's section, tick it off, and delete it once the release is good.

## Before tagging

- [ ] CI is green on `main` for all three desktop platforms (the `desktop` jobs build and clippy the app on macOS, Windows and Linux).

## Every release

**Release run**
- [ ] The Release workflow is green, and the release page has the installers (`.dmg`, `-setup.exe`, `.msi`, `.AppImage`, `.deb`, `.rpm`, plus the fixed-name copies), the `yacs-cli-*` binaries with their `.sig` files, and the relay image `ghcr.io/jonasseifried/yacs:<version>`.

**Relay** (home server, in `deploy/`)
- [ ] Update it: `yacs relay update`, or `docker compose -f compose.nginx.yaml pull && docker compose -f compose.nginx.yaml up -d`.
- [ ] `curl -s https://yacs-relay.jonasseifried.com/api/v1/config` shows the new `version`.
- [ ] `docker compose -f compose.nginx.yaml logs --since 10m yacs` has no errors.

**Desktop** (each of macOS, Windows, Linux)
- [ ] The app updates itself (Settings → Updates, or the tray) and Settings shows the new version afterwards.
- [ ] The hotkey opens Spotlight; ⌘V / Ctrl+V sends the clipboard; ↵ copies a clip back.
- [ ] A clip sent from here shows up on the phone and the other computers, and the other way round: text, a screenshot, a file.

**Phone** (iPhone home-screen app, Android)
- [ ] Close and reopen the app while online (it loads the new version then); send and copy a clip.

**CLI**
- [ ] `yacs update`, then `yacs --version` shows the new version; `yacs send --text hi` and `yacs recv` work.

## 0.7.3

**`yacs join` on Windows** (it said "wrong code" in the 0.7.2 checks)
- [ ] Show a code on the phone and run `yacs join 12-panda-tulip` with it: it joins. Typing it at the prompt shows what you type.

**One space for the app and the command** (a Mac or Windows PC with the command installed)
- [ ] After the update, `yacs spaces` shows the app's space without installing the command again. If `yacs` was in another space before, its first command says it now uses the app's.
- [ ] Leave the space in the app, then `yacs join` with a code or link, then open Spotlight: it shows the space's clips, and new ones arrive.
- [ ] `yacs leave`, then open Settings: it shows "Welcome to YACS". Join in the app: `yacs spaces` shows it.

**Nothing in the background** (desktop, each OS)
- [ ] With Spotlight open, copy something on the phone: it shows up in Spotlight by itself.
- [ ] Close Spotlight, send a few clips from the phone, open Spotlight: they're all listed, the top one previews.
- [ ] Show an invite code or QR in Settings and use it on another device: Settings says it joined.
- [ ] Once the next release is out: opening Spotlight or Settings (an hour after the last check) offers it in Settings and the tray, with nothing checking in between.
