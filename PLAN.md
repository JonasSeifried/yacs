# YACS: Implementation Plan

Self-hostable, end-to-end encrypted clipboard sync. Rust monorepo: Axum relay, Tauri v2 desktop, PWA for mobile (v1), native Tauri mobile (v2).

## 1. Decisions

| Topic | Decision |
| --- | --- |
| Desktop v1 | macOS + Windows. Linux in v1.x (X11 first; Wayland gets a CLI/tray fallback because global hotkeys are blocked there). |
| Mobile | v1: PWA served by the relay. v2: Tauri mobile app with native clipboard plugins and share extensions. Both share one UI codebase. |
| Pairing | Shared phrase, focus on self-hosting. Low-cost hardening: the app generates the phrase by default, and Argon2id is used for key derivation. Accounts only if a public hosted version happens later. |
| Hotkey UX | `Ctrl+Shift+Space` opens Spotlight with a preview of the latest remote clip. `Ctrl+C` copies the preview to the local clipboard. `Ctrl+V` sends the local clipboard. `Esc` or losing focus closes it. |
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
│  ├─ yacs-cli/                # `yacs send` / `yacs recv`: test harness now, Linux/Wayland fallback later
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
- The key is derived once at pairing time and stored: macOS Keychain / Windows Credential Manager via the `keyring` crate, IndexedDB on the PWA. The Argon2 cost is paid only once.
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
GET    /api/v1/config                                 → { default_ttl, max_ttl, max_size, max_clips }
POST   /api/v1/channels/{channel_id}/clips?ttl=900    body: octet-stream Envelope
                                                      → 201 { id, created_at, expires_at }  (ttl clamped to max_ttl)
