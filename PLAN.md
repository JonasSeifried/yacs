# YACS: Implementation Plan

Self-hostable, end-to-end encrypted clipboard sync. Rust monorepo: Axum relay, Tauri v2 desktop, PWA for mobile, native Tauri mobile later.

## 1. Decisions

| Topic | Decision |
| --- | --- |
| Desktop v1 | macOS + Windows. Linux since 0.2.3 (AppImage, .deb, .rpm; on Wayland, bind `yacs-desktop --toggle` since global hotkeys are blocked there). |
| Mobile | Now: PWA served by the relay. Later: Tauri mobile app with native clipboard plugins and share extensions. Both share one UI codebase. |
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
│  ├─ yacs-client/             # HTTP client (reqwest), used by desktop + CLI (+ Tauri mobile later)
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
- The key is derived once at pairing time and stored, so the Argon2 cost is paid only once: `pairing.json` (mode 0600) in the desktop app's config dir, `cli.json` for `yacs`, `localStorage` in the PWA. Not the OS keychain: once the app pairs the `yacs` command, the key is in a file anyway; on Linux a keyring isn't always running; and any program running as the user can read the clipboard directly. Versions up to 0.2.3 used the keychain, and 0.2.4 moved such pairings to the file; after that, devices still on 0.2.3 have to pair again.
- Parameters are versioned (`v1`). Every client must use exactly the same parameters, so they are enforced by shared test vectors.
- The default phrase is generated (6 words from the EFF wordlist, about 77 bits). Users can still type their own.

