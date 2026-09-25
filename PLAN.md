# YACS: Implementation Plan

Self-hostable, end-to-end encrypted clipboard sync. Rust monorepo: Axum relay, Tauri v2 desktop, PWA for mobile (v1), native Tauri mobile (v2).

## 1. Decisions

| Topic | Decision |
| --- | --- |
| Desktop v1 | macOS + Windows. Linux since 0.2.3 (AppImage, .deb, .rpm; on Wayland, bind `yacs-desktop --toggle` since global hotkeys are blocked there). |
| Mobile | v1: PWA served by the relay. v2: Tauri mobile app with native clipboard plugins and share extensions. Both share one UI codebase. |
| Pairing | Shared phrase, focus on self-hosting. Low-cost hardening: the app generates the phrase by default, and Argon2id is used for key derivation. Accounts only if a public hosted version happens later. |
| Hotkey UX | `CommandOrControl+Shift+Space` (⌘⇧Space on macOS, Ctrl+Shift+Space on Windows) opens Spotlight with a preview of the latest remote clip. `Ctrl+C` copies the preview to the local clipboard. `Ctrl+V` sends the local clipboard. `Esc` or losing focus closes it. |
| Server model | Short history: each channel keeps every clip until its TTL expires (capped per channel). Reading doesn't delete, so 3+ devices work. |
| TTL | Chosen per clip by the sender from a UI dropdown, default **15 min**. The server clamps it to a configurable maximum (default **24 h**). |
| Frontend | React + TypeScript + Vite. |

## 2. Repository layout

```
yacs/
├─ Cargo.toml                  # workspace
├─ crates/
│  ├─ yacs-core/               # protocol types + crypto. No OS/tokio deps, must build for wasm32
│  ├─ yacs-client/             # HTTP client (reqwest), used by desktop + CLI (+ Tauri mobile in v2)
│  ├─ yacs-server/             # Axum relay, also serves the PWA
│  ├─ yacs-cli/                # `yacs`: servers and scripts (`yacs pair`, `yacs send FILE`, `yacs relay update`), Wayland fallback later
│  └─ yacs-wasm/               # wasm-bindgen wrapper around yacs-core for the PWA
├─ apps/
│  └─ desktop/src-tauri/       # Tauri v2 Rust side (workspace member)
├─ ui/                         # shared React + TS frontend (Vite)
│  ├─ src/spotlight/           # desktop Spotlight window
│  ├─ src/mobile/              # PWA now, Tauri mobile later
│  └─ src/platform/            # adapter: tauri.ts | web.ts (same interface)
└─ deploy/                     # Dockerfile, docker-compose + Caddy example
```

The `platform` adapter is how the PWA later becomes a native app without rewriting the UI:

```ts
interface Platform {
  pair(phrase: string, server: string): Promise<void>;
  serverConfig(): Promise<ServerConfig>;           // default/max TTL, max size
  listClips(): Promise<ClipMeta[]>;                // newest first
  getClip(id: ClipId): Promise<Clip>;              // decrypted
  sendClipboard(ttlSecs: number): Promise<void>;   // desktop: Rust reads native clipboard
  sendItems(items: ClipItem[], ttlSecs: number): Promise<void>; // mobile: pasted / picked content
  copyToClipboard(id: ClipId): Promise<void>;
  deleteClip(id: ClipId): Promise<void>;
}
```

- `tauri.ts` calls Rust commands, and Rust does the clipboard, crypto and network work via `yacs-core` + `yacs-client`.
- `web.ts` uses the WASM build of `yacs-core` + `fetch` + the Async Clipboard API.

## 3. Protocol & crypto (`yacs-core`)

