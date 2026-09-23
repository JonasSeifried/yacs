// Mirrors of the Rust types that cross the IPC boundary.

/** `yacs_core::api::ClipMeta`: what the relay knows about a clip. */
export interface ClipMeta {
  id: string;
  created_at_ms: number;
  expires_at_ms: number;
  size: number;
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
