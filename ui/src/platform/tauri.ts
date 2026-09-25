import { Channel, invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { TransferChanged } from "../shared/types";
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
  sendClipboard: (ttlSecs, onProgress) => {
    const channel = new Channel<{ done: number; total: number }>();
    channel.onmessage = ({ done, total }) => onProgress(done, total);
    return invoke("send_clipboard", { ttlSecs, onProgress: channel });
  },
  transferStatus: () => invoke("transfer_status"),
  cancelTransfer: () => invoke("cancel_transfer"),
  deleteClip: (id) => invoke("delete_clip", { id }),
  setDefaultTtl: (ttlSecs) => invoke("set_default_ttl", { ttlSecs }),
  phonePairing: () => invoke("phone_pairing"),
  installCli: () => invoke("install_cli"),
  uninstallCli: () => invoke("uninstall_cli"),
  checkUpdate: () => invoke("check_update"),
  installUpdate: () => invoke("install_update"),
  hideSpotlight: () => invoke("hide_spotlight"),
  openSettings: () => invoke("open_settings"),
  hideSettings: () => invoke("hide_settings"),
  pauseHotkey: () => invoke("pause_hotkey"),
  resumeHotkey: () => invoke("resume_hotkey"),
  onSpotlightShown: (handler) => listen("spotlight-shown", handler),
  onStatusChanged: (handler) => listen("status-changed", handler),
  onSettingsShown: (handler) => listen("settings-shown", handler),
  onClipsChanged: (handler) => listen("clips-changed", handler),
  onTransferChanged: (handler) => listen<TransferChanged>("transfer-changed", (e) => handler(e.payload)),
};