### Envelope
```rust
// plaintext (serialized with postcard, binary, no base64 bloat)
enum Payload {
    Clip(Clip),
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
    File { name: String, mime: String, data: Vec<u8> },  // one per copied file
}

// wire format (what the server stores)
struct Envelope { version: u8, nonce: [u8; 24], ciphertext: Vec<u8> }
```
- Cipher: **XChaCha20-Poly1305**. The 24-byte random nonce is safe to generate randomly, and the implementation is pure Rust, so it builds for WASM. AAD = `version || channel_id`.
- The payload type sits **inside** the ciphertext, so the server only sees opaque bytes and their size. New kinds of payload never need a visible `type` field.
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
…      /api/v1/channels/{channel_id}/uploads…         chunked uploads of big files, see section 7
GET    /healthz
GET    /*                                             embedded PWA (rust-embed)
```

- **Clip IDs** are ULIDs assigned by the server. They sort by time, so "newest first" is just a reverse sort of the directory listing.
- **History = everything not yet expired.** A per-channel cap (`max_clips`) evicts the oldest clip first, so a busy channel can't grow without limit.
- **Envelope responses** carry the metadata in `x-yacs-clip-id`, `x-yacs-created-at` and `x-yacs-expires-at` headers.
- **Storage:** `data/{hex channel_id}/{ulid}.{expires_at_ms}.bin`. Hex, not base64url, so two ids can't collide on case-insensitive filesystems (macOS, Windows). Because the expiry is in the filename, the reaper never has to open a file. Writes go to a temp file first and are then renamed, so a clip is never half-written. The reaper runs on a `tokio::time::interval` (60 s) and deletes expired files and empty channel dirs.
- **The server sees** each clip's size, creation time and expiry. Contents, formats and device names stay encrypted.
- **Limits:** `DefaultBodyLimit` (`YACS_MAX_SIZE`, for single envelopes), `max_clips` per channel, total disk quota (chunked uploads count in full from their start, see section 7). No rate limiting: the access token keeps strangers out, the quotas bound disk use, and behind a reverse proxy per-IP limits would lump all clients together. If wanted, rate-limit in Caddy/Traefik. Expired-but-not-yet-reaped clips don't hold a history slot.
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
  | `YACS_MAX_DISK` | `25GB` |
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
3. `Ctrl+V`: the keydown is intercepted. Rust reads the native clipboard (text, HTML, RTF, image; PNG is passed through untouched), encrypts, POSTs with the TTL from the dropdown, shows "Sent" with the new clip selected, then the window hides after 5 s (`Esc` hides at once, any other key keeps it open). Pressing `Ctrl+V` again in those 5 s hides instead of sending the same clipboard twice. Files copied in Finder/Explorer are sent as files (name, MIME type guessed from the extension, contents), several at once if several are copied; folders are refused (zip them). Their total is checked against the relay's limit for one clip before anything is read: bigger ones upload as chunks in the background (section 7), or fail right away on relays before 0.3.0. `↵` on a file clip saves the files to Downloads (reusing one that's already there with the same content, otherwise `name (1).ext`) and puts them on the clipboard as files, so they paste into Finder/Explorer or a mail. Names from other devices go through `File::safe_name` first (no path parts, no characters or names Windows rejects). A single image file is previewed as the picture.
4. `Del` (macOS: `⌘⌫`, like Finder, so a stray Backspace can't delete): removes the selected clip from the server for every device.
5. The preview is sanitized: HTML goes through DOMPurify (never rendered if DOMPurify reports it can't run) into an `<iframe sandbox srcdoc>` whose own CSP blocks every network request, so remote images can't signal that a clip was viewed. Images arrive as binary IPC and are shown as blob URLs. Text previews are capped at 20k characters and HTML at 512 KB; copy always writes the full clip.

**Windows clipboard detail:** `clipboard-rs` empties the clipboard when it sets an image, so an image on its own goes through its image path (PNG plus a bitmap for Paint and older apps), and an image next to text is added as raw `PNG` data, which rich-text apps read.

**TTL dropdown:** `5m · 15m (default) · 1h · 8h · 24h · 7d`. Options above the server's `max_ttl` (from `/api/v1/config`) are hidden. The last choice becomes the device's default expiry (the same setting as in Settings), and `Tab` / `Shift+Tab` cycle through the options so you never need the mouse.

**Pairing / settings:** first run opens Settings: relay URL, optional access token, and the phrase (a **Generate** button fills in 6 EFF words; on other devices you type it). Pairing derives the key, checks the relay and token, and only then stores anything. Settings also hold device name, hotkey (recorded by pressing it), default expiry and launch at login. *Pair another device…* shows a QR code for the PWA and the same link to copy: another computer takes it pasted into the pair form, a server into `yacs pair`.

**Tray & lifecycle:** menu bar/tray icon with Open, Settings and Quit. Closing windows never quits. A second launch opens Settings, and `yacs-desktop --toggle` toggles Spotlight in the running instance, so Linux/Wayland users can bind that command to a desktop shortcut. On Wayland the app doesn't register its hotkey, and Settings shows that command (the AppImage's path, or `yacs-desktop` from the packages) with steps for the detected desktop instead of the shortcut recorder.

**Distribution:** `.github/workflows/release.yml` runs on a `v*` tag: `tauri-action` builds a macOS universal app and Windows MSI/NSIS into a draft release, the relay image goes to `ghcr.io/jonasseifried/yacs` (amd64 + arm64), and the release is published once all of that succeeded (tags like `v1.1.0-beta.1` become prereleases, which the updater skips). macOS is signed and notarized, and Windows is signed via Azure Trusted Signing, when those secrets exist; otherwise the builds are unsigned (Gatekeeper / SmartScreen warn).

**Updates:** `tauri-plugin-updater` against `releases/latest/download/latest.json`. Updates must be signed with the release key (`plugins.updater.pubkey`), and `requireSignedVersion` blocks downgrades to older signed releases. Release builds check 30 s after launch and every 12 h. An available update shows in the tray menu and in Settings, and installs only when the user clicks it. Updater artifacts are enabled only in CI, where the private key is.

## 6. Mobile

### PWA (served by the relay)
- **Pairing via QR:** desktop Settings → *Pair another device* shows `https://your-server/#pair=v1.<channel id>.<key>[&token=…]`. It carries the *derived* pairing (`Pairing::to_secret` in `yacs-core`), not the phrase: the desktop never stores the phrase, and the phone skips Argon2id. Everything is in the URL fragment, which browsers never send, so it never reaches the server or its logs; the app removes it from the address bar as soon as it's read. The PWA can also scan the code itself (BarcodeDetector, or jsQR loaded on demand), which iOS needs: a home screen app has its own storage, so a pairing made in the browser tab doesn't carry over, and the camera app only opens the browser. Typing the phrase works too (Argon2id in WASM, about a second on a phone).
- **Storage:** the pairing lives in `localStorage`. The page's CSP only runs scripts from the relay itself, so no third-party script can read it.
- **HTTPS required** for the clipboard API, the service worker and installing. Over plain `http://` (except `localhost`) you can still type, pick images and receive; the desktop's QR dialog warns about it.
- **Receive:** opening the app shows the history list (newest first) with the newest clip expanded; it refreshes when the app comes to the foreground and every 10 s while open. Each clip has a **Copy** button that calls `navigator.clipboard.write([new ClipboardItem({ "text/plain", "text/html", "image/png" })])` straight from the tap (Safari only allows clipboard writes inside the gesture). RTF is dropped, since browsers can't write it. Images and files get a **Save/Share** button (Web Share API, download as fallback). Files can't go on a browser's clipboard, except a single image file, which Copy puts there as the picture.
- **Send:** a Paste button (`navigator.clipboard.read()`; iOS shows its own "Paste" confirmation), a text area, or a file picker (pictures are sent as images, to paste anywhere; everything else as files; checked against the relay's size limit before sending), plus the same TTL dropdown as desktop. Android: Web Share Target, so "Share → YACS" works from other apps: the service worker takes the POST, holds the content in a cache just long enough for the page to pick it up, and deletes it.
- **Service worker:** caches only the app shell (hashed assets forever, the page network-first). It never touches `/api`, so nothing decrypted is ever cached.
- **Build:** `yacs-wasm` via `wasm-pack` (`pnpm --filter @yacs/ui wasm`), then `vite build --mode web` into `ui/dist/web`, which `yacs-server` embeds with `rust-embed` (read from disk in debug builds). The desktop build is `--mode desktop` and doesn't need WASM.

### Later: Tauri mobile
Same `ui/src/mobile` with the `tauri.ts` adapter. Rust reuses `yacs-core` + `yacs-client` directly (no WASM). Tauri's clipboard plugin is text-only on mobile, so rich formats need a small custom plugin: Swift `UIPasteboard` / Kotlin `ClipboardManager`. Adds an iOS Share Extension + Android share intent.

| Capability | PWA | Tauri mobile + native plugin |
| --- | --- | --- |
| Plain text copy | ✅ | ✅ |
| Rich text copy | ⚠️ `text/html` only; whether the target app accepts it varies | ✅ HTML + RTF |
| Image copy | ✅ PNG | ✅ |
| Files | ✅ up to the relay's size limit, via share sheet/download (not the clipboard) | ✅ share sheet / Files app |
| Send from other apps | Android only | ✅ iOS + Android |
| Background sync / notifications | ❌ (iOS) | ✅ push possible |
| Distribution | a URL | store review |

## 7. Large files (chunked upload)

Implemented for 0.3.0: files of any size (multi-GB, phones included) through the same relay, never more than a few MB in memory anywhere. Single-envelope files (0.2.x) stay for small ones. Direct device-to-device transfer (iroh, WebRTC) is dropped as overkill.

**Naming:** we call this "v2" in chat, but it is **not** a 2.0 release; ship it as the normal next version.

### Crypto (`yacs-core::stream`)
The age payload construction (STREAM, Hoang–Reyhanitabar–Rogaway–Vizár), written directly on `chacha20poly1305` (no `aead-stream` crate, since uploads need random access by chunk index for parallel and retried chunks):
- Per clip, the sender picks a random 32-byte `salt`. `file_key = HKDF-SHA256(ikm = channel key, salt, info = "yacs/v1/stream" || channel_id)`.
- The clip's files are **concatenated into one stream** and cut into chunks of `chunk_size` plaintext bytes (**4 MiB**; receivers accept 64 KiB to 16 MiB). Chunk `i` is ChaCha20-Poly1305 under `file_key` with nonce `i as 11-byte big-endian || last` (`last` = `0x01` on the final chunk, else `0x00`), AAD = `version || channel_id`. Sealed chunk = chunk + 16-byte tag.
- The final chunk may be short but is never empty (unless the whole stream is empty: then it's the only chunk). Reordering fails (index in the nonce), truncation fails (last flag), mixing clips fails (key per salt). Receivers also check every chunk's length against the header.
- **Never re-encrypt chunk `i` of a salt with different data** (nonce reuse breaks ChaCha20-Poly1305). Senders seal each chunk once and keep the sealed bytes for retries; a restarted upload gets a new salt.
- Shared test vectors (`streams` in `tests/vectors.json`: SHA-256 of each sealed chunk, the short one in full), run native and wasm.

### Payload
The clip itself stays a normal sealed envelope (the "header": small, fits the single-envelope route and limit). One appended variant describes the files:
```rust
ClipItem::Stream(Stream { salt: [u8; 32], chunk_size: u32, files: Vec<StreamFile { name, mime, size: u64 }> })
```
Offsets follow from the sizes. Other items (text) can sit next to it. Older clients can't decode it and show "Can't decrypt this clip", as with `File`.

**Which path:** files whose total is at most `ServerConfig::inline_file_limit()` (= `min(8 MiB, max_size − 64 KiB)` when the relay takes chunks) go in the envelope as `ClipItem::File` (instant previews, prefetch). Bigger ones are chunked when `/config` has `chunked: { max_chunk_bytes }`; without it (relays before 0.3.0) they're refused with a hint to update the relay.

### Relay
```
POST   /api/v1/channels/{c}/uploads?ttl=&length=&chunk_size=   body: header envelope → 201 { id }
PUT    /api/v1/channels/{c}/uploads/{id}/chunks/{i}            body: sealed chunk i, any order, repeatable → 204
GET    /api/v1/channels/{c}/uploads/{id}                       → { received: [i, …] }
POST   /api/v1/channels/{c}/uploads/{id}/complete              → 201 ClipMeta (409 if chunks are missing)
DELETE /api/v1/channels/{c}/uploads/{id}                       abort
GET    /api/v1/channels/{c}/clips/{id}                         → the header envelope, as today
GET    /api/v1/channels/{c}/clips/{id}/chunks/{i}              → sealed chunk i (immutable, cacheable)
```
- `length` is the sealed size of all chunks and `chunk_size` the sealed size of each but the last, so the relay knows how many chunks to expect and how long each must be; it rejects anything else (413 too long, 400 too short). Plain per-chunk PUTs (the S3 multipart model), not tus: simpler, parallel, and each retry is one small request.
- Chunk PUTs stream the body to `N.part` in the upload's dir (route without `DefaultBodyLimit`, counted by hand, 256 KiB write buffer), then rename it into place.
- **Storage:** open uploads live in `data/.tmp/{upload id}.up/` (`header.bin`, `00000000`, …) and are tracked in memory: a restart drops them with the rest of `.tmp`, and senders start over. `complete` renames the dir to `{channel}/{ulid}.{expires_at_ms}.{size}.d/` under a **new** ULID, so a long upload sorts as the newest clip, and the TTL starts then. The reaper also removes expired `.d` dirs, uploads idle for 24 h, and orphaned `.up` dirs. `max_clips` counts completed clips (eviction removes the whole dir); at most 4 open uploads per channel (429 beyond).
- `ClipMeta.size` is the total (header + chunks) and `ClipMeta.chunked` is set, also as `x-yacs-size` / `x-yacs-chunked` headers on the header's GET.
- **Limits:** no per-file limit; the disk quota (`YACS_MAX_DISK`, now **25 GB** by default) is the only cap, and an upload reserves its full size at `POST` (507 when it doesn't fit). `YACS_MAX_SIZE` keeps limiting single envelopes.
- **Proxies:** 4 MiB chunks pass Cloudflare (100 MB per request, 100 s per request) and our `deploy/nginx.conf` (`client_max_body_size 25m`). The README says a hand-written nginx config needs `client_max_body_size` ≥ 5m (its default is 1m). Caddy doesn't compress chunks.

### Clients
- **`yacs-client`:** `push_stream(clip, LocalFiles, ttl, progress, cancel)`: 3 chunks in flight, each read from disk and sealed on the blocking pool, retried with backoff (network errors, 5xx, 408, 429); on failure or cancel the upload is `DELETE`d so the quota frees at once. `fetch_stream(id, stream, sink, progress)`: 3 chunks ahead, in order, each read into an exactly sized buffer and opened on the blocking pool, written to a `Sink` file by file. Peak memory stays around 30 MB either way (buffers that grow while filling bloated it to the file's size).
- **CLI:** `yacs send big.iso` picks the chunked path by size (anything over the inline limit is sent as a file, even text), with a progress line on stderr; Ctrl+C aborts the upload. `yacs recv -o` streams to `name.part`, then renames; piped stdout works too. `yacs info` says whether the relay takes big files. The e2e test sends 200 MB and checks each process peaks below 50 MB (`wait4`).
- **Desktop:** ⌘V with big files starts a background upload (`transfers.rs`, one transfer at a time) with a progress bar and Cancel in Spotlight, which may hide meanwhile; the finished clip shows up like any clip. Big clips are never prefetched beyond their header: the preview lists names and sizes; ↵ downloads them into Downloads (`name.part`, then a free name) with progress, puts them on the clipboard and hides Spotlight. The paths are remembered, so ↵ again doesn't download again.
- **PWA upload:** in a module Web Worker: `file.slice()` → `arrayBuffer()` → WASM `StreamCipher.seal` → `PUT` with a `Uint8Array` body (streaming request bodies don't work in Safari). Progress bar plus the hint **"Keep this screen open until the upload finishes"**, and a screen wake lock while it runs: iOS pauses background pages; retries carry on when the page is visible again (about four minutes of attempts per chunk), a reload starts over. Android's share target receives big files too.
- **PWA download:** a download worker fetches and opens the chunks and hands them, one per pull, through a `MessagePort` to the service worker, which answers a hidden iframe's `/download/{token}` with a streamed `Content-Disposition: attachment` response (always `application/octet-stream`). The key never reaches the service worker; the page registers each download with it and waits for an ack, so an older service worker without the route falls back. iOS (chosen by user agent) and pages without a service worker: the worker writes the decrypted file into OPFS (`createSyncAccessHandle`), then a **Share** tap (iOS needs a fresh gesture) hands `getFile()` to the share sheet; the OPFS copy goes after sharing, or at the next start. Last resort: a Blob in memory, refused above 1 GB. Never a multi-GB `Blob` otherwise. Verified in Chromium: uploads, cancel (relay gets the `DELETE`), service-worker and OPFS downloads byte for byte. Unverified: whether OPFS-backed files stay out of RAM on iOS and whether service-worker downloads work from a home-screen app. **The user tests on iPhone and Android after the release.**

## 8. Roadmap

This reorders the original roadmap: crypto and the protocol come first, so the UI is built on a finished, tested core and nothing has to be retrofitted later.

| Phase | Focus | Deliverables | Done when |
| --- | --- | --- | --- |
| **0: Core** ✅ | `yacs-core` | Workspace, KDF, envelope, cipher, postcard serialization, test vectors, CI job checking the `wasm32` build | Round-trip + vector tests pass on native and wasm |
| **1: Server + CLI** ✅ | `yacs-server`, `yacs-client`, `yacs-cli` | Routes, per-clip TTL + clamping, history + per-channel cap, disk store, reaper, limits, token, config; `yacs send --ttl 1h` / `yacs list` / `yacs recv [id]` for text + images | Two terminals sync clips end-to-end through a local server; expiry and eviction covered by integration tests |
| **2: Desktop shell** ✅ | Tauri | Tray, hidden Spotlight window, global hotkey, single instance, autostart, pairing + settings UI (incl. 6-word EFF phrase generator in `yacs-core`), keyring | Hotkey opens/closes Spotlight reliably on macOS + Windows |
| **3: Desktop clipboard** ✅ (verified Mac ↔ PC) | Core UX | `clipboard-rs` multi-format read/write, history list + keyboard navigation, Ctrl+C / Ctrl+V / Del, TTL dropdown, sanitized preview | Rich text from Word/browser and screenshots round-trip between Mac and PC |
| **4: PWA + release (v1.0)** ✅ (v0.1.1: phones pair via QR over HTTPS, relay on a VPS behind nginx, desktop self-update verified) | Mobile + ship | `yacs-wasm`, mobile UI, embedded PWA, QR pairing, Dockerfile + compose (Caddy or nginx), signed desktop builds, updater | A phone can pair via QR and copy/send; `docker compose up` works on a VPS |
| **5: v1.x** | Breadth | ✅ SSE live updates (0.2.0) and the relay version in desktop Settings; ✅ CLI for servers: `yacs pair` saves the pairing (from the desktop's link or the phrase), `yacs send FILE`, static release binaries, `yacs update` (signed), `yacs relay update` (Docker compose); the desktop apps bundle `yacs` and put it on the PATH on request (macOS, Windows); ✅ Linux desktop (0.2.3); files through the relay, up to its size limit | |
| **6: Large files** ✅ (0.3.0; device testing pending) | Any size through the relay | Chunked, streamed uploads and downloads on every client (section 7) | A multi-GB file goes phone ↔ desktop through the relay, memory stays flat |
| **7: Native mobile** (later) | Tauri mobile | Native clipboard plugins + share extensions | |

### Note on P2P
Direct device-to-device transfer (iroh or WebRTC) was considered for big files and dropped: chunked uploads through the relay (section 7) cover it without hole punching, TURN or both devices being online at once.

## 9. Testing
- **core:** unit tests + fixed test vectors (phrase → channel_id/key/ciphertext), run against both the native and WASM builds so desktop and PWA can never derive different keys.
- **server:** Axum integration tests with `tempfile` data dirs and a fake clock: TTL clamping, expiry, oldest-first eviction at `max_clips`, list ordering.
- **e2e:** the CLI sends and receives against a real server in CI.
- **manual matrix (Phase 3):** Word, Google Docs, Safari/Chrome, Notes, screenshots, on macOS and Windows.

## 10. Open questions
None right now. Settled: `YACS_MAX_CLIPS_PER_CHANNEL=50`, with a disk quota covering the worst case, and `7d` stays in the dropdown but only shows when a self-hoster raises `YACS_MAX_TTL`.
