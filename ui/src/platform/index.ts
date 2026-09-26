import type {
  ClipMeta,
  ClipView,
  Invite,
  Preferences,
  Sent,
  ServerConfig,
  Status,
  Transfer,
  TransferChanged,
} from "../shared/types";
import { tauriPlatform } from "./tauri";

/**
 * Everything the UI needs from the host it runs in. The desktop app implements
 * it with Tauri commands (Rust does crypto, network and clipboard); the PWA
 * will implement it with the WASM core and web APIs.
 */
export interface Platform {
  status(): Promise<Status>;
  /** Starts a new space on the relay, once it accepts the token. */
  createSpace(serverUrl: string, token: string | null, name: string | null): Promise<void>;
  /** Joins the space in an invite link from another device. */
  joinSpace(link: string): Promise<void>;
  /** This computer's own name for the space; resolves to the name as saved. */
  renameSpace(name: string): Promise<string>;
  leaveSpace(): Promise<void>;
  savePreferences(preferences: Preferences): Promise<void>;
  serverConfig(): Promise<ServerConfig>;
  listClips(): Promise<ClipMeta[]>;
  /** Decrypted; null if the clip expired or was deleted. */
  getClip(id: string): Promise<ClipView | null>;
  clipImage(id: string): Promise<Blob>;
  /**
   * Puts every format of the clip on the clipboard. False when its files are
   * big: they download in the background first (see `onTransferChanged`).
   */
  copyClip(id: string): Promise<boolean>;
  /**
   * Encrypts and sends what's on the clipboard right now, telling
   * `onProgress` how much went out; big files upload in the background.
   */
  sendClipboard(ttlSecs: number, onProgress: (done: number, total: number) => void): Promise<Sent>;
  /** The big upload or download in progress, if any. */
  transferStatus(): Promise<Transfer | null>;
  cancelTransfer(): Promise<void>;
  deleteClip(id: string): Promise<void>;
  setDefaultTtl(ttlSecs: number): Promise<void>;
  /** Contains the space's key: only fetch it when the user asks to see it. */
  invite(): Promise<Invite>;
  /** Puts the bundled `yacs` command on the PATH, in this computer's space. */
  installCli(): Promise<void>;
  uninstallCli(): Promise<void>;
  /** Resolves to the new version, if there is one. */
  checkUpdate(): Promise<string | null>;
  /** Restarts the app when it succeeds. */
  installUpdate(): Promise<void>;
  hideSpotlight(): Promise<void>;
  openSettings(): Promise<void>;
  hideSettings(): Promise<void>;
  /** While Settings records a new shortcut, so the current one reaches it. */
  pauseHotkey(): Promise<void>;
  resumeHotkey(): Promise<void>;
  /** Returns an unsubscribe function. */
  onSpotlightShown(handler: () => void): Promise<() => void>;
  onStatusChanged(handler: () => void): Promise<() => void>;
  onSettingsShown(handler: () => void): Promise<() => void>;
  /** The relay reported new or deleted clips (only sent while Spotlight is open). */
  onClipsChanged(handler: () => void): Promise<() => void>;
  /** Progress of a big transfer, and how it ended. */
  onTransferChanged(handler: (event: TransferChanged) => void): Promise<() => void>;
}

export const platform: Platform = tauriPlatform;
