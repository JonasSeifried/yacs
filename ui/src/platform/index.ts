import type { ClipMeta, Preferences, ServerConfig, Status } from "../shared/types";
import { tauriPlatform } from "./tauri";

/**
 * Everything the UI needs from the host it runs in. The desktop app implements
 * it with Tauri commands (Rust does crypto, network and clipboard); the PWA
 * will implement it with the WASM core and web APIs.
 */
export interface Platform {
  status(): Promise<Status>;
  generatePhrase(): Promise<string>;
  pair(serverUrl: string, token: string | null, phrase: string): Promise<void>;
  unpair(): Promise<void>;
  savePreferences(preferences: Preferences): Promise<void>;
  serverConfig(): Promise<ServerConfig>;
  listClips(): Promise<ClipMeta[]>;
  hideSpotlight(): Promise<void>;
  openSettings(): Promise<void>;
  /** Returns an unsubscribe function. */
  onSpotlightShown(handler: () => void): Promise<() => void>;
  onStatusChanged(handler: () => void): Promise<() => void>;
}

export const platform: Platform = tauriPlatform;
