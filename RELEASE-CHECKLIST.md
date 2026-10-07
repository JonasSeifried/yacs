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

## Next release (after 0.7.4)

**Stats page**
- [ ] `https://yacs-relay.jonasseifried.com/stats` on the phone and a computer: a wrong key says so, the stats key shows the numbers and charts, and reopening the page doesn't ask again.

## 0.7.4

**Linux .deb after an in-app update** (on GNOME Wayland, 0.5.3 → 0.7.3 left Spotlight unable to open until the .deb was reinstalled; cause unknown)
- [ ] With only the .deb installed, update from the previous version inside the app. Afterwards, both `yacs-desktop --toggle` (the GNOME shortcut) and the tray's "Open YACS" open Spotlight. If they don't: run `pgrep -af yacs-desktop` and `yacs-desktop --toggle` from a terminal, note the output, then quit and start YACS from the app menu and try again.

**Relay stats**
- [ ] On the server, `git pull` in `/srv/yacs` (the compose file passes `YACS_STATS_TOKEN` on now) and set the key in `deploy/.env`, then update the relay.
- [ ] `curl -H "Authorization: Bearer <stats key>" https://yacs-relay.jonasseifried.com/api/v1/stats` shows totals; without the key it's a 401.
- [ ] A minute later, `stats.json` is in the data volume: `docker run --rm -v deploy_yacs-data:/data alpine ls -l /data`.
- [ ] Uptime Kuma's Json Query monitors (README, Watching the relay) are green.

**Update offered on a window opening** (from 0.7.3)
- [ ] Opening Spotlight or Settings (an hour after the last check) offers 0.7.4 in Settings and the tray, with nothing checking in between.

**Live updates on Linux** (they never started when a window opened)
- [ ] With Spotlight open, copy something on the phone: it shows up in Spotlight by itself.
- [ ] Show an invite code in Settings and use it on another device: Settings says it joined.
