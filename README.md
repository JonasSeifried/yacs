# YACS

**Yet Another Clipboard Service**: copy on one device, paste on another. End-to-end encrypted, no accounts, free and open source.

- **On a computer:** [download YACS for macOS, Windows or Linux](https://github.com/JonasSeifried/yacs/releases/latest)
- **On a phone or tablet:** nothing to download, [open YACS in the browser](https://yacs-relay.jonasseifried.com)

- **On your computer**, press `⌘⇧Space` (Mac) or `Ctrl+Shift+Space` (Windows, Linux) for a Spotlight-style panel. `⌘V` / `Ctrl+V` sends what's on your clipboard; `↵` copies a clip back with every format it had: text, formatted text (HTML, RTF), images.
- **Files too**: copy a file and send it like anything else. It arrives in Downloads and on the clipboard.
- **On your phone or tablet**, YACS is a web app. Scan a QR code from another device and it's in; add it to your home screen to use it like any other app.
- **From a terminal**, `yacs send` and `yacs recv` do the same, for servers and scripts.

Clips go through a small server, the relay, that can't read them and forgets them when they expire (15 minutes by default). Use the free one, which the apps are set up for, or run your own in a few minutes.

## Get started

1. **Start on one device.** Install YACS on your computer from the [latest release](https://github.com/JonasSeifried/yacs/releases/latest), or open [the web app](https://yacs-relay.jonasseifried.com) on a phone or tablet. Choose **I'm new to YACS**.

   | | |
   | --- | --- |
   | macOS | `YACS_…_universal.dmg` (Apple silicon and Intel) |
   | Windows | `YACS_…_x64-setup.exe`, or the `.msi`. Windows may warn that the app is unrecognized: click **More info → Run anyway**. |
   | Linux | `YACS_…_amd64.AppImage`, or the `.deb` / `.rpm`. See the [Linux tips](#linux-tips). |

   The app updates itself from then on. This starts a space: a key that only your devices have, which encrypts every clip. It's called "My devices"; rename it whenever you like.
2. **Add your other devices.** Right after that, YACS shows a QR code and a link, each good for one device within 24 hours, and a short code like `7-tulip-apple` that works while it's on screen. Later, find them under Settings → **Invite a device**.
   - **Phone or tablet:** scan the QR code with the camera. On iPhone and iPad, add YACS to your home screen first and scan from inside it (see the [phone tips](#phone-tips)).
   - **Another computer:** install YACS there, choose **I already use YACS on another device** and type the code, or paste the link.
   - **A server:** `yacs join` with the link or code (see [Command line](#command-line)).

   A wrongly typed code is used up and the other device shows a new one, so codes can't be guessed by trying.

### Phone tips

- **iPhone and iPad:** add YACS to the home screen *before* joining. Open [yacs-relay.jonasseifried.com](https://yacs-relay.jonasseifried.com) (or your own relay) in Safari, tap Share → **Add to Home Screen**, open YACS from there, choose **I already use YACS on another device** and tap **Scan QR code**. The home screen app doesn't share storage with Safari, so joining in a browser tab doesn't carry over.
- **Android:** Chrome is recommended (menu → **Install app**).
- For big files, keep the screen on until the upload or download is done: phones pause apps in the background.

### Linux tips

- Make the `.AppImage` executable (`chmod +x`) and run it, or install the `.deb` / `.rpm`. All three update themselves; the packages ask for your password to install an update.
- The tray icon needs AppIndicator support. Ubuntu has it; on other GNOME desktops, add the "AppIndicator and KStatusNotifierItem Support" extension.
- On Wayland, apps can't set global shortcuts, so bind `yacs-desktop --toggle` (with the AppImage: its path, then `--toggle`) to a shortcut in your desktop's keyboard settings. YACS Settings shows the exact command and where to add it (GNOME, KDE, Hyprland, Sway).
- The command line isn't bundled on Linux; install it as below.

## How it's private

Every clip is encrypted on your device, with your space's key (XChaCha20-Poly1305), before it leaves. The relay stores encrypted blobs under a random channel id until they expire, then deletes them. It never sees what you copied, its format, your device names or your space's name; it does see the blobs' sizes and expiry times.

Invites are sealed the same way. The relay hands an invite to the first device that opens its link and then forgets it, without being able to open it. If someone else got there first, your device is told the invite was already used.

## The free relay

`yacs-relay.jonasseifried.com` is a relay anyone can use, run by YACS's author as a free service, with no uptime guarantee. The apps use it unless you choose your own. It sees what any relay sees (see above), plus your IP address, which it keeps in memory only, to limit new spaces and requests per address.

To keep it free and hard to abuse, spaces on it have limits:

- clips and files up to **10 MB**
- kept up to **1 hour**
- **500 MB** of transfer per space a day

The apps show these limits and only offer what fits. For bigger files and longer expiry, [run your own relay](#run-your-own-relay).

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
```

The space is saved in `~/.config/yacs/cli.json`, readable only by you; `yacs spaces` shows it, `yacs space rename` renames it and `yacs leave` forgets it. On a machine without other devices, `yacs space new` starts a space on the free relay (`--relay URL` for your own, `--account-key` if it has one) and `yacs invite` prints a link for the others, then shows a code and waits until it's typed. `yacs info` shows the space's limits. Scripts can skip the saved space and set `YACS_RELAY` and `YACS_SPACE` (from `yacs space export`) instead (`YACS_SERVER` works too).

## Run your own relay

Your own relay has no free-plan limits: files of any size (big ones go through in encrypted 4 MB chunks, so no device holds a whole file in memory), and clips kept up to a day, or longer if you allow it.

You need a machine with Docker, a domain pointing at it, and ports 80 and 443 open. Caddy gets the HTTPS certificate, which the phone app needs.

```sh
git clone https://github.com/JonasSeifried/yacs.git
cd yacs/deploy
cp .env.example .env   # set YACS_DOMAIN and YACS_ACCESS_TOKEN
docker compose up -d
```

Then open `https://your-domain` to check it's up, and press **Use my own relay** (its URL and account key) in the desktop app's Settings, under **I'm new to YACS**. The devices you invite don't need the account key.

**Already running nginx?** Use `compose.nginx.yaml` instead, which starts only the relay, on `127.0.0.1:8080`, and put [`nginx.conf`](deploy/nginx.conf) in front of it (the steps are at the top of that file; `certbot --nginx` adds HTTPS):

```sh
docker compose -f compose.nginx.yaml up -d
```

| Setting | Default | |
| --- | --- | --- |
| `YACS_ACCESS_TOKEN` | unset | The relay's account key: creating a space needs it, joining one doesn't. Set it whenever the relay is reachable from the internet. |
| `YACS_DEFAULT_TTL` | `15m` | Expiry when a client doesn't choose one. |
| `YACS_MAX_TTL` | `24h` | Longest expiry a client may choose (`7d` shows up in the apps once allowed). |
| `YACS_MAX_SIZE` | `20MB` | Largest clip sent in one piece. Bigger files go in chunks, which only `YACS_MAX_DISK` limits. |
| `YACS_MAX_CLIPS_PER_CHANNEL` | `50` | History length; the oldest clip goes first. |
| `YACS_MAX_DISK` | `25GB` | Total storage for all channels. An upload counts in full from its start. |
| `YACS_BIND` / `YACS_DATA_DIR` | `0.0.0.0:8080` / `./data` | Set by the Docker image to `/data`. |
| `YACS_VERSION` | `latest` | Compose only: image tag to run, e.g. `0.5.2`. |
| `YACS_PORT` | `8080` | `compose.nginx.yaml` only: the localhost port nginx proxies to. |

**A public relay** like the free one: set `YACS_PUBLIC=true`, and anyone may create spaces on a free plan (`YACS_FREE_MAX_SIZE` `10MB` per clip, `YACS_FREE_MAX_TTL` `1h`, `YACS_FREE_DAILY_TRANSFER` `500MB` per space), with limits per IP address (`YACS_NEW_SPACES_PER_IP` `10` a day, `YACS_REQUESTS_PER_MINUTE` `600`). Spaces created with `YACS_ACCESS_TOKEN` keep the limits in the table. The reverse proxy must set `X-Forwarded-For` to the client's address, as both setups here do. Point `YACS_PRIVACY_URL` and `YACS_IMPRINT_URL` at your privacy policy and imprint; the phone app links them.

Without Docker: `cargo build --release -p yacs-server` (after building the web app, see [Development](#development)) gives a single binary; put it behind any HTTPS reverse proxy. Keep that proxy's access log off or path-free: request paths contain channel ids. The proxy must accept request bodies above `YACS_MAX_SIZE`, and at least 5 MB for the chunks of big files (nginx: `client_max_body_size 25m`; its default of 1 MB is too small). Cloudflare's limits are fine.

### Updating the relay

The relay is a Docker image, `ghcr.io/jonasseifried/yacs`. With `yacs` on the relay's machine, `yacs relay update` finds the relay container, pulls the new image and restarts it with the compose files it was started with (it needs to be allowed to use Docker, so maybe `sudo`). `yacs update` mentions it when the relay is behind, and desktop Settings shows a hint when the relay is older than the apps. By hand, in `deploy/`:

```sh
docker compose pull && docker compose up -d
# nginx setup:
docker compose -f compose.nginx.yaml pull && docker compose -f compose.nginx.yaml up -d
```

To update it automatically, point a tool like Watchtower at the relay container; it follows `latest` unless you pinned `YACS_VERSION`.

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

The design, protocol and roadmap are in [PLAN.md](PLAN.md). Bug reports and ideas are welcome in [issues](https://github.com/JonasSeifried/yacs/issues).

## License

[AGPL-3.0](LICENSE). Use it and self-host it freely, for yourself or your company. If you change YACS and let others use your version, including as a hosted relay, you have to publish your changes under the same license.
