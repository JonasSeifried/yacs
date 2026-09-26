import { type FormEvent, type KeyboardEvent, useCallback, useEffect, useRef, useState } from "react";
import { platform } from "../platform";
import { acceleratorFromEvent, formatAccelerator } from "../shared/hotkey";
import { ttlChoices } from "../shared/time";
import type {
  CodeEvent,
  Invite,
  ManualShortcut,
  Os,
  Preferences,
  ServerConfig,
  SpaceStatus,
  Status,
} from "../shared/types";
import { relayBehind } from "../shared/version";

export function Settings() {
  const [status, setStatus] = useState<Status | null>(null);
  const [serverConfig, setServerConfig] = useState<ServerConfig | null>(null);

  const refresh = useCallback(async () => {
    const s = await platform.status();
    setStatus(s);
    setServerConfig(s.space ? await platform.serverConfig().catch(() => null) : null);
  }, []);

  useEffect(() => {
    refresh();
    // Settings stays loaded while hidden: re-read the relay's version etc. each time it opens.
    const subscriptions = [platform.onStatusChanged(refresh), platform.onSettingsShown(refresh)];
    return () => subscriptions.forEach((s) => s.then((unsubscribe) => unsubscribe()));
  }, [refresh]);

  // Esc and ⌘W / Ctrl+W close the window, like its close button. The hotkey
  // recorder takes Esc for itself (and prevents the default) while recording.
  useEffect(() => {
    const onKey = (e: globalThis.KeyboardEvent) => {
      const mod = status?.os === "macos" ? e.metaKey : e.ctrlKey;
      if (e.defaultPrevented) return;
      if (e.key === "Escape" || (mod && e.key.toLowerCase() === "w")) {
        e.preventDefault();
        platform.hideSettings();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [status?.os]);

  if (!status) return null;
  return (
    <main className="settings">
      <section className="card">
        <h2>Space</h2>
        {status.space ? <SpaceSettings space={status.space} /> : <SetUpSpace />}
      </section>
      <section className="card">
        <h2>Preferences</h2>
        <PreferencesForm status={status} serverConfig={serverConfig} />
      </section>
      {status.cli.available && (
        <section className="card">
          <h2>Command line</h2>
          <CommandLine status={status} />
        </section>
      )}
      <section className="card">
        <h2>Updates</h2>
        <Updates status={status} serverConfig={serverConfig} />
      </section>
    </main>
  );
}

/**
 * The relay doesn't update itself, and the desktop app does, so the relay
 * falls behind unless someone pulls the new image. `latest` is the newest
 * release this app knows of.
 */
function RelayHint({ relay, latest }: { relay: string | undefined; latest: string }) {
  if (!relayBehind(relay, latest)) return null;
  return (
    <p className="hint">
      Relay update available: {latest} (the relay runs {relay ?? "a version before 0.2.0"}). Update it on the server
      with <span className="mono">yacs relay update</span>, or{" "}
      <span className="mono">docker compose pull && docker compose up -d</span>.
    </p>
  );
}

function Updates({ status, serverConfig }: { status: Status; serverConfig: ServerConfig | null }) {
  const [state, setState] = useState<"idle" | "checking" | "current" | "installing">("idle");
  const [error, setError] = useState<string | null>(null);

  const check = async () => {
    setState("checking");
    setError(null);
    try {
      setState((await platform.checkUpdate()) ? "idle" : "current");
    } catch (e) {
      setError(String(e));
      setState("idle");
    }
  };
  const install = async () => {
    setState("installing");
    setError(null);
    try {
      await platform.installUpdate(); // restarts on success
    } catch (e) {
      setError(String(e));
      setState("idle");
    }
  };

  return (
    <>
      <p className="hint">
        YACS {status.version}
        {state === "current" && " is up to date."}
        {serverConfig?.version && ` · relay ${serverConfig.version}`}
      </p>
      {serverConfig && <RelayHint relay={serverConfig.version} latest={status.update ?? status.version} />}
      {(error ?? status.updateError) && <p className="error">{error ?? status.updateError}</p>}
      <div className="actions">
        {status.updateInstalling ? (
          <button className="primary" disabled>
            Installing {status.updateInstalling}…
          </button>
        ) : status.update ? (
          <button className="primary" onClick={install} disabled={state === "installing"}>
            {state === "installing" ? "Installing…" : `Update to ${status.update} and restart`}
          </button>
        ) : (
          <button onClick={check} disabled={state === "checking"}>
            {state === "checking" ? "Checking…" : "Check for updates"}
          </button>
        )}
      </div>
    </>
  );
}

/** The `yacs` command that comes with the app, on the PATH only if the user wants it. */
function CommandLine({ status }: { status: Status }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const { installed, location } = status.cli;

  const run = async (action: () => Promise<void>) => {
    setBusy(true);
    setError(null);
    try {
      await action();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <>
      {installed ? (
        <p className="hint">
          {status.os === "windows" ? (
            <>
              <code>yacs</code> is on your PATH (from {location}). Open a new terminal to use it.
            </>
          ) : (
            <>
              <code>yacs</code> is installed at {location}.
            </>
          )}{" "}
          Try <code>yacs send notes.txt</code> or <code>yacs recv</code>. It updates along with the app.
        </p>
      ) : (
        <p className="hint">
          Adds the <code>yacs</code> command for terminals and scripts
          {status.space && `, in “${status.space.name}” like this computer`}.{" "}
          {status.os === "macos"
            ? `It goes to ${location}; macOS may ask for your password.`
            : "It updates along with the app."}
        </p>
      )}
      {error && <p className="error">{error}</p>}
      <div className="actions">
        {installed ? (
          <button onClick={() => run(platform.uninstallCli)} disabled={busy}>
            {busy ? "Removing…" : "Remove command"}
          </button>
        ) : (
          <button onClick={() => run(platform.installCli)} disabled={busy}>
            {busy ? "Installing…" : "Install command"}
          </button>
        )}
      </div>
    </>
  );
}

function SpaceSettings({ space }: { space: SpaceStatus }) {
  const [error, setError] = useState<string | null>(null);
  const leave = async () => {
    if (!confirm(`Leave “${space.name}”? This computer stops sharing clips with the space. To come back, you'll need an invite from one of its devices.`))
      return;
    try {
      await platform.leaveSpace();
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <>
      <SpaceName name={space.name} />
      <p className="paired">
        <span className="dot" /> Syncing through <strong>{space.relay}</strong>
      </p>
      {error && <p className="error">{error}</p>}
      <InviteDevice relay={space.relay} />
      <div className="actions">
        <button className="danger" onClick={leave}>
          Leave this space
        </button>
      </div>
    </>
  );
}

/** Saves once typing pauses, like the device name. */
function SpaceName({ name }: { name: string }) {
  const [value, setValue] = useState(name);
  const [error, setError] = useState<string | null>(null);
  const timer = useRef<ReturnType<typeof setTimeout>>(undefined);

  // Renamed from elsewhere (the other window, say) while not typing here.
  useEffect(() => {
    if (!timer.current) setValue(name);
  }, [name]);

  const save = async (next: string) => {
    clearTimeout(timer.current);
    timer.current = undefined;
    if (!next.trim()) {
      setError("The name can't be empty.");
      return;
    }
    setError(null);
    try {
      setValue(await platform.renameSpace(next));
    } catch (e) {
      setError(String(e));
    }
  };

  return (
    <label className="space-name">
      Name <span className="optional">only on this computer; your other devices keep their own</span>
      <input
        required
        maxLength={64}
        value={value}
        onChange={(e) => {
          const next = e.target.value;
          setValue(next);
          clearTimeout(timer.current);
          timer.current = setTimeout(() => save(next), NAME_SAVE_MS);
        }}
        onBlur={() => timer.current && save(value)}
      />
      {error && <span className="error">{error}</span>}
    </label>
  );
}

function InviteDevice({ relay }: { relay: string }) {
  const [invite, setInvite] = useState<Invite | null>(null);
  /** Who joined, once someone did: with the link (null) or the code (their device name). */
  const [joined, setJoined] = useState<{ device: string | null } | null>(null);
  const [code, setCode] = useState<CodeEvent | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);
  /** The link went somewhere (copied), so it has to stay valid. */
  const shared = useRef(false);
  const current = useRef<Invite | null>(null);
  current.current = invite;

  /** A link only this screen showed needn't stay on the relay once it's done with. */
  const retire = useCallback(() => {
    const shown = current.current;
    if (shown && !shared.current) platform.revokeInvite(shown.slot).catch(() => {});
    platform.stopCode();
  }, []);

  const hide = useCallback(() => {
    retire();
    setInvite(null);
    setJoined(null);
    setCode(null);
  }, [retire]);

  // Don't leave the invite on screen, or its code open, when the window is hidden.
  useEffect(() => {
    const onVisibility = () => document.visibilityState === "hidden" && hide();
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      document.removeEventListener("visibilitychange", onVisibility);
      retire();
    };
  }, [hide, retire]);

  // Codes can arrive as soon as they're started: listen all along.
  useEffect(() => {
    const codes = platform.onInviteCode((event) => {
      if (event.kind === "joined") {
        setJoined({ device: event.device });
        retire();
      } else setCode(event);
    });
    return () => void codes.then((u) => u());
  }, [retire]);

  // The relay says when the link on screen was used.
  useEffect(() => {
    if (!invite || joined) return;
    const used = platform.onInviteUsed((slot) => {
      if (slot !== invite.slot) return;
      setJoined({ device: null });
      platform.stopCode();
    });
    return () => void used.then((u) => u());
  }, [invite, joined]);

  const show = async () => {
    setBusy(true);
    setError(null);
    retire();
    try {
      shared.current = false;
      setInvite(await platform.invite());
      setJoined(null);
      setCode(null);
      await platform.startCode();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  if (!invite) {
    return (
      <div className="pair-device">
        <p className="hint">
          Makes a one-time invite: a QR code for a phone's camera, a link to paste on another computer or into{" "}
          <code>yacs join</code> on a server, and a code to type.
        </p>
        {error && <p className="error">{error}</p>}
        <button onClick={show} disabled={busy}>
          {busy ? "Making an invite…" : "Invite a device…"}
        </button>
      </div>
    );
  }
  if (joined) {
    return (
      <div className="pair-device">
        <p className="paired">
          <span className="dot" /> {joined.device ? `${joined.device} joined.` : "The invite was used: a device joined."}
        </p>
        {error && <p className="error">{error}</p>}
        <div className="actions">
          <button onClick={show} disabled={busy}>
            {busy ? "Making an invite…" : "Invite another device…"}
          </button>
          <button onClick={hide}>Done</button>
        </div>
      </div>
    );
  }
  return (
    <div className="pair-device">
      <img className="qr" src={invite.qr} alt="Invite QR code" />
      {invite.warning && <p className="error">{invite.warning}</p>}
      <p className="hint">
        Scan it with the other device's camera, or send it the link. Works once, within 24 hours: whoever opens it first
        joins your space, so send the link only to the device you mean.
      </p>
      <div className="actions">
        <button
          onClick={async () => {
            await navigator.clipboard.writeText(invite.url);
            shared.current = true;
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          }}
        >
          {copied ? "Copied" : "Copy link"}
        </button>
        <button onClick={hide}>Hide</button>
      </div>
      {code?.kind === "code" && (
        <>
          <p className="divider">or type this code on the other device</p>
          <p className="code mono">{code.code}</p>
          <p className="hint">
            {code.replaced && "Someone typed a wrong code, so here's a new one. "}
            Works while this window is open. The other device also needs the relay: {relay}
          </p>
        </>
      )}
      {code?.kind === "failed" && <p className="hint">No code this time: {code.error}</p>}
    </div>
  );
}

/** Join your other devices' space with an invite link, or start a new one. */
function SetUpSpace() {
  const [link, setLink] = useState("");
  const [serverUrl, setServerUrl] = useState("");
  const [token, setToken] = useState("");
  const [busy, setBusy] = useState<"join" | "create" | null>(null);
  const [error, setError] = useState<{ form: "join" | "create"; text: string } | null>(null);

  const run = async (form: "join" | "create", action: () => Promise<void>) => {
    setBusy(form);
    setError(null);
    try {
      await action();
    } catch (e) {
      setError({ form, text: String(e) });
    } finally {
      setBusy(null);
    }
  };
  // A code (`7-tulip-apple`) doesn't say where its relay is, unlike a link.
  const isCode = /^\s*\d/.test(link);
  const join = (e: FormEvent) => {
    e.preventDefault();
    run("join", () => platform.joinSpace(link, isCode ? serverUrl : null));
  };
  const create = (e: FormEvent) => {
    e.preventDefault();
    run("create", () => platform.createSpace(serverUrl, token || null, null));
  };

  return (
    <>
      <form onSubmit={join}>
        <label>
          Invite link or code
          <input
            required
            value={link}
            onChange={(e) => setLink(e.target.value)}
            placeholder="https://…/#join=…  or  7-tulip-apple"
            autoComplete="off"
            spellCheck={false}
          />
          <span className="hint">On a computer in the space: Settings → Invite a device….</span>
        </label>
        {isCode && (
          <label>
            Relay URL <span className="optional">shown under the code</span>
            <input
              type="url"
              required
              placeholder="https://clip.example.com"
              value={serverUrl}
              onChange={(e) => setServerUrl(e.target.value)}
            />
          </label>
        )}
        {error?.form === "join" && <p className="error">{error.text}</p>}
        <div className="actions">
          <button className="primary" type="submit" disabled={busy !== null}>
            {busy === "join" ? "Joining…" : "Join space"}
          </button>
        </div>
      </form>
      <p className="divider">or start a new space</p>
      <form onSubmit={create}>
        <label>
          Relay URL
          <input
            type="url"
            required
            placeholder="https://clip.example.com"
            value={serverUrl}
            onChange={(e) => {
              // An invite link pasted here out of habit belongs above.
              if (/#(join|pair)=/.test(e.target.value)) {
                setLink(e.target.value.trim());
                setServerUrl("");
              } else {
                setServerUrl(e.target.value);
              }
            }}
          />
        </label>
        <label>
          Access token <span className="optional">if your relay requires one</span>
          <input type="password" value={token} onChange={(e) => setToken(e.target.value)} autoComplete="off" />
        </label>
        {error?.form === "create" && <p className="error">{error.text}</p>}
        <div className="actions">
          <button type="submit" disabled={busy !== null}>
            {busy === "create" ? "Starting…" : "Start a new space"}
          </button>
        </div>
        <p className="hint">Then invite your other devices from here.</p>
      </form>
    </>
  );
}

/** Typing a device name saves once this long has passed without a key. */
const NAME_SAVE_MS = 600;

/**
 * Each change saves right away (the device name once typing pauses), so
 * closing the window never leaves a change unsaved that looks applied.
 */
function PreferencesForm({ status, serverConfig }: { status: Status; serverConfig: ServerConfig | null }) {
  const fromStatus = (s: Status): Preferences => ({
    deviceName: s.deviceName,
    hotkey: s.hotkey,
    defaultTtlSecs: s.defaultTtlSecs,
    autostart: s.autostart,
  });
  const [prefs, setPrefs] = useState(() => fromStatus(status));
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const savedTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const nameTimer = useRef<ReturnType<typeof setTimeout>>(undefined);

  // Spotlight's dropdown also changes the default expiry.
  useEffect(() => {
    setPrefs((p) => ({ ...p, defaultTtlSecs: status.defaultTtlSecs }));
  }, [status.defaultTtlSecs]);

  /** Resolves to whether it saved. */
  const save = async (next: Preferences) => {
    clearTimeout(nameTimer.current);
    nameTimer.current = undefined;
    if (!next.deviceName.trim()) {
      setError("The device name can't be empty.");
      return false;
    }
    setError(null);
    try {
      await platform.savePreferences(next);
      setSaved(true);
      clearTimeout(savedTimer.current);
      savedTimer.current = setTimeout(() => setSaved(false), 1500);
      return true;
    } catch (e) {
      setError(String(e));
      return false;
    }
  };
  const set = <K extends keyof Preferences>(key: K, value: Preferences[K]) => {
    const next = { ...prefs, [key]: value };
    setPrefs(next);
    setSaved(false);
    if (key === "deviceName") {
      clearTimeout(nameTimer.current);
      nameTimer.current = setTimeout(() => save(next), NAME_SAVE_MS);
    } else {
      // A shortcut that's taken, say: show what's really in effect.
      save(next).then((ok) => ok || setPrefs((p) => ({ ...p, [key]: fromStatus(status)[key] })));
    }
  };
  const submit = (e: FormEvent) => {
    e.preventDefault();
    save(prefs);
  };

  const options = ttlChoices(prefs.defaultTtlSecs, serverConfig?.max_ttl_secs);

  return (
    <form onSubmit={submit}>
      <label>
        Device name <span className="optional">shown to your other devices</span>
        <input required value={prefs.deviceName} onChange={(e) => set("deviceName", e.target.value)} onBlur={() => nameTimer.current && save(prefs)} />
      </label>
      {status.manualShortcut ? (
        <ManualShortcutHelp shortcut={status.manualShortcut} />
      ) : (
        // Not a <label>: a click anywhere in one clicks the recorder, which
        // would start recording again.
        <div className="field" role="group" aria-labelledby="hotkey-label">
          <span id="hotkey-label">Shortcut to open YACS</span>
          <HotkeyRecorder value={prefs.hotkey} os={status.os} onChange={(v) => set("hotkey", v)} />
          {status.hotkeyError && <span className="error">{status.hotkeyError}</span>}
        </div>
      )}
      <label>
        Default expiry for sent clips
        <select value={prefs.defaultTtlSecs} onChange={(e) => set("defaultTtlSecs", Number(e.target.value))}>
          {options.map((o) => (
            <option key={o.secs} value={o.secs}>
              {o.label}
            </option>
          ))}
        </select>
      </label>
      <label className="check">
        <input type="checkbox" checked={prefs.autostart} onChange={(e) => set("autostart", e.target.checked)} />
        Launch YACS at login
      </label>
      {error && <p className="error">{error}</p>}
      <p className="hint autosave" role="status">
        {saved ? <span className="saved">Saved</span> : "Changes save automatically."}
      </p>
    </form>
  );
}

/** For tiling compositors, the config line to add; other desktops take the bare command. */
function configLine({ command, desktop }: ManualShortcut) {
  if (desktop === "hyprland")
    return { file: "~/.config/hypr/hyprland.conf", line: `bind = CTRL SHIFT, space, exec, ${command}` };
  if (desktop === "sway") return { file: "~/.config/sway/config", line: `bindsym Ctrl+Shift+space exec ${command}` };
  return null;
}

/**
 * Wayland doesn't let apps grab global shortcuts, so instead of the recorder
 * Settings shows how to bind `--toggle` on the desktop it detected.
 */
function ManualShortcutHelp({ shortcut }: { shortcut: ManualShortcut }) {
  const [copied, setCopied] = useState(false);
  const config = configLine(shortcut);
  const text = config?.line ?? shortcut.command;
  const copy = async () => {
    await navigator.clipboard.writeText(text);
    setCopied(true);
    setTimeout(() => setCopied(false), 1500);
  };

  return (
    <div className="notice">
      <strong>Shortcut to open YACS</strong>
      <p>
        Wayland doesn't let apps set their own global shortcuts.{" "}
        {config ? (
          <>
            Add this line to <code>{config.file}</code>:
          </>
        ) : (
          "Add one in your desktop's settings that runs:"
        )}
      </p>
      <div className="row">
        <code className="command">
          {/* Browsers break lines after hyphens, which would split the flag. */}
          {text.replace(/--toggle$/, "")}
          <span className="nowrap">--toggle</span>
        </code>
        <button type="button" onClick={copy}>
          {copied ? "Copied" : "Copy"}
        </button>
      </div>
      {shortcut.desktop === "gnome" && (
        <p>
          In Settings → Keyboard → View and Customize Shortcuts → Custom Shortcuts, click <b>Add Shortcut</b>, name it
          YACS, paste the command, and press a shortcut such as Ctrl+Shift+Space.
        </p>
      )}
      {shortcut.desktop === "kde" && (
        <p>
          In System Settings → Keyboard → Shortcuts, click <b>Add New</b> → <b>Command or Script</b>, paste the command,
          then set a shortcut such as Ctrl+Shift+Space.
        </p>
      )}
      {shortcut.desktop === "sway" && <p>Then reload sway (Mod+Shift+C).</p>}
      {shortcut.desktop === "other" && (
        <p>
          Look for custom or application shortcuts in your desktop's keyboard settings, and paste the command there.
        </p>
      )}
      {shortcut.appimage && <p className="hint">If you move the AppImage, update the path in the shortcut.</p>}
    </div>
  );
}

function HotkeyRecorder({ value, os, onChange }: { value: string; os: Os; onChange: (value: string) => void }) {
  const [recording, setRecording] = useState(false);
  const [hint, setHint] = useState<string | null>(null);

  // The global shortcut would swallow the current one before it gets here.
  useEffect(() => {
    if (!recording) return;
    platform.pauseHotkey();
    return () => void platform.resumeHotkey();
  }, [recording]);

  const onKeyDown = (e: KeyboardEvent<HTMLButtonElement>) => {
    if (!recording) return;
    e.preventDefault();
    if (e.key === "Escape") {
      setRecording(false);
      setHint(null);
      return;
    }
    const accelerator = acceleratorFromEvent(e.nativeEvent);
    if (accelerator) {
      onChange(accelerator);
      setRecording(false);
      setHint(null);
    } else if (!["Shift", "Control", "Alt", "Meta"].includes(e.key)) {
      setHint(os === "macos" ? "Include ⌘, ⌃ or ⌥" : "Include Ctrl, Alt or Win");
    }
  };

  return (
    <div className="row">
      <button
        type="button"
        className={recording ? "recorder recording" : "recorder"}
        onClick={() => setRecording(true)}
        onKeyDown={onKeyDown}
        onBlur={() => setRecording(false)}
      >
        {recording ? "Press a shortcut…" : formatAccelerator(value, os)}
      </button>
      {recording && <span className="hint">{hint ?? "Esc to cancel"}</span>}
    </div>
  );
}
