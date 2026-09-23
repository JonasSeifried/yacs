import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { Platform } from ".";

export const tauriPlatform: Platform = {
  status: () => invoke("status"),
  generatePhrase: () => invoke("generate_phrase"),
  pair: (serverUrl, token, phrase) => invoke("pair", { serverUrl, token, phrase }),
  unpair: () => invoke("unpair"),
  savePreferences: (preferences) => invoke("save_preferences", { preferences }),
  serverConfig: () => invoke("server_config"),
  listClips: () => invoke("list_clips"),
  hideSpotlight: () => invoke("hide_spotlight"),
  openSettings: () => invoke("open_settings"),
  onSpotlightShown: (handler) => listen("spotlight-shown", handler),
  onStatusChanged: (handler) => listen("status-changed", handler),
};