### Key derivation
```
phrase ── normalize (NFKC, lowercase, collapse whitespace)
       ── Argon2id(m=64 MiB, t=3, p=1, salt="yacs/v1/argon2id") → root (32 B)
root   ── HKDF-SHA256(info="yacs/v1/channel")                  → channel_id (32 B, base64url)
root   ── HKDF-SHA256(info="yacs/v1/key")                      → enc_key (32 B)
```
- The salt is fixed on purpose: both devices must derive the same values without talking to each other.
- The key is derived once at pairing time and stored, so the Argon2 cost is paid only once: `pairing.json` (mode 0600) in the desktop app's config dir, `cli.json` for `yacs`, `localStorage` in the PWA. Not the OS keychain: once the app pairs the `yacs` command, the key is in a file anyway; on Linux a keyring isn't always running; and any program running as the user can read the clipboard directly. Versions up to 0.2.3 used the keychain; the app moves such pairings to the file on start, which is all the `keyring` crate is still there for.
- Parameters are versioned (`v1`). Every client must use exactly the same parameters, so they are enforced by shared test vectors.
- The default phrase is generated (6 words from the EFF wordlist, about 77 bits). Users can still type their own.

### Envelope
```rust
// plaintext (serialized with postcard, binary, no base64 bloat)
enum Payload {
    Clip(Clip),
    // v2: P2pOffer { .. }  (large-file transfer handshake)
}
struct Clip {
    created_at_ms: i64,             // set by the sender (no clock in WASM core)
    device_name: String,
    items: Vec<ClipItem>,          // all formats of one copy action
}
enum ClipItem {
    Text(String),
    Html(String),
    Rtf(String),
    Image { mime: String, data: Vec<u8> },   // normalized to PNG
    // later: Files(..)
}

// wire format (what the server stores)
struct Envelope { version: u8, nonce: [u8; 24], ciphertext: Vec<u8> }
```
- Cipher: **XChaCha20-Poly1305**. The 24-byte random nonce is safe to generate randomly, and the implementation is pure Rust, so it builds for WASM. AAD = `version || channel_id`.
- The payload type sits **inside** the ciphertext, so the server only sees opaque bytes and their size. V2 signaling gets its own endpoint and does not need a visible `type` field.
- Default max size is 20 MB and configurable.

## 4. Relay server (`yacs-server`)

```
GET    /api/v1/config                                 → { default_ttl_secs, max_ttl_secs, max_size_bytes, max_clips, version }
POST   /api/v1/channels/{channel_id}/clips?ttl=900    body: octet-stream Envelope
                                                      → 201 { id, created_at_ms, expires_at_ms, size }  (ttl clamped to max_ttl)
GET    /api/v1/channels/{channel_id}/clips            → [{ id, created_at_ms, expires_at_ms, size }]  newest first
GET    /api/v1/channels/{channel_id}/clips/latest     → 200 Envelope | 404   (ETag = clip id, supports 304)
GET    /api/v1/channels/{channel_id}/clips/{id}       → 200 Envelope | 404   (immutable, cacheable)
DELETE /api/v1/channels/{channel_id}/clips/{id}
DELETE /api/v1/channels/{channel_id}/clips            clear the whole history
GET    /api/v1/channels/{channel_id}/events           SSE: { type: added | deleted | cleared, … } as clips change
GET    /healthz
GET    /*                                             embedded PWA (rust-embed)
```

