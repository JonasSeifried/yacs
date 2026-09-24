# YACS

**Yet Another Clipboard Service**: copy on one device, paste on another. End-to-end encrypted, self-hosted, no accounts.

- **Desktop** (macOS, Windows): press `⌘⇧Space` / `Ctrl+Shift+Space` for a Spotlight-style panel. `⌘V` sends your clipboard; `↵` copies a clip back, with every format it had (text, HTML, RTF, images).
- **Phone**: a web app served by your relay. Scan a QR code from the desktop and it's paired; add it to the home screen to use it like an app.
- **Relay**: one small server that stores encrypted clips until they expire (15 minutes by default). It can't read them.

## How it's private

Devices that share a pairing phrase derive the same key (Argon2id, then HKDF) and encrypt every clip with XChaCha20-Poly1305 before it leaves the device. The relay only sees a channel id, encrypted blobs, their sizes and their expiry times: never content, formats or device names. Clips are deleted when they expire, and nothing is kept beyond that.

The phrase (or the QR code, which carries the derived key) is the only secret. Anyone who has it can read and send your clips, so keep it to your own devices. An access token on the relay keeps strangers from storing data on it.

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
| `YACS_MAX_SIZE` | `20MB` | Largest clip. |
| `YACS_MAX_CLIPS_PER_CHANNEL` | `50` | History length; the oldest clip goes first. |
| `YACS_MAX_DISK` | `2GB` | Total storage for all channels. |
| `YACS_BIND` / `YACS_DATA_DIR` | `0.0.0.0:8080` / `./data` | Set by the Docker image to `/data`. |
| `YACS_VERSION` | `latest` | Compose only: image tag to run, e.g. `0.1.0`. |
| `YACS_PORT` | `8080` | `compose.nginx.yaml` only: the localhost port nginx proxies to. |

Without Docker: `cargo build --release -p yacs-server` (after building the web app, see below) gives a single binary; put it behind any HTTPS reverse proxy. Keep that proxy's access log off or path-free: request paths contain channel ids.

## Pairing

1. On the first computer: open YACS → Settings, enter your relay's URL and token, press **Generate** for a new phrase, then **Pair**.
2. On other computers: same, but type that phrase.
3. On a phone: on a paired computer, Settings → **Pair a phone…** and scan the code with the camera. Or open your relay's URL on the phone and type the phrase.

### Phone tips

- **iPhone:** add YACS to the home screen *before* pairing. Open your relay's URL, tap Share → **Add to Home Screen**, open YACS from there, then tap **Scan QR code**. The home screen app doesn't share storage with the browser, so pairing in a browser tab doesn't carry over.
- **Android:** use Chrome (menu → **Install app**). Firefox works too, but puts its logo on the home screen icon.
- The clipboard buttons need HTTPS, which both setups above give you.

## Updating

The desktop apps update themselves. The relay is a Docker image: in `deploy/`, run

```sh
docker compose pull && docker compose up -d
# nginx setup:
docker compose -f compose.nginx.yaml pull && docker compose -f compose.nginx.yaml up -d
```

Desktop Settings shows a hint when the relay is older than the apps. To update it automatically, point a tool like Watchtower at the relay container; it follows `latest` unless you pinned `YACS_VERSION`.

## Development

Needs Rust (stable), Node 22+, pnpm, and [`wasm-pack`](https://rustwasm.github.io/wasm-pack/) for the web app.

```sh
pnpm install
cargo run -p yacs-server -- --data-dir /tmp/yacs --bind 127.0.0.1:8080   # a local relay
pnpm desktop        # the desktop app (Tauri dev)
pnpm web            # the web app on http://localhost:1420, proxying /api to the relay
pnpm web:build      # build ui/dist/web, which yacs-server embeds in release builds
cargo test --workspace && pnpm ui:test
```

`yacs` (in `crates/yacs-cli`) sends and receives clips from a terminal: `YACS_SERVER=… YACS_PHRASE=… yacs send "hi"`.

The design, protocol and roadmap are in [PLAN.md](PLAN.md).