GET    /api/v1/channels/{channel_id}/clips            → [{ id, created_at, expires_at, size }]  newest first
GET    /api/v1/channels/{channel_id}/clips/latest     → 200 Envelope | 404   (ETag = clip id, supports 304)
GET    /api/v1/channels/{channel_id}/clips/{id}       → 200 Envelope | 404   (immutable, cacheable)
DELETE /api/v1/channels/{channel_id}/clips/{id}
DELETE /api/v1/channels/{channel_id}/clips            clear the whole history
GET    /api/v1/channels/{channel_id}/events           SSE "new clip" ping (v1.x)
GET    /healthz
GET    /*                                             embedded PWA (rust-embed)
```

- **Clip IDs** are ULIDs assigned by the server. They sort by time, so "newest first" is just a reverse sort of the directory listing.
- **History = everything not yet expired.** A per-channel cap (`max_clips`) evicts the oldest clip first, so a busy channel can't grow without limit.
- **Storage:** `data/{channel_id}/{ulid}.{expires_unix}.bin`. Because the expiry is in the filename, the reaper never has to open a file. Writes go to a temp file first and are then renamed, so a clip is never half-written. The reaper runs on a `tokio::time::interval` (60 s) and deletes expired files and empty channel dirs.
- **The server sees** each clip's size, creation time and expiry. Contents, formats and device names stay encrypted.
- **Limits:** `DefaultBodyLimit`, per-IP rate limiting (`tower_governor`), `max_clips` per channel, total disk quota.
- **Optional access token:** set `YACS_ACCESS_TOKEN` and clients must send `Authorization: Bearer …`. This keeps strangers from filling your disk when the server is reachable from the internet.
- **Config:** env vars / `clap` flags. Durations are parsed with `humantime` (`15m`, `24h`, `7d`).

  | Setting | Default |
  | --- | --- |
  | `YACS_BIND` | `0.0.0.0:8080` |
  | `YACS_DATA_DIR` | `./data` |
  | `YACS_DEFAULT_TTL` | `15m` |
  | `YACS_MAX_TTL` | `24h` |
  | `YACS_MAX_SIZE` | `20MB` |
  | `YACS_MAX_CLIPS_PER_CHANNEL` | `50` |
  | `YACS_ACCESS_TOKEN` | unset (open) |
- **TLS:** terminate at a reverse proxy (Caddy example in `deploy/`).
- **No CORS needed.** The PWA is served from the same origin, and desktop makes its requests from Rust.
- **Container:** multi-stage Docker build producing a static musl binary on a distroless or scratch base.

## 5. Desktop (Tauri v2, macOS + Windows)

**Plugins & crates:** `tauri-plugin-global-shortcut`, `-single-instance`, `-autostart`, `-store` (settings), `-updater`, built-in tray, `keyring`, **`clipboard-rs`**.

- **Why `clipboard-rs` instead of the Tauri clipboard plugin:** the plugin can't *read* HTML/RTF. `clipboard-rs` reads and writes text, HTML, RTF and images on macOS and Windows.
- **Why Rust handles `Ctrl+V` instead of the DOM `paste` event:** the webview paste event hides RTF and behaves differently in WKWebView and WebView2.

**Spotlight window:** `decorations: false`, `transparent`, `alwaysOnTop`, `skipTaskbar`, hidden at start, hides when it loses focus. macOS: `ActivationPolicy::Accessory` (no Dock icon). Consider `tauri-nspanel` so it also shows over fullscreen apps. After hiding, focus goes back to the previous app.

**Spotlight layout**
```
┌──────────────────────────────────────────────────────┐
│ ▸ "Meeting notes for Thursday…"   MacBook · 2m ago   │  ← selected
│   [image 1280×720]                PC · 11m ago       │
│   "https://github.com/…"          iPhone · 1h ago    │
├──────────────────────────────────────────────────────┤
│  preview of the selected clip                        │
├──────────────────────────────────────────────────────┤
│ Ctrl+C copy · Ctrl+V send · Del delete   TTL [15m ▾] │
└──────────────────────────────────────────────────────┘
```

**Flow**
1. Hotkey pressed: show the window and at the same time `list_clips` + fetch the newest clip (ETag cache means repeat fetches are cheap), then decrypt and render the preview. Older clips are fetched when you select them and cached in memory by ID (clips never change).
2. `↑/↓`: select a clip. `Ctrl+C`: Rust writes all formats of the selected clip to the clipboard, then the window hides.
3. `Ctrl+V`: the keydown is intercepted. Rust reads the native clipboard (all formats), encrypts, POSTs with the TTL from the dropdown, shows a "Sent" confirmation, then the window hides.
4. `Del`: removes the selected clip from the server for every device.
5. The preview is sanitized: HTML goes through DOMPurify and is rendered in a sandboxed iframe, and images are shown as blob URLs.

**TTL dropdown:** `5m · 15m (default) · 1h · 8h · 24h · 7d`. Options above the server's `max_ttl` (from `/api/v1/config`) are hidden. The last choice is remembered per device, and `Tab` cycles through the options so you never need the mouse.

**Pairing / settings:** first run offers "Create pairing" (generates a phrase, shows it + a QR code) or "Join" (type the phrase). Settings: server URL, access token, hotkey, device name.

**Distribution:** GitHub Actions + `tauri-action`. macOS universal build, signed and notarized (Apple account available). Windows MSI/NSIS, signed via Azure Trusted Signing, otherwise users see SmartScreen warnings.

## 6. Mobile

### v1: PWA (served by the relay)
- **Pairing via QR:** `https://your-server/#pair=<phrase>`. The phrase is in the URL fragment, so it never reaches the server or its logs. Scanning it installs the pairing.
- **Receive:** opening the app shows the history list (newest first) with the newest clip expanded. Each clip has a **Copy** button that calls `navigator.clipboard.write([new ClipboardItem({ "text/plain", "text/html" | "image/png" })])`. Images also get a **Save/Share** button (Web Share API).
- **Send:** a Paste button (`navigator.clipboard.read()`; iOS shows its own "Paste" confirmation), a text area, or an image picker, plus the same TTL dropdown as desktop. Android: Web Share Target, so "Share → YACS" works from other apps.

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
| **1: Server + CLI** | `yacs-server`, `yacs-client`, `yacs-cli` | Routes, per-clip TTL + clamping, history + per-channel cap, disk store, reaper, limits, token, config; `yacs send --ttl 1h` / `yacs list` / `yacs recv [id]` for text + images | Two terminals sync clips end-to-end through a local server; expiry and eviction covered by integration tests |
| **2: Desktop shell** | Tauri | Tray, hidden Spotlight window, global hotkey, single instance, autostart, pairing + settings UI (incl. 6-word EFF phrase generator in `yacs-core`), keyring | Hotkey opens/closes Spotlight reliably on macOS + Windows |
| **3: Desktop clipboard** | Core UX | `clipboard-rs` multi-format read/write, history list + keyboard navigation, Ctrl+C / Ctrl+V / Del, TTL dropdown, sanitized preview | Rich text from Word/browser and screenshots round-trip between Mac and PC |
| **4: PWA + release (v1.0)** | Mobile + ship | `yacs-wasm`, mobile UI, embedded PWA, QR pairing, Dockerfile + compose/Caddy, signed desktop builds, updater | A phone can pair via QR and copy/send; `docker compose up` works on a VPS |
| **5: v1.x** | Breadth | Linux (X11 + CLI fallback for Wayland), SSE live updates (history refreshes while Spotlight is open) | |
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
