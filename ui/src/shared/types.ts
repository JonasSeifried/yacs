// Mirrors of the Rust types that cross the IPC boundary.

/** `yacs_core::api::ClipMeta`: what the relay knows about a clip. */
export interface ClipMeta {
  id: string;
  created_at_ms: number;
  expires_at_ms: number;
  /** Everything stored, chunks included. */
  size: number;
  /** The files are stored as chunks next to the clip, which stays small. */
  chunked?: boolean;
}

/** A decrypted clip, as the desktop's `ClipView` (Rust) shows it. */
export interface ClipView {
  meta: ClipMeta;
  deviceName: string;
  /** Capped for the preview; `textTruncated` says whether it was cut. */
  text: string | null;
  textTruncated: boolean;
  /** Raw, unsanitized HTML from the sender; null when absent or too large to preview. */
  html: string | null;
  rtf: boolean;
  image: ImageInfo | null;
  files: FileInfo[];
}

export interface FileInfo {
  name: string;
  mime: string;
  size: number;
}

export interface ImageInfo {
  mime: string;
  size: number;
  width: number | null;
  height: number | null;
}

/** `yacs_core::api::ChannelEvent`: one message of a channel's live event stream. */
export type ChannelEvent =
  | { type: "added"; clip: ClipMeta }
  | { type: "deleted"; id: string }
  | { type: "cleared" }
  | { type: "other" };

/** `yacs_core::api::ServerConfig`. */
export interface ServerConfig {
  default_ttl_secs: number;
  max_ttl_secs: number;
  max_size_bytes: number;
  max_clips: number;
  /** Missing from relays before 0.2.0. */
  version?: string;
  /** Set when the relay takes big files as chunks (0.3.0 and later). */
  chunked?: { max_chunk_bytes: number };
  /** Set when the relay registers spaces (0.5.0 and later); `public`: anyone may create one. */
  accounts?: { public: boolean };
  /** The relay has a privacy policy and imprint at `/privacy` and `/imprint`. */
  legal?: boolean;
}

/** `yacs_core::api::SpaceLimits`: what one space may do (relays from 0.5.0). */
export interface SpaceLimits {
  plan: "free" | "unlimited" | "other";
  default_ttl_secs: number;
  max_ttl_secs: number;
  /** Chunks included; missing when only the relay's disk limits it. */
  max_clip_bytes?: number;
  /** Uploads and downloads per day (UTC); missing when there's no daily limit. */
  daily_transfer_bytes?: number;
  transfer_used_bytes: number;
  max_clips: number;
}

/** A big upload or download running in the background (desktop). */
export interface Transfer {
  direction: "upload" | "download";
  /** "disk.iso" or "3 files". */
  label: string;
  done: number;
  total: number;
}

/** The desktop's `transfer-changed` event. */
export interface TransferChanged {
  transfer: Transfer | null;
  /** Set once, when a transfer ends. */
  finished: { direction: Transfer["direction"]; ok: boolean; cancelled: boolean; message: string } | null;
}

/** What `sendClipboard` did: sent a clip, or started uploading big files. */
export interface Sent {
  clip: ClipView | null;
  upload: Transfer | null;
}

export type Os = "macos" | "windows" | "linux";

/** Desktop app state, from the `status` command. */
export interface Status {
  /** The space in use; null until this computer starts or joins one. */
  space: SpaceStatus | null;
  deviceName: string;
  hotkey: string;
  hotkeyError: string | null;
  /** Wayland: the user binds this in their desktop instead of recording a hotkey. */
  manualShortcut: ManualShortcut | null;
  defaultTtlSecs: number;
  autostart: boolean;
  os: Os;
  version: string;
  /** A newer release that's ready to install. */
  update: string | null;
  /** The version being installed right now (from the tray, say). */
  updateInstalling: string | null;
  /** Why the last install failed. */
  updateError: string | null;
  cli: CliStatus;
}

export interface SpaceStatus {
  /** This computer's own name for it. */
  name: string;
  /** The relay's URL. */
  relay: string;
}

export interface ManualShortcut {
  /** This AppImage or the installed binary, with `--toggle`. */
  command: string;
  appimage: boolean;
  desktop: "gnome" | "kde" | "hyprland" | "sway" | "other";
}

/** The `yacs` command bundled with the desktop app. */
export interface CliStatus {
  /** Release builds bundle it; dev builds may not. */
  available: boolean;
  installed: boolean;
  /** macOS: `/usr/local/bin/yacs`. Windows: the app's folder, which goes on the PATH. */
  location: string | null;
}

/** The code in the desktop's Invite panel, as it changes. */
export type CodeEvent =
  | { kind: "code"; code: string; /** After someone typed a wrong one. */ replaced: boolean }
  | { kind: "joined"; device: string }
  | { kind: "failed"; error: string };

/** The desktop's "Invite a device" QR code and link, for a one-time invite. */
export interface Invite {
  url: string;
  /** `data:image/svg+xml` URL. */
  qr: string;
  warning: string | null;
  /** Where the relay keeps it; `onInviteUsed` names it. */
  slot: string;
}

export interface Preferences {
  deviceName: string;
  hotkey: string;
  defaultTtlSecs: number;
  autostart: boolean;
}

/** `yacs_core::ClipItem`, as serde hands it to JS through `yacs-wasm`. */
export type ClipItem =
  | { Text: string }
  | { Html: string }
  | { Rtf: string }
  | { Image: { mime: string; data: Uint8Array } }
  | { File: { name: string; mime: string; data: Uint8Array } }
  | { Stream: StreamInfo };

/** `yacs_core::Stream`: files too big for the clip, stored as chunks. */
export interface StreamInfo {
  salt: Uint8Array;
  /** Plaintext bytes per chunk. */
  chunk_size: number;
  files: { name: string; mime: string; size: number }[];
}

/** `yacs_core::Clip`: a decrypted clip with every format it carries. */
export interface Clip {
  created_at_ms: number;
  device_name: string;
  items: ClipItem[];
}
