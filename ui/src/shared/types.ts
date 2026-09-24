// Mirrors of the Rust types that cross the IPC boundary.

/** `yacs_core::api::ClipMeta`: what the relay knows about a clip. */
export interface ClipMeta {
  id: string;
  created_at_ms: number;
  expires_at_ms: number;
  size: number;
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
}

export type Os = "macos" | "windows" | "linux";

/** Desktop app state, from the `status` command. */
export interface Status {
  paired: boolean;
  serverUrl: string | null;
  deviceName: string;
  hotkey: string;
  hotkeyError: string | null;
  defaultTtlSecs: number;
  autostart: boolean;
  os: Os;
  version: string;
  /** A newer release that's ready to install. */
  update: string | null;
}

/** The desktop's "Pair another device" QR code and link. */
export interface PhonePairing {
  url: string;
  /** `data:image/svg+xml` URL. */
  qr: string;
  warning: string | null;
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
  | { Image: { mime: string; data: Uint8Array } };

/** `yacs_core::Clip`: a decrypted clip with every format it carries. */
export interface Clip {
  created_at_ms: number;
  device_name: string;
  items: ClipItem[];
}
