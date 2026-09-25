import { type FormEvent, type KeyboardEvent, useCallback, useEffect, useRef, useState } from "react";
import { platform } from "../platform";
import { acceleratorFromEvent, formatAccelerator } from "../shared/hotkey";
import { readPairLink } from "../shared/pairlink";
import { ttlChoices } from "../shared/time";
import type { ManualShortcut, Os, PhonePairing, Preferences, ServerConfig, Status } from "../shared/types";
import { relayBehind } from "../shared/version";

export function Settings() {
  const [status, setStatus] = useState<Status | null>(null);
  const [serverConfig, setServerConfig] = useState<ServerConfig | null>(null);

  const refresh = useCallback(async () => {
    const s = await platform.status();
    setStatus(s);
    setServerConfig(s.paired ? await platform.serverConfig().catch(() => null) : null);
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
        <h2>Pairing</h2>
        {status.paired ? <Paired status={status} /> : <PairForm />}
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
          {status.paired && ", paired like this computer"}.{" "}
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

function Paired({ status }: { status: Status }) {
  const [error, setError] = useState<string | null>(null);
  const unpair = async () => {
    if (!confirm("Unpair this device? You'll need the pairing phrase to pair it again.")) return;
    try {
      await platform.unpair();
    } catch (e) {
      setError(String(e));
    }
  };
  return (
    <>
      <p className="paired">
        <span className="dot" /> Paired through <strong>{status.serverUrl}</strong>
      </p>
      {error && <p className="error">{error}</p>}
      <PairDevice />
      <div className="actions">
        <button className="danger" onClick={unpair}>
          Unpair this device
        </button>
      </div>
    </>
  );
}

function PairDevice() {
  const [pairing, setPairing] = useState<PhonePairing | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [copied, setCopied] = useState(false);

  // Don't leave the key on screen when the window is hidden and shown again.
  useEffect(() => {
    const hide = () => document.visibilityState === "hidden" && setPairing(null);
    document.addEventListener("visibilitychange", hide);
    return () => document.removeEventListener("visibilitychange", hide);
  }, []);

  const show = async () => {
    setError(null);
    try {
      setPairing(await platform.phonePairing());
    } catch (e) {
      setError(String(e));
    }
  };

  if (!pairing) {
    return (
      <div className="pair-device">
        <p className="hint">
          Shows a QR code for a phone's camera, and a link to paste on another computer or into <code>yacs pair</code>{" "}
          on a server.
        </p>
        {error && <p className="error">{error}</p>}
        <button onClick={show}>Pair another device…</button>
      </div>
    );
  }
  return (
    <div className="pair-device">
      <img className="qr" src={pairing.qr} alt="Pairing QR code" />
      {pairing.warning && <p className="error">{pairing.warning}</p>}
      <p className="hint">
        Anyone with this code or link can read and send your clips. Only use it on your own devices.
      </p>
      <div className="actions">
        <button
          onClick={async () => {
            await navigator.clipboard.writeText(pairing.url);
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
          }}
        >
          {copied ? "Copied" : "Copy link"}
        </button>
        <button onClick={() => setPairing(null)}>Hide</button>
      </div>
    </div>
  );
}

function PairForm() {
  const [serverUrl, setServerUrl] = useState("");
  const [token, setToken] = useState("");
  const [phrase, setPhrase] = useState("");
  // From a pasted pairing link: replaces the phrase.
  const [linkSecret, setLinkSecret] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const generate = async () => {
    setPhrase(await platform.generatePhrase());
    setCopied(false);
  };
  const copy = async () => {
    await navigator.clipboard.writeText(phrase);
    setCopied(true);
  };
  /** A pairing link pasted into any field fills in the whole form. */
  const takeLink = (text: string) => {
    const link = readPairLink(text);
    if (!link) return false;
    setServerUrl(link.serverUrl);
    if (link.token) setToken(link.token);
    setLinkSecret(link.secret);
    setPhrase("");
    return true;
  };
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await platform.pair(serverUrl, token || null, linkSecret ?? phrase);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit}>
      <label>
        Relay URL
        <input
          type="url"
          required
          placeholder="https://clip.example.com"
          value={serverUrl}
          onChange={(e) => takeLink(e.target.value) || setServerUrl(e.target.value)}
        />
        <span className="hint">
          Or paste a pairing link here (on a paired computer: Settings → Pair another device… → Copy link).
        </span>
      </label>
      <label>
        Access token <span className="optional">if your relay requires one</span>
        <input type="password" value={token} onChange={(e) => setToken(e.target.value)} autoComplete="off" />
      </label>
      {linkSecret ? (
        <p className="hint">
          Pairing from the link.{" "}
          <button type="button" onClick={() => setLinkSecret(null)}>
            Use a phrase instead
          </button>
        </p>
      ) : (
        <label>
          Pairing phrase
          <div className="row phrase">
            <input
              required
              value={phrase}
              onChange={(e) => {
                if (takeLink(e.target.value)) return;
                setPhrase(e.target.value);
                setCopied(false);
              }}
              placeholder="six words from your other device"
              autoComplete="off"
              spellCheck={false}
              className="mono"
            />
            <button type="button" onClick={generate}>
              Generate
            </button>
            <button type="button" onClick={copy} disabled={!phrase}>
              {copied ? "Copied" : "Copy"}
            </button>
          </div>
          <span className="hint">
            Every device uses the same phrase. Generate one on your first device, then type it on the others. It never
            leaves your devices.
          </span>
        </label>
      )}
      {error && <p className="error">{error}</p>}
      <div className="actions">
        <button className="primary" type="submit" disabled={busy}>
          {busy ? "Pairing…" : "Pair this device"}
        </button>
      </div>
    </form>
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
