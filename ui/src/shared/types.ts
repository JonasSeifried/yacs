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

/** `yacs_core::api::ServerConfig`. */
export interface ServerConfig {
  default_ttl_secs: number;
  max_ttl_secs: number;
  max_size_bytes: number;
  max_clips: number;
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
}

export interface Preferences {
  deviceName: string;
  hotkey: string;
  defaultTtlSecs: number;
  autostart: boolean;
}
