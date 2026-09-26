# YACS

**Yet Another Clipboard Service**: copy on one device, paste on another. End-to-end encrypted, self-hosted, no accounts.

- **Desktop** (macOS, Windows, Linux): press `⌘⇧Space` / `Ctrl+Shift+Space` for a Spotlight-style panel. `⌘V` sends your clipboard; `↵` copies a clip back, with every format it had (text, HTML, RTF, images). Copied files are sent too, of any size, and arrive in Downloads and on the clipboard.
- **Phone**: a web app served by your relay. Scan a QR code from the desktop and it's in; add it to the home screen to use it like an app.
- **Relay**: one small server that stores encrypted clips until they expire (15 minutes by default). It can't read them.

Big files (a video, a disk image) go through the relay in encrypted 4 MB chunks, so no device ever holds a whole file in memory. The phone app shows progress; keep its screen on until an upload or download is done, since phones pause apps in the background.

## How it's private

Your devices share a **space**: a random key, made on the first device, that every clip is encrypted with (XChaCha20-Poly1305) before it leaves the device. The relay only sees the space's channel id, encrypted blobs, their sizes and their expiry times: never content, formats, device names or the space's name. Clips are deleted when they expire, and nothing is kept beyond that.

The invite link (or its QR code) carries the key, and it's the only secret. Anyone who has it can read and send your clips, so only use it for your own devices. An access token on the relay keeps strangers from storing data on it.

## Self-hosting the relay

You need a machine with Docker, a domain pointing at it, and ports 80 and 443 open. Caddy gets the HTTPS certificate, which the phone app needs.

```sh
git clone https://github.com/JonasSeifried/yacs.git
cd yacs/deploy
cp .env.example .env   # set YACS_DOMAIN and YACS_ACCESS_TOKEN
docker compose up -d
```

Then open `https://your-domain` to check it's up.

**Already running nginx?** Use `compose.nginx.yaml` instead, which starts only the relay, on `127.0.0.1:8080`, and put [`nginx.conf`](deploy/nginx.conf) in front of it (the steps are at the top of that file; `certbot --nginx` adds HTTPS):

```sh
docker compose -f compose.nginx.yaml up -d
```

| Setting | Default | |
| --- | --- | --- |
| `YACS_ACCESS_TOKEN` | unset | Clients must send it. Set it whenever the relay is reachable from the internet. |
| `YACS_DEFAULT_TTL` | `15m` | Expiry when a client doesn't choose one. |
| `YACS_MAX_TTL` | `24h` | Longest expiry a client may choose (`7d` shows up in the apps once allowed). |
| `YACS_MAX_SIZE` | `20MB` | Largest clip sent in one piece. Bigger files go in chunks, which only `YACS_MAX_DISK` limits. |
| `YACS_MAX_CLIPS_PER_CHANNEL` | `50` | History length; the oldest clip goes first. |
| `YACS_MAX_DISK` | `25GB` | Total storage for all channels. An upload counts in full from its start. |
| `YACS_BIND` / `YACS_DATA_DIR` | `0.0.0.0:8080` / `./data` | Set by the Docker image to `/data`. |
| `YACS_VERSION` | `latest` | Compose only: image tag to run, e.g. `0.1.0`. |
| `YACS_PORT` | `8080` | `compose.nginx.yaml` only: the localhost port nginx proxies to. |

Without Docker: `cargo build --release -p yacs-server` (after building the web app, see below) gives a single binary; put it behind any HTTPS reverse proxy. Keep that proxy's access log off or path-free: request paths contain channel ids. The proxy must accept request bodies above `YACS_MAX_SIZE`, and at least 5 MB for the chunks of big files (nginx: `client_max_body_size 25m`; its default of 1 MB is too small). Cloudflare's limits are fine.

## Adding your devices

