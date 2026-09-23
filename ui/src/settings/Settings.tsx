import { type FormEvent, type KeyboardEvent, useCallback, useEffect, useState } from "react";
import { platform } from "../platform";
import { acceleratorFromEvent, formatAccelerator } from "../shared/hotkey";
import { ttlChoices } from "../shared/time";
import type { Os, PhonePairing, Preferences, ServerConfig, Status } from "../shared/types";

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
    const subscription = platform.onStatusChanged(refresh);
    return () => void subscription.then((unsubscribe) => unsubscribe());
  }, [refresh]);

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
    </main>
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
      <PairPhone />
      <div className="actions">
        <button className="danger" onClick={unpair}>
          Unpair this device
        </button>
      </div>
    </>
  );
}

function PairPhone() {
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
      <div className="phone">
        <p className="hint">Scan a QR code with your phone's camera to open YACS there, already paired.</p>
        {error && <p className="error">{error}</p>}
        <button onClick={show}>Pair a phone…</button>
      </div>
    );
  }
  return (
    <div className="phone">
      <img className="qr" src={pairing.qr} alt="Pairing QR code" />
      {pairing.warning && <p className="error">{pairing.warning}</p>}
      <p className="hint">
        Anyone who scans this code can read and send your clips. Only show it to your own devices.
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
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await platform.pair(serverUrl, token || null, phrase);
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
          onChange={(e) => setServerUrl(e.target.value)}
        />
      </label>
      <label>
        Access token <span className="optional">if your relay requires one</span>
        <input type="password" value={token} onChange={(e) => setToken(e.target.value)} autoComplete="off" />
      </label>
      <label>
        Pairing phrase
        <div className="row">
          <input
            required
            value={phrase}
            onChange={(e) => {
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
      {error && <p className="error">{error}</p>}
      <div className="actions">
        <button className="primary" type="submit" disabled={busy}>
          {busy ? "Pairing…" : "Pair this device"}
        </button>
      </div>
    </form>
  );
}

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

  // Spotlight's dropdown also changes the default expiry.
  useEffect(() => {
    setPrefs((p) => ({ ...p, defaultTtlSecs: status.defaultTtlSecs }));
  }, [status.defaultTtlSecs]);

  const set = <K extends keyof Preferences>(key: K, value: Preferences[K]) => {
    setPrefs((p) => ({ ...p, [key]: value }));
    setSaved(false);
  };
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setError(null);
    try {
      await platform.savePreferences(prefs);
      setSaved(true);
    } catch (e) {
      setError(String(e));
    }
  };

  const options = ttlChoices(prefs.defaultTtlSecs, serverConfig?.max_ttl_secs);

  return (
    <form onSubmit={submit}>
      <label>
        Device name <span className="optional">shown to your other devices</span>
        <input required value={prefs.deviceName} onChange={(e) => set("deviceName", e.target.value)} />
      </label>
      <label>
        Shortcut to open YACS
        <HotkeyRecorder value={prefs.hotkey} os={status.os} onChange={(v) => set("hotkey", v)} />
        {status.hotkeyError && <span className="error">{status.hotkeyError}</span>}
      </label>
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
      <div className="actions">
        {saved && <span className="saved">Saved</span>}
        <button className="primary" type="submit">
          Save
        </button>
      </div>
    </form>
  );
}

function HotkeyRecorder({ value, os, onChange }: { value: string; os: Os; onChange: (value: string) => void }) {
  const [recording, setRecording] = useState(false);
  const [hint, setHint] = useState<string | null>(null);

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
