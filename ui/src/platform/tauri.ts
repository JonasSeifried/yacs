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
  getClip: (id) => invoke("get_clip", { id }),
  clipImage: async (id) => {
    const bytes = await invoke<ArrayBuffer>("clip_image", { id });
    return new Blob([bytes]);
  },
  copyClip: (id) => invoke("copy_clip", { id }),
  sendClipboard: (ttlSecs) => invoke("send_clipboard", { ttlSecs }),
  deleteClip: (id) => invoke("delete_clip", { id }),
  setDefaultTtl: (ttlSecs) => invoke("set_default_ttl", { ttlSecs }),
  phonePairing: () => invoke("phone_pairing"),
  checkUpdate: () => invoke("check_update"),
  installUpdate: () => invoke("install_update"),
  hideSpotlight: () => invoke("hide_spotlight"),
  openSettings: () => invoke("open_settings"),
  onSpotlightShown: (handler) => listen("spotlight-shown", handler),
  onStatusChanged: (handler) => listen("status-changed", handler),
  onClipsChanged: (handler) => listen("clips-changed", handler),
};
