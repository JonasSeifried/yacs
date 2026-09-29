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

## 0.7.2

What the automated tests couldn't cover, or covered only in a browser. Everything not listed (the relay's disk limit, the upload retries, the damaged spaces files) is covered by tests that failed without the fix.

**CI fix**
- [ ] The `desktop (windows-latest)` and `desktop (ubuntu-22.04)` CI jobs are green again (they failed for 0.7.1).
- [ ] The CLI's `yacs join` on Windows still saves the space (its save code changed and is only compiled there): Settings → Command line → Install command, then `yacs spaces` in a new terminal.

**Relay: the backup of `accounts.json`**
- [ ] Use any space once, wait a minute, then list the data volume (its name from `docker volume ls`, e.g. `deploy_yacs-data`):
  `docker run --rm -v deploy_yacs-data:/data alpine ls -l /data`. There's an `accounts.json.bak` next to `accounts.json`.
- [ ] The logs have no `reaper can't …` warnings (they'd mean a file on the disk can't be deleted).

**Spotlight: Undo after clicking into the preview** (macOS and Windows)
- [ ] Copy something with formatting (a paragraph from a web page) on another device, so its clip has an HTML preview.
- [ ] In Spotlight, delete the clip just above the formatted one (⌘⌫ / Delete), so the formatted one is selected next. Click into its preview, then press Undo (or ⌘Z / Ctrl+Z). The deleted clip comes back and stays on the other devices.
- [ ] Delete a clip again and click into another app right away. Spotlight closes, and the clip is gone on the other devices.
- [ ] Delete a clip and wait until the notice goes away: the clip is gone on the other devices, and no Undo is left on screen.

**Phone: deletes when the app is closed** (iPhone and Android)
- [ ] Delete a clip and swipe the app away at once. On a computer, the clip is gone from Spotlight (close and reopen it if it's still listed).
- [ ] Delete a clip, switch to another app within a second or two, and come back: no "Undo" is offered any more, and the clip is gone on the other devices.
- [ ] Delete a clip and tap Undo in the app: it comes back and stays on the other devices.

**Phone: joining** (one phone, fresh: delete the home-screen app or use a private tab)
- [ ] Join from a computer's QR code or link: it works as before. (Retrying after a failed join was checked in a browser with an injected error; a real failure is hard to cause on purpose.)