- **Clip IDs** are ULIDs assigned by the server. They sort by time, so "newest first" is just a reverse sort of the directory listing.
- **History = everything not yet expired.** A per-channel cap (`max_clips`) evicts the oldest clip first, so a busy channel can't grow without limit.
- **Envelope responses** carry the metadata in `x-yacs-clip-id`, `x-yacs-created-at` and `x-yacs-expires-at` headers.
- **Storage:** `data/{hex channel_id}/{ulid}.{expires_at_ms}.bin`. Hex, not base64url, so two ids can't collide on case-insensitive filesystems (macOS, Windows). Because the expiry is in the filename, the reaper never has to open a file. Writes go to a temp file first and are then renamed, so a clip is never half-written. The reaper runs on a `tokio::time::interval` (60 s) and deletes expired files and empty channel dirs.
- **The server sees** each clip's size, creation time and expiry. Contents, formats and device names stay encrypted.
- **Limits:** `DefaultBodyLimit`, `max_clips` per channel, total disk quota. No rate limiting: the access token keeps strangers out, the quotas bound disk use, and behind a reverse proxy per-IP limits would lump all clients together. If wanted, rate-limit in Caddy/Traefik. Expired-but-not-yet-reaped clips don't hold a history slot.
- **Live updates:** `/events` is a server-sent event stream per channel (a `tokio::sync::broadcast` per listened-to channel, pruned by the reaper). Events carry only what the relay already knows (clip metadata, deleted ids); clients re-list after (re)connecting, since events sent while they were away are gone. A keep-alive comment every 20 s keeps proxies (nginx drops quiet upstreams after 60 s) from cutting the stream, and lets clients spot a dead connection after 60 s of silence; `X-Accel-Buffering: no` stops nginx from buffering it. Streams end on shutdown, so a restart doesn't wait on them. The desktop listens in the background while paired and prefetches new clips up to 4 MB, so Spotlight opens with them decrypted; the PWA listens while it's on screen and polls every 10 s only without a connection (e.g. relays before 0.2.0, which answer 404).
- **Version:** `/config` reports the relay's version. The desktop, which updates itself, shows it in Settings with a hint when the relay is older than the newest release.
- **Logging** shows the route pattern (`/api/v1/channels/{channel}/clips`), never the URI, because the URI contains the channel id.
- **Optional access token:** set `YACS_ACCESS_TOKEN` and clients must send `Authorization: Bearer …`. This keeps strangers from filling your disk when the server is reachable from the internet. The server logs a warning at startup when it's unset.
- **Config:** env vars / `clap` flags. Durations are parsed with `humantime` (`15m`, `24h`, `7d`).

  | Setting | Default |
  | --- | --- |
  | `YACS_BIND` | `0.0.0.0:8080` |
  | `YACS_DATA_DIR` | `./data` |
  | `YACS_DEFAULT_TTL` | `15m` |
  | `YACS_MAX_TTL` | `24h` |
  | `YACS_MAX_SIZE` | `20MB` |
  | `YACS_MAX_CLIPS_PER_CHANNEL` | `50` |
  | `YACS_MAX_DISK` | `2GB` |
  | `YACS_ACCESS_TOKEN` | unset (open) |
- **TLS:** terminate at a reverse proxy. `deploy/compose.yaml` runs the relay behind Caddy, which gets the certificate; its Caddyfile keeps the access log off because paths contain channel ids.
- **No CORS needed.** The PWA is served from the same origin, and desktop makes its requests from Rust.
- **Container:** `deploy/Dockerfile`: WASM (wasm-pack) → PWA (Vite) → static musl relay with the PWA embedded → `distroless/static:nonroot`, about 14 MB.

## 5. Desktop (Tauri v2, macOS + Windows)

**Plugins & crates:** `tauri-plugin-global-shortcut`, `-single-instance`, `-autostart`, `-store` (settings), `-updater`, built-in tray, **`clipboard-rs`**.

- **Why `clipboard-rs` instead of the Tauri clipboard plugin:** the plugin can't *read* HTML/RTF. `clipboard-rs` reads and writes text, HTML, RTF and images on macOS and Windows.
- **Why Rust handles `Ctrl+V` instead of the DOM `paste` event:** the webview paste event hides RTF and behaves differently in WKWebView and WebView2.

**Spotlight window:** `decorations: false`, `transparent`, `alwaysOnTop`, `skipTaskbar`, hidden at start, hides when it loses focus. macOS: `ActivationPolicy::Accessory` (no Dock icon). Consider `tauri-nspanel` so it also shows over fullscreen apps. After hiding, focus goes back to the previous app.