1. On the first computer: open YACS → Settings, enter your relay's URL and token under **Start a new space**, and press it. The space is called "My devices"; rename it there whenever you like (each device keeps its own name for it).
2. Everything else: on a computer in the space, Settings → **Invite a device…** shows a QR code and a link.
   - Phone: scan the code with the camera (on iPhone, see the tips below), or paste the link into the app.
   - Another computer: **Copy link** and paste it into that computer's **Invite link** field.
   - Server: paste the link into `yacs join` (see [Command line](#command-line)).

### Phone tips

- **iPhone:** add YACS to the home screen *before* joining. Open your relay's URL, tap Share → **Add to Home Screen**, open YACS from there, then tap **Scan QR code**. The home screen app doesn't share storage with the browser, so joining in a browser tab doesn't carry over.
- **Android:** Chrome is recommended (menu → **Install app**).
- The clipboard buttons need HTTPS, which both setups above give you.

### Linux tips

- Take the `.AppImage` (`chmod +x` it, then run it) or the `.deb` / `.rpm`. All three update themselves; the packages ask for your password to install an update.
- The tray icon needs AppIndicator support. Ubuntu has it; on other GNOME desktops, add the "AppIndicator and KStatusNotifierItem Support" extension.
- On Wayland, apps can't set global shortcuts, so bind `yacs-desktop --toggle` (with the AppImage: its path, then `--toggle`) to a shortcut in your desktop's keyboard settings. YACS Settings shows the exact command and where to add it (GNOME, KDE, Hyprland, Sway).
- The command line isn't bundled on Linux; install it as below.

## Command line

`yacs` sends and receives clips from a terminal, e.g. on a server with no clipboard of its own. On Linux:

```sh
mkdir -p ~/.local/bin
curl -fsSLo ~/.local/bin/yacs https://github.com/JonasSeifried/yacs/releases/latest/download/yacs-cli-linux-$(uname -m)
chmod +x ~/.local/bin/yacs
yacs join   # paste the link from a computer in the space: Settings → Invite a device… → Copy link
```

On a Mac or Windows PC with the desktop app, use Settings → Command line → **Install command** instead: it joins the computer's space and updates along with the app. (`yacs-cli-macos` and `yacs-cli-windows-x86_64.exe` are also on the [releases page](https://github.com/JonasSeifried/yacs/releases).) Once it's in a space:

```sh
yacs send ~/.ssh/id_ed25519.pub   # text files arrive as text, images as images
yacs send report.pdf              # other files as files (--as-file for any file)
yacs send disk.iso                # big files too, in chunks, with a progress line
some-command | yacs send          # or send stdin; the final newline is dropped
yacs send --text "hello"
yacs recv > clip.txt              # the newest clip; `yacs list` shows the others
yacs recv -o ~/Downloads          # a file clip, under its own name
yacs update                       # the newest version, checked against the release signature
yacs relay update                 # on the relay's machine: pull the new relay image and restart it
```

The space is saved in `~/.config/yacs/cli.json`, readable only by you; `yacs spaces` shows it, `yacs space rename` renames it and `yacs leave` forgets it. On a machine without other devices, `yacs space new --server URL` starts a space and `yacs invite` prints a link for the others. Scripts can skip the saved space and set `YACS_SERVER`, `YACS_TOKEN` and `YACS_SPACE` (from `yacs space export`) instead.

## Updating

The desktop apps update themselves, and `yacs update` updates the command line (from 0.2.1, run the `curl` line once more).

The relay is a Docker image. With `yacs` on the relay's machine, `yacs relay update` finds the relay container, pulls the new image and restarts it with the compose files it was started with (it needs to be allowed to use Docker, so maybe `sudo`). `yacs update` mentions it when the relay is behind. By hand, in `deploy/`:

```sh
docker compose pull && docker compose up -d
# nginx setup:
docker compose -f compose.nginx.yaml pull && docker compose -f compose.nginx.yaml up -d
```

Desktop Settings shows a hint when the relay is older than the apps. To update it automatically, point a tool like Watchtower at the relay container; it follows `latest` unless you pinned `YACS_VERSION`.

## Development

Needs Rust (stable), Node 22+, pnpm, and [`wasm-pack`](https://rustwasm.github.io/wasm-pack/) for the web app. The desktop app on Linux also needs [Tauri's system libraries](https://v2.tauri.app/start/prerequisites/#linux) and `libdbus-1-dev`.

```sh
pnpm install
cargo run -p yacs-server -- --data-dir /tmp/yacs --bind 127.0.0.1:8080   # a local relay
pnpm desktop        # the desktop app (Tauri dev)
pnpm web            # the web app on http://localhost:1420, proxying /api to the relay
pnpm web:build      # build ui/dist/web, which yacs-server embeds in release builds
cargo run -p yacs-cli -- send --text hi   # the command-line client
cargo test --workspace && pnpm ui:test
```

The design, protocol and roadmap are in [PLAN.md](PLAN.md).

## License

[AGPL-3.0](LICENSE). Use it and self-host it freely, for yourself or your company. If you change YACS and let others use your version, including as a hosted relay, you have to publish your changes under the same license.