**Spotlight layout**
```
┌───────────────────────────────────────────────────────────┐
│ YACS  clip.example.com                                 ⚙  │
├──────────────────────┬────────────────────────────────────┤
│▸ Meeting notes for…  │  preview of the selected clip      │
│  MacBook · 2 min ago │  (sanitized HTML, text or image)   │
│  Image · 1280×720    │                                    │
│  PC · 11 min ago     │                                    │
│  https://github.com… │  expires in 13 min · 2.1 KB · text │
├──────────────────────┴────────────────────────────────────┤
│ ↵ copy · ⌘⌫ delete · ⌘V send clipboard   expires in [15m] │
└───────────────────────────────────────────────────────────┘
```

**Flow**
1. Hotkey pressed: show the window and at the same time `list_clips`. The newest clip, every clip up to 256 KB, and whichever clip you select are fetched and decrypted, so rows show a title and device. Rust caches decrypted clips in memory by ID (clips never change); entries go when the relay stops listing them, and the oldest go once the cache holds 256 MB.
2. `↑/↓`: select a clip. `Enter` or `Ctrl+C`: Rust writes all formats of the selected clip to the clipboard, then the window hides and focus returns to the previous app. Double-click does the same.
3. `Ctrl+V`: the keydown is intercepted. Rust reads the native clipboard (text, HTML, RTF, image; PNG is passed through untouched), encrypts, POSTs with the TTL from the dropdown, shows "Sent" with the new clip selected, then the window hides after 5 s (`Esc` hides at once, any other key keeps it open). Pressing `Ctrl+V` again in those 5 s hides instead of sending the same clipboard twice. A single copied image file (Finder/Explorer) is sent as that image: PNG, JPEG, GIF and WebP as they are, BMP and TIFF converted to PNG, up to 64 MB. Other files are refused with a clear message until P2P in v2.
4. `Del` (macOS: `⌘⌫`, like Finder, so a stray Backspace can't delete): removes the selected clip from the server for every device.
5. The preview is sanitized: HTML goes through DOMPurify (never rendered if DOMPurify reports it can't run) into an `<iframe sandbox srcdoc>` whose own CSP blocks every network request, so remote images can't signal that a clip was viewed. Images arrive as binary IPC and are shown as blob URLs. Text previews are capped at 20k characters and HTML at 512 KB; copy always writes the full clip.

**Windows clipboard detail:** `clipboard-rs` empties the clipboard when it sets an image, so an image on its own goes through its image path (PNG plus a bitmap for Paint and older apps), and an image next to text is added as raw `PNG` data, which rich-text apps read.

**TTL dropdown:** `5m · 15m (default) · 1h · 8h · 24h · 7d`. Options above the server's `max_ttl` (from `/api/v1/config`) are hidden. The last choice becomes the device's default expiry (the same setting as in Settings), and `Tab` / `Shift+Tab` cycle through the options so you never need the mouse.

**Pairing / settings:** first run opens Settings: relay URL, optional access token, and the phrase (a **Generate** button fills in 6 EFF words; on other devices you type it). Pairing derives the key, checks the relay and token, and only then stores anything. Settings also hold device name, hotkey (recorded by pressing it), default expiry and launch at login. *Pair another device…* shows a QR code for the PWA and the same link to copy: another computer takes it pasted into the pair form, a server into `yacs pair`.

**Tray & lifecycle:** menu bar/tray icon with Open, Settings and Quit. Closing windows never quits. A second launch opens Settings, and `yacs-desktop --toggle` toggles Spotlight in the running instance, so Linux/Wayland users can bind that command to a desktop shortcut. On Wayland the app doesn't register its hotkey, and Settings shows that command (the AppImage's path, or `yacs-desktop` from the packages) with steps for the detected desktop instead of the shortcut recorder.

**Distribution:** `.github/workflows/release.yml` runs on a `v*` tag: `tauri-action` builds a macOS universal app and Windows MSI/NSIS into a draft release, the relay image goes to `ghcr.io/jonasseifried/yacs` (amd64 + arm64), and the release is published once all of that succeeded (tags like `v1.1.0-beta.1` become prereleases, which the updater skips). macOS is signed and notarized, and Windows is signed via Azure Trusted Signing, when those secrets exist; otherwise the builds are unsigned (Gatekeeper / SmartScreen warn).

**Updates:** `tauri-plugin-updater` against `releases/latest/download/latest.json`. Updates must be signed with the release key (`plugins.updater.pubkey`), and `requireSignedVersion` blocks downgrades to older signed releases. Release builds check 30 s after launch and every 12 h. An available update shows in the tray menu and in Settings, and installs only when the user clicks it. Updater artifacts are enabled only in CI, where the private key is.

## 6. Mobile

### v1: PWA (served by the relay)
- **Pairing via QR:** desktop Settings → *Pair another device* shows `https://your-server/#pair=v1.<channel id>.<key>[&token=…]`. It carries the *derived* pairing (`Pairing::to_secret` in `yacs-core`), not the phrase: the desktop never stores the phrase, and the phone skips Argon2id. Everything is in the URL fragment, which browsers never send, so it never reaches the server or its logs; the app removes it from the address bar as soon as it's read. The PWA can also scan the code itself (BarcodeDetector, or jsQR loaded on demand), which iOS needs: a home screen app has its own storage, so a pairing made in the browser tab doesn't carry over, and the camera app only opens the browser. Typing the phrase works too (Argon2id in WASM, about a second on a phone).
- **Storage:** the pairing lives in `localStorage`. The page's CSP only runs scripts from the relay itself, so no third-party script can read it.
- **HTTPS required** for the clipboard API, the service worker and installing. Over plain `http://` (except `localhost`) you can still type, pick images and receive; the desktop's QR dialog warns about it.
- **Receive:** opening the app shows the history list (newest first) with the newest clip expanded; it refreshes when the app comes to the foreground and every 10 s while open. Each clip has a **Copy** button that calls `navigator.clipboard.write([new ClipboardItem({ "text/plain", "text/html", "image/png" })])` straight from the tap (Safari only allows clipboard writes inside the gesture). RTF is dropped, since browsers can't write it. Images also get a **Save/Share** button (Web Share API, download as fallback).
- **Send:** a Paste button (`navigator.clipboard.read()`; iOS shows its own "Paste" confirmation), a text area, or an image picker, plus the same TTL dropdown as desktop. Android: Web Share Target, so "Share → YACS" works from other apps: the service worker takes the POST, holds the content in a cache just long enough for the page to pick it up, and deletes it.
- **Service worker:** caches only the app shell (hashed assets forever, the page network-first). It never touches `/api`, so nothing decrypted is ever cached.
- **Build:** `yacs-wasm` via `wasm-pack` (`pnpm --filter @yacs/ui wasm`), then `vite build --mode web` into `ui/dist/web`, which `yacs-server` embeds with `rust-embed` (read from disk in debug builds). The desktop build is `--mode desktop` and doesn't need WASM.

### v2: Tauri mobile
Same `ui/src/mobile` with the `tauri.ts` adapter. Rust reuses `yacs-core` + `yacs-client` directly (no WASM). Tauri's clipboard plugin is text-only on mobile, so rich formats need a small custom plugin: Swift `UIPasteboard` / Kotlin `ClipboardManager`. Adds an iOS Share Extension + Android share intent.

| Capability | PWA | Tauri mobile + native plugin |
| --- | --- | --- |
| Plain text copy | ✅ | ✅ |
| Rich text copy | ⚠️ `text/html` only; whether the target app accepts it varies | ✅ HTML + RTF |
| Image copy | ✅ PNG | ✅ |
| Files | ❌ clipboard, ✅ via share sheet/download | ✅ share sheet / Files app |
| Send from other apps | Android only | ✅ iOS + Android |
| Background sync / notifications | ❌ (iOS) | ✅ push possible |
| Distribution | a URL | store review |

## 7. Roadmap

This reorders the original roadmap: crypto and the protocol come first, so the UI is built on a finished, tested core and nothing has to be retrofitted later.

| Phase | Focus | Deliverables | Done when |
| --- | --- | --- | --- |
| **0: Core** ✅ | `yacs-core` | Workspace, KDF, envelope, cipher, postcard serialization, test vectors, CI job checking the `wasm32` build | Round-trip + vector tests pass on native and wasm |
| **1: Server + CLI** ✅ | `yacs-server`, `yacs-client`, `yacs-cli` | Routes, per-clip TTL + clamping, history + per-channel cap, disk store, reaper, limits, token, config; `yacs send --ttl 1h` / `yacs list` / `yacs recv [id]` for text + images | Two terminals sync clips end-to-end through a local server; expiry and eviction covered by integration tests |
| **2: Desktop shell** ✅ | Tauri | Tray, hidden Spotlight window, global hotkey, single instance, autostart, pairing + settings UI (incl. 6-word EFF phrase generator in `yacs-core`), keyring | Hotkey opens/closes Spotlight reliably on macOS + Windows |
| **3: Desktop clipboard** ✅ (verified Mac ↔ PC) | Core UX | `clipboard-rs` multi-format read/write, history list + keyboard navigation, Ctrl+C / Ctrl+V / Del, TTL dropdown, sanitized preview | Rich text from Word/browser and screenshots round-trip between Mac and PC |
| **4: PWA + release (v1.0)** ✅ (v0.1.1: phones pair via QR over HTTPS, relay on a VPS behind nginx, desktop self-update verified) | Mobile + ship | `yacs-wasm`, mobile UI, embedded PWA, QR pairing, Dockerfile + compose (Caddy or nginx), signed desktop builds, updater | A phone can pair via QR and copy/send; `docker compose up` works on a VPS |
| **5: v1.x** | Breadth | ✅ SSE live updates (0.2.0) and the relay version in desktop Settings; ✅ CLI for servers: `yacs pair` saves the pairing (from the desktop's link or the phrase), `yacs send FILE`, static release binaries, `yacs update` (signed), `yacs relay update` (Docker compose); the desktop apps bundle `yacs` and put it on the PATH on request (macOS, Windows); ✅ Linux desktop (0.2.3) | |
| **6: v2.0** | Native + big files | Tauri mobile with native clipboard plugins + share extensions; P2P large-file transfer | |

### Note on v2 P2P
If mobile goes native in v2, consider **iroh** (Rust, QUIC, hole punching, self-hostable relay fallback) instead of raw WebRTC. WebRTC is mainly worth it when a *browser* has to take part (i.e. the PWA receiving big files). Also: WebRTC needs a TURN fallback when hole punching fails, and that puts bandwidth back on the server. Decide at the start of v2. The `Payload` enum already has room for it.

## 8. Testing
- **core:** unit tests + fixed test vectors (phrase → channel_id/key/ciphertext), run against both the native and WASM builds so desktop and PWA can never derive different keys.
- **server:** Axum integration tests with `tempfile` data dirs and a fake clock: TTL clamping, expiry, oldest-first eviction at `max_clips`, list ordering.
- **e2e:** the CLI sends and receives against a real server in CI.
- **manual matrix (Phase 3):** Word, Google Docs, Safari/Chrome, Notes, screenshots, on macOS and Windows.

## 9. Open questions
None right now. Settled: `YACS_MAX_CLIPS_PER_CHANNEL=50`, with a disk quota covering the worst case, and `7d` stays in the dropdown but only shows when a self-hoster raises `YACS_MAX_TTL`.
