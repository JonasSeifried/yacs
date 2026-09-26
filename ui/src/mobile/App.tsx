import { type FormEvent, type ReactNode, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  DEFAULT_SPACE_NAME,
  type Decrypted,
  LiveUnsupported,
  type Session,
  type Stored,
  WebClient,
  enterSpace,
  forgetDownloads,
  joinWithCode,
  leaveSpace,
  loadStored,
  relayConfig,
  renameSpace,
  saveDeviceName,
  session,
  takeInvite,
} from "../platform/web";
import { EAGER_CONCURRENCY, loadsEagerly, runLimited } from "../shared/async";
import { clipTitle, isImageMime, previewDocument, previewKind } from "../shared/clip";
import { describeLimits, maxTtlSecs } from "../shared/plan";
import { inlineFileLimit, streamTotal } from "../shared/stream";
import { formatDuration, formatSize, ttlChoices } from "../shared/time";
import type { ClipItem, ClipMeta, ServerConfig, SpaceLimits } from "../shared/types";
import { canCopy, canSave, copyClip, fileItem, pick, readClipboard, savable, shareFiles } from "./clipboard";
import {
  type InviteLink,
  forgetInviteLink,
  guessDeviceName,
  inviteLinkFromCode,
  inviteUrl,
  isIos,
  isIosBrowserTab,
  looksLikeCode,
  parseInviteLink,
} from "./link";
import { canScan, qrDecoder } from "./qr";

const TTL_KEY = "yacs.ttl";
/** Sends bigger than this show a progress bar; smaller ones are over in a blink. */
const PROGRESS_BYTES = 256 * 1024;
/** While the app is open without a live connection, look for new clips this often. */
const POLL_MS = 10_000;
/** How often relative times ("2 min ago") are redrawn. */
const TICK_MS = 30_000;
const LIVE_RETRY_MIN_MS = 1_000;
const LIVE_RETRY_MAX_MS = 60_000;

// Read once at startup, then removed from the address bar.
const initialLink = parseInviteLink(location.hash);
if (initialLink) forgetInviteLink();

export function App() {
  const [stored, setStored] = useState<Stored | null>(loadStored);
  const [link, setLink] = useState<InviteLink | null>(initialLink);
  const current = session(stored);

  // An invite link opened while the app is already open only changes the hash.
  useEffect(() => {
    const onHash = () => {
      const next = parseInviteLink(location.hash);
      if (!next) return;
      forgetInviteLink();
      setLink(next);
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  if (link) {
    return (
      <JoinFromLink
        link={link}
        stored={stored}
        onDone={(next) => {
          setLink(null);
          if (next) setStored(next);
        }}
      />
    );
  }
  if (!stored || !current) return <Welcome stored={stored} onJoined={setStored} onLink={setLink} />;
  return <Home key={current.secret} stored={stored} current={current} onChange={setStored} />;
}

// ── Joining a space ────────────────────────────────────────────────────────

/**
 * The "Join?" step: nothing is fetched or stored until the tap, so a link
 * preview in a messenger can't use up the invite.
 */
function JoinFromLink(props: { link: InviteLink; stored: Stored | null; onDone: (s: Stored | null) => void }) {
  const { link } = props;
  const [deviceName, setDeviceName] = useState(() => props.stored?.deviceName ?? guessDeviceName());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const replacing = (props.stored?.spaces.length ?? 0) > 0;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      if (link.kind === "invite" || link.kind === "code") {
        const invite = link.kind === "invite" ? await takeInvite(link.secret) : await joinWithCode(link.code, deviceName);
        props.onDone(await enterSpace(invite.space, invite.name, invite.token, deviceName));
      } else {
        props.onDone(await enterSpace(link.secret, link.name ?? DEFAULT_SPACE_NAME, link.token, deviceName));
      }
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  };

  return (
    <Screen>
      <form className="card pair" onSubmit={submit}>
        <h1>{link.kind === "space" && link.name ? `Join “${link.name}”?` : "Join a space?"}</h1>
        <p className="muted">
          This device will share clips with the devices in it through <b>{location.host}</b>, end-to-end encrypted.
          {replacing && " This replaces the space this device is in now."}
        </p>
        <IosHomeScreenHint />
        <DeviceNameField value={deviceName} onChange={setDeviceName} />
        {error && <p className="error">{error}</p>}
        <button className="primary" disabled={busy}>
          {busy ? (link.kind === "code" ? "Waiting for the other device…" : "Joining…") : "Join"}
        </button>
        <button type="button" className="ghost" onClick={() => props.onDone(null)} disabled={busy}>
          Cancel
        </button>
      </form>
    </Screen>
  );
}

/** This relay's settings, read once, before this device is in a space. */
function useRelayConfig(): ServerConfig | null {
  const [config, setConfig] = useState<ServerConfig | null>(null);
  useEffect(() => {
    relayConfig().then(setConfig, () => {});
  }, []);
  return config;
}

function Welcome(props: { stored: Stored | null; onJoined: (s: Stored) => void; onLink: (l: InviteLink) => void }) {
  const relay = useRelayConfig();
  const [scanning, setScanning] = useState(false);
  const [pasted, setPasted] = useState("");
  const [starting, setStarting] = useState(false);
  const [token, setToken] = useState("");
  const [deviceName, setDeviceName] = useState(() => props.stored?.deviceName ?? guessDeviceName());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const join = (e: FormEvent) => {
    e.preventDefault();
    if (looksLikeCode(pasted)) {
      setError(null);
      return props.onLink({ kind: "code", code: pasted.trim() });
    }
    const link = inviteLinkFromCode(pasted);
    if ("error" in link) return setError(link.error);
    setError(null);
    props.onLink(link);
  };
  const start = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      props.onJoined(await enterSpace(null, DEFAULT_SPACE_NAME, token, deviceName));
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  };

  if (scanning) return <Scanner onLink={props.onLink} onCancel={() => setScanning(false)} />;

  if (starting) {
    return (
      <Screen>
        <form className="card pair" onSubmit={start}>
          <h1>Start a new space</h1>
          <p className="muted">
            For this device and the ones you invite, through <b>{location.host}</b>. Invite them from Settings once
            it's started.
            {relay?.accounts?.public && " It's free, for clips up to 10 MB kept up to an hour."}
          </p>
          {!relay?.accounts?.public && (
            <label>
              <span>Account key <span className="muted">if the relay has one</span></span>
              <input type="password" value={token} onChange={(e) => setToken(e.target.value)} autoCapitalize="none" autoComplete="off" />
            </label>
          )}
          <DeviceNameField value={deviceName} onChange={setDeviceName} />
          {error && <p className="error">{error}</p>}
          <button className="primary" disabled={busy}>
            {busy ? "Starting…" : "Start space"}
          </button>
          <button type="button" className="ghost" onClick={() => setStarting(false)} disabled={busy}>
            Back
          </button>
        </form>
        <LegalLinks config={relay} />
      </Screen>
    );
  }

  return (
    <Screen>
      <form className="card pair" onSubmit={join}>
        <h1>Join your devices</h1>
        <p className="muted">
          Scan the QR code in YACS on your computer (Settings → Invite a device), or paste the invite link or type the
          code shown there.
        </p>
        <IosHomeScreenHint />
        {canScan() && (
          <button type="button" className="primary" onClick={() => setScanning(true)}>
            Scan QR code
          </button>
        )}
        <label>
          Invite link or code
          <input
            value={pasted}
            onChange={(e) => setPasted(e.target.value)}
            required
            autoCapitalize="none"
            autoCorrect="off"
            autoComplete="off"
            spellCheck={false}
            placeholder="7-tulip-apple"
          />
        </label>
        {error && <p className="error">{error}</p>}
        <button className={canScan() ? "" : "primary"}>Continue</button>
        <button type="button" className="link" onClick={() => (setError(null), setStarting(true))}>
          No other devices yet? Start a new space
        </button>
      </form>
      <LegalLinks config={relay} />
    </Screen>
  );
}

/** The relay's privacy policy and imprint, if it has them. */
function LegalLinks({ config }: { config: ServerConfig | null }) {
  if (!config?.legal) return null;
  return (
    <p className="legal muted small">
      <a href="/privacy" target="_blank" rel="noreferrer">
        Privacy
      </a>
      {" · "}
      <a href="/imprint" target="_blank" rel="noreferrer">
        Imprint
      </a>
    </p>
  );
}

function DeviceNameField(props: { value: string; onChange: (value: string) => void }) {
  return (
    <label>
      <span>Device name <span className="muted">shown to your other devices</span></span>
      <input value={props.value} onChange={(e) => props.onChange(e.target.value)} required />
    </label>
  );
}

function Scanner({ onLink, onCancel }: { onLink: (l: InviteLink) => void; onCancel: () => void }) {
  const video = useRef<HTMLVideoElement>(null);
  const [error, setError] = useState<string | null>(null);
  const [problem, setProblem] = useState<string | null>(null);
  const onLinkRef = useRef(onLink);
  onLinkRef.current = onLink;

  useEffect(() => {
    let stopped = false;
    let stream: MediaStream | undefined;
    let timer: ReturnType<typeof setTimeout> | undefined;
    (async () => {
      try {
        const decode = await qrDecoder();
        stream = await navigator.mediaDevices.getUserMedia({ video: { facingMode: "environment" }, audio: false });
        const el = video.current;
        if (stopped || !el) return;
        el.srcObject = stream;
        await el.play();
        const tick = async () => {
          if (stopped) return;
          const code = await decode(el).catch(() => null);
          if (stopped) return;
          if (code) {
            const link = inviteLinkFromCode(code);
            if (!("error" in link)) return onLinkRef.current(link);
            setProblem(link.error);
          }
          timer = setTimeout(tick, 150);
        };
        tick();
      } catch (e) {
        if (!stopped) setError(cameraError(e));
      }
    })();
    return () => {
      stopped = true;
      clearTimeout(timer);
      for (const track of stream?.getTracks() ?? []) track.stop();
    };
  }, []);

  return (
    <Screen>
      <div className="card pair">
        <h1>Scan the QR code</h1>
        <p className="muted">On your computer: YACS → Settings → Invite a device.</p>
        {error ? (
          <p className="error">{error}</p>
        ) : (
          <video ref={video} className="scanner" playsInline muted autoPlay />
        )}
        {problem && !error && <p className="error">{problem}</p>}
        <button type="button" className="ghost" onClick={onCancel}>
          {error ? "Back" : "Cancel"}
        </button>
      </div>
    </Screen>
  );
}

function cameraError(e: unknown): string {
  const name = e instanceof DOMException ? e.name : "";
  if (name === "NotAllowedError") return "YACS isn't allowed to use the camera. Allow it in your browser's settings, or paste the invite link instead.";
  if (name === "NotFoundError" || name === "OverconstrainedError") return "No camera found. Paste the invite link instead.";
  return errorText(e);
}

/** Joining in an iPhone browser tab doesn't carry over to the home screen app. */
function IosHomeScreenHint() {
  if (!isIosBrowserTab()) return null;
  return (
    <p className="muted small">
      Want YACS on your home screen? Add it there first (Share → Add to Home Screen), then join from inside it: the
      home screen app doesn't share anything with the browser.
    </p>
  );
}

/**
 * Keeps a live connection to the relay while the app is on screen, calling
 * `onChange` when the history changes. Phones suspend background pages
 * anyway, so it disconnects when hidden. The returned ref says whether it's
 * connected; polling covers the rest.
 */
function useLiveUpdates(client: WebClient, onChange: () => void) {
  const connected = useRef(false);
  const onChangeRef = useRef(onChange);
  onChangeRef.current = onChange;

  useEffect(() => {
    let stopped = false;
    let controller: AbortController | null = null;
    let timer: ReturnType<typeof setTimeout> | undefined;
    let retry = LIVE_RETRY_MIN_MS;

    const connect = async () => {
      clearTimeout(timer);
      if (stopped || controller || document.visibilityState !== "visible") return;
      const current = new AbortController();
      controller = current;
      const started = Date.now();
      let unsupported = false;
      try {
        await client.listen(
          current.signal,
          () => {
            connected.current = true;
            onChangeRef.current();
          },
          () => onChangeRef.current(),
        );
      } catch (e) {
        unsupported = e instanceof LiveUnsupported;
      } finally {
        connected.current = false;
        if (controller === current) controller = null;
      }
      // Old relays get polled; the next time the app is shown, it asks again.
      if (stopped || unsupported || current.signal.aborted) return;
      if (Date.now() - started > LIVE_RETRY_MAX_MS) retry = LIVE_RETRY_MIN_MS;
      timer = setTimeout(connect, retry);
      retry = Math.min(retry * 2, LIVE_RETRY_MAX_MS);
    };
    const onVisibility = () => {
      if (document.visibilityState === "visible") {
        retry = LIVE_RETRY_MIN_MS;
        connect();
      } else {
        clearTimeout(timer);
        controller?.abort();
      }
    };

    connect();
    document.addEventListener("visibilitychange", onVisibility);
    return () => {
      stopped = true;
      clearTimeout(timer);
      controller?.abort();
      document.removeEventListener("visibilitychange", onVisibility);
    };
  }, [client]);

  return connected;
}

// ── Home: send + history ─────────────────────────────────────────────────

/** A failed refresh keeps the clips it had, with `error` set, instead of hiding them. */
type List =
  | { state: "loading" }
  | { state: "ok"; clips: ClipMeta[]; error?: string }
  | { state: "error"; message: string };
/** A big upload or download in progress. */
type Progress = { done: number; total: number; cancel: () => void };

/** An upload the user cancelled: not an error to show. */
class Cancelled extends Error {}
type Loaded = { state: "loading" } | { state: "ok"; clip: Decrypted } | { state: "gone" } | { state: "error"; message: string };
/** `undo`: the clip just deleted, which the toast's Undo brings back. */
type Toast = { kind: "ok" | "error"; text: string; undo?: string };

/** A delete waits this long for Undo before it goes to the relay (for every device). */
const UNDO_MS = 5000;

function Home({ stored, current, onChange }: { stored: Stored; current: Session; onChange: (s: Stored) => void }) {
  const { secret, token, deviceName } = current;
  const client = useMemo(() => new WebClient({ secret, token, deviceName }), [secret, token, deviceName]);
  const [config, setConfig] = useState<ServerConfig | null>(null);
  const [limits, setLimits] = useState<SpaceLimits | null>(null);
  const [list, setList] = useState<List>({ state: "loading" });
  const [loaded, setLoaded] = useState<Record<string, Loaded>>({});
  const [openId, setOpenId] = useState<string | null>(null);
  const [ttl, setTtl] = useState(() => Number(localStorage.getItem(TTL_KEY)) || 15 * 60);
  const [toast, setToast] = useState<Toast | null>(null);
  const [settings, setSettings] = useState(false);
  const [now, setNow] = useState(Date.now());
  const [upload, setUpload] = useState<Progress | null>(null);
  const [drafted, setDrafted] = useState(false);
  const outdated = useOutdated(config, upload === null && !drafted);
  const requested = useRef(new Set<string>());
  /** Deleted here but not yet on the relay, with the timer that sends it. */
  const [deleting, setDeleting] = useState<string[]>([]);
  const pendingDeletes = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const toastTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  useTransfer(upload !== null);

  // Files a closed app downloaded but never shared.
  useEffect(() => {
    forgetDownloads();
  }, []);

  const notify = useCallback((kind: Toast["kind"], text: string, undo?: string) => {
    clearTimeout(toastTimer.current);
    setToast({ kind, text, undo });
    toastTimer.current = setTimeout(() => setToast(null), undo ? UNDO_MS : kind === "ok" ? 2500 : 6000);
  }, []);

  const load = useCallback(
    async (id: string) => {
      if (requested.current.has(id)) return;
      requested.current.add(id);
      setLoaded((l) => ({ ...l, [id]: { state: "loading" } }));
      let result: Loaded;
      try {
        const clip = await client.get(id);
        result = clip ? { state: "ok", clip } : { state: "gone" };
      } catch (e) {
        requested.current.delete(id);
        result = { state: "error", message: errorText(e) };
      }
      setLoaded((l) => ({ ...l, [id]: result }));
    },
    [client],
  );

  const refresh = useCallback(async () => {
    setNow(Date.now());
    client.config().then(setConfig, () => {});
    client.limits().then(setLimits, () => {});
    try {
      const listed = await client.list();
      setList({ state: "ok", clips: listed });
      const ids = new Set(listed.map((c) => c.id));
      for (const id of requested.current) if (!ids.has(id)) requested.current.delete(id);
      setLoaded((l) => Object.fromEntries(Object.entries(l).filter(([id]) => ids.has(id))));
      if (listed[0]) load(listed[0].id);
      runLimited(
        listed.filter(loadsEagerly).map((c) => () => load(c.id)),
        EAGER_CONCURRENCY,
      );
    } catch (e) {
      const message = errorText(e);
      setList((l) => (l.state === "ok" ? { ...l, error: message } : { state: "error", message }));
    }
  }, [client, load]);

  const live = useLiveUpdates(client, refresh);

  useEffect(() => {
    refresh();
    const onVisible = () => document.visibilityState === "visible" && refresh();
    document.addEventListener("visibilitychange", onVisible);
    const poll = setInterval(() => document.visibilityState === "visible" && !live.current && refresh(), POLL_MS);
    return () => {
      document.removeEventListener("visibilitychange", onVisible);
      clearInterval(poll);
    };
  }, [refresh, live]);

  // "2 min ago" keeps counting while live updates make polling unnecessary,
  // and clips drop out when they expire (the relay sends no event for that).
  useEffect(() => {
    const tick = setInterval(() => document.visibilityState === "visible" && setNow(Date.now()), TICK_MS);
    return () => clearInterval(tick);
  }, []);

  const clips =
    list.state === "ok" ? list.clips.filter((c) => c.expires_at_ms > now && !deleting.includes(c.id)) : [];
  const open = clips.find((c) => c.id === openId) ?? clips[0] ?? null;
  useEffect(() => {
    if (open) load(open.id);
  }, [open, load]);

  const changeTtl = (secs: number) => {
    setTtl(secs);
    localStorage.setItem(TTL_KEY, String(secs));
  };

  /** Shows the progress bar while `work` runs. */
  const withProgress = async (work: (progress: (done: number, total: number) => void, signal: AbortSignal) => Promise<Decrypted>) => {
    const controller = new AbortController();
    setUpload({ done: 0, total: 0, cancel: () => controller.abort() });
    try {
      return await work((done, total) => setUpload((u) => u && { ...u, done, total }), controller.signal);
    } catch (e) {
      throw controller.signal.aborted ? new Cancelled() : e;
    } finally {
      setUpload(null);
    }
  };

  const send = async (items: ClipItem[], bigFiles: File[] = []) => {
    const sent =
      bigFiles.length && config ? await withProgress((progress, signal) => client.sendBig(items, bigFiles, ttl, config, progress, signal))
      : itemBytes(items) > PROGRESS_BYTES ? await withProgress((progress, signal) => client.send(items, ttl, progress, signal))
      : await client.send(items, ttl);
    const id = sent.view.meta.id;
    requested.current.add(id);
    setLoaded((l) => ({ ...l, [id]: { state: "ok", clip: sent } }));
    setList((l) => ({ state: "ok", clips: [sent.view.meta, ...(l.state === "ok" ? l.clips : [])] }));
    setOpenId(id);
    notify("ok", `Sent · expires in ${formatDuration(ttl * 1000)}`);
  };

  const commitDelete = useCallback(
    async (id: string) => {
      clearTimeout(pendingDeletes.current.get(id));
      pendingDeletes.current.delete(id);
      try {
        await client.delete(id);
        setList((l) => (l.state === "ok" ? { ...l, clips: l.clips.filter((c) => c.id !== id) } : l));
      } catch (e) {
        notify("error", errorText(e));
      } finally {
        setDeleting((d) => d.filter((x) => x !== id)); // back in the list if it failed
      }
    },
    [client, notify],
  );

  /** Hidden at once; deleted for every device unless Undo comes first. */
  const remove = (id: string) => {
    setDeleting((d) => [...d, id]);
    pendingDeletes.current.set(id, setTimeout(() => commitDelete(id), UNDO_MS));
    notify("ok", "Deleted", id);
  };

  const undoDelete = (id: string) => {
    clearTimeout(pendingDeletes.current.get(id));
    pendingDeletes.current.delete(id);
    setDeleting((d) => d.filter((x) => x !== id));
    setOpenId(id);
    clearTimeout(toastTimer.current);
    setToast(null);
  };

  // A phone may freeze a page in the background: send pending deletes first.
  useEffect(() => {
    const onHidden = () =>
      document.visibilityState === "hidden" && [...pendingDeletes.current.keys()].forEach(commitDelete);
    document.addEventListener("visibilitychange", onHidden);
    return () => document.removeEventListener("visibilitychange", onHidden);
  }, [commitDelete]);

  return (
    <Screen>
      <header className="top">
        <span className="brand">YACS</span>
        <span className="server" title={`Through ${location.host}`}>
          {stored.spaces[0].name}
        </span>
        <button className="icon" aria-label="Settings" onClick={() => setSettings(true)}>
          <GearIcon />
        </button>
      </header>

      {outdated && (
        <div className="card update">
          <span>There's a new version of YACS.</span>
          <button className="primary" onClick={() => location.reload()}>
            Reload
          </button>
        </div>
      )}

      <Composer
        ttl={ttl}
        ttlChoices={ttlChoices(ttl, maxTtlSecs(config, limits))}
        config={config}
        maxClipBytes={limits?.max_clip_bytes ?? null}
        upload={upload}
        onDrafted={setDrafted}
        onTtl={changeTtl}
        onSend={send}
        onError={(text) => notify("error", text)}
      />

      <section className="history">
        <div className="history-head">
          <h2>History</h2>
          <button className="link" onClick={refresh}>
            Refresh
          </button>
        </div>
        {list.state === "loading" && <p className="muted center">Loading…</p>}
        {list.state === "error" && <p className="error center">{list.message}</p>}
        {list.state === "ok" && list.error && <p className="error center">Couldn't refresh: {list.error}</p>}
        {list.state === "ok" && clips.length === 0 && (
          <p className="muted center">No clips yet. Send one from here or from another device.</p>
        )}
        {clips.map((meta) => (
          <ClipCard
            key={meta.id}
            meta={meta}
            loaded={loaded[meta.id]}
            now={now}
            open={meta.id === open?.id}
            onOpen={() => setOpenId(meta.id)}
            onCopied={() => notify("ok", "Copied")}
            onDelete={() => remove(meta.id)}
            onError={(text) => notify("error", text)}
            onSaved={(text) => notify("ok", text)}
            client={client}
          />
        ))}
      </section>

      {toast && (
        <div className={`toast ${toast.kind}`} role="status">
          {toast.text}
          {toast.undo && (
            <button className="toast-action" onClick={() => undoDelete(toast.undo!)}>
              Undo
            </button>
          )}
        </div>
      )}
      {settings && (
        <SettingsSheet
          stored={stored}
          client={client}
          config={config}
          limits={limits}
          onClose={() => setSettings(false)}
          onChange={onChange}
        />
      )}
    </Screen>
  );
}

function Composer(props: {
  ttl: number;
  ttlChoices: { secs: number; label: string }[];
  /** The relay's limits, once known. */
  config: ServerConfig | null;
  /** The space's plan limit per clip, if it has one. */
  maxClipBytes: number | null;
  upload: Progress | null;
  /** Whether there's something typed or picked, which a reload would lose. */
  onDrafted: (drafted: boolean) => void;
  onTtl: (secs: number) => void;
  /** `bigFiles` go as chunks. */
  onSend: (items: ClipItem[], bigFiles?: File[]) => Promise<void>;
  onError: (text: string) => void;
}) {
  const [text, setText] = useState("");
  const [files, setFiles] = useState<File[]>([]);
  const [busy, setBusy] = useState(false);
  const shownImage = files.length === 1 && isImageMime(files[0].type) ? files[0] : null;
  const imageUrl = useObjectUrl(shownImage);
  const picker = useRef<HTMLInputElement>(null);

  // Android "Share → YACS": the service worker left the content in a cache.
  useEffect(() => {
    if (!new URLSearchParams(location.search).has("shared")) return;
    history.replaceState(null, "", "/");
    (async () => {
      const cache = await caches.open("yacs-shared");
      const sharedText = await (await cache.match("/shared/text"))?.text();
      const sharedFiles: File[] = [];
      for (const request of await cache.keys()) {
        if (!new URL(request.url).pathname.startsWith("/shared/file/")) continue;
        const res = await cache.match(request);
        if (!res) continue;
        const name = decodeURIComponent(res.headers.get("x-yacs-name") ?? "") || "shared";
        const blob = await res.blob();
        sharedFiles.push(new File([blob], name, { type: blob.type }));
      }
      await caches.delete("yacs-shared");
      if (sharedText) setText(sharedText);
      if (sharedFiles.length) setFiles(sharedFiles);
    })().catch(() => {});
  }, []);

  const run = async (items: () => Promise<ClipItem[]>, bigFiles: File[] = []) => {
    setBusy(true);
    try {
      const got = await items();
      if (got.length === 0 && bigFiles.length === 0) throw new Error("Nothing to send: the clipboard is empty.");
      await props.onSend(got, bigFiles);
      setText("");
      setFiles([]);
    } catch (e) {
      // Cancelled on purpose: the draft stays, ready to send again.
      if (!(e instanceof Cancelled)) props.onError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const total = files.reduce((sum, f) => sum + f.size, 0);
  const limit = props.config ? inlineFileLimit(props.config) : null;
  // Too big for the clip itself: the files go as chunks, if the relay takes them.
  const big = limit !== null && total > limit;
  const tooLarge = big && !props.config?.chunked;
  const overPlan = props.maxClipBytes !== null && total > props.maxClipBytes;
  // Until the relay's limits are known, it's unclear whether files go as chunks.
  const waiting = files.length > 0 && props.config === null;
  const drafted = text.trim() !== "" || files.length > 0;
  const { onDrafted } = props;
  useEffect(() => onDrafted(drafted), [drafted, onDrafted]);
  const textItems = (): ClipItem[] => (text.trim() ? [{ Text: text }] : []);
  return (
    <section className="card composer">
      {drafted ? (
        <>
          {imageUrl && <img className="draft-image" src={imageUrl} alt="" />}
          {files.length > 0 && !shownImage && (
            <ul className="draft-files">
              {files.map((f, i) => (
                <li key={i}>
                  <span className="file-name">{f.name}</span>
                  <span className="muted">{formatSize(f.size)}</span>
                </li>
              ))}
            </ul>
          )}
          {overPlan && (
            <p className="error small">
              {formatSize(total)} is more than this space takes per clip ({formatSize(props.maxClipBytes!)}).
            </p>
          )}
          {tooLarge && !overPlan && (
            <p className="error small">
              {formatSize(total)} is more than this relay takes per clip ({formatSize(limit!)}). Update the relay to
              send bigger files.
            </p>
          )}
          {props.upload ? (
            <TransferProgress verb="Sending" progress={props.upload}>
              Keep this screen open until the upload finishes.
            </TransferProgress>
          ) : (
            <button
              className="primary"
              disabled={busy || tooLarge || overPlan || waiting}
              onClick={() =>
                big ? run(async () => textItems(), files)
                : run(async () => [...textItems(), ...(await Promise.all(files.map(fileItem)))])
              }
            >
              {busy ? "Sending…" : "Send"}
            </button>
          )}
        </>
      ) : (
        <button className="primary" disabled={busy} onClick={() => run(readClipboard)}>
          {busy ? "Sending…" : "Paste & send clipboard"}
        </button>
      )}
      <textarea
        value={text}
        onChange={(e) => setText(e.target.value)}
        rows={drafted ? 4 : 2}
        placeholder="…or type something to send"
      />
      <div className="composer-row">
        <button className="ghost" onClick={() => picker.current?.click()} disabled={busy}>
          {files.length ? "Change files" : "Add files"}
        </button>
        {drafted && (
          <button
            className="ghost"
            disabled={busy}
            onClick={() => {
              setText("");
              setFiles([]);
            }}
          >
            Clear
          </button>
        )}
        <label className="ttl">
          Expires in
          <select value={props.ttl} onChange={(e) => props.onTtl(Number(e.target.value))}>
            {props.ttlChoices.map((o) => (
              <option key={o.secs} value={o.secs}>
                {o.label}
              </option>
            ))}
          </select>
        </label>
      </div>
      <input
        ref={picker}
        type="file"
        multiple
        hidden
        onChange={(e) => {
          setFiles([...(e.target.files ?? [])]);
          e.target.value = "";
        }}
      />
    </section>
  );
}

function ClipCard(props: {
  meta: ClipMeta;
  loaded: Loaded | undefined;
  now: number;
  open: boolean;
  onOpen: () => void;
  onCopied: () => void;
  onDelete: () => void;
  onError: (text: string) => void;
  onSaved: (text: string) => void;
  client: WebClient;
}) {
  const { meta, loaded, now, open } = props;
  const [download, setDownload] = useState<Progress | null>(null);
  /** Downloaded files waiting for a tap to share them (iOS wants a fresh one). */
  const [ready, setReady] = useState<File[] | null>(null);
  useTransfer(download !== null);
  const ago = `${formatDuration(now - meta.created_at_ms)} ago`;
  const expires = `expires in ${formatDuration(meta.expires_at_ms - now)}`;
  const decrypted = loaded?.state === "ok" ? loaded.clip : null;

  const title =
    decrypted ? clipTitle(decrypted.view)
    : loaded?.state === "error" ? "Can't decrypt this clip"
    : loaded?.state === "gone" ? "Expired"
    : "Encrypted clip";
  const detail = decrypted ? `${decrypted.view.deviceName} · ${ago}` : `${ago} · ${formatSize(meta.size)}`;

  // Tapping Copy must reach clipboard.write without awaiting anything (Safari).
  const copy = () => {
    if (!decrypted) return;
    copyClip(decrypted.clip).then(props.onCopied, (e) => props.onError(errorText(e)));
  };

  const stream = decrypted ? pick(decrypted.clip, "Stream") : undefined;
  /** Chunked files, one after another. */
  const fetchFiles = async () => {
    if (!stream) return;
    const controller = new AbortController();
    const total = streamTotal(stream);
    setDownload({ done: 0, total, cancel: () => controller.abort() });
    const files: File[] = [];
    let before = 0;
    try {
      for (const [i, info] of stream.files.entries()) {
        const progress = (done: number) => setDownload((d) => d && { ...d, done: before + done });
        const file = await props.client.download(meta.id, stream, i, progress, controller.signal);
        if (file) files.push(file);
        before += info.size;
      }
      if (files.length) setReady(files);
      else props.onSaved(stream.files.length === 1 ? "Saved to your downloads" : `${stream.files.length} files saved to your downloads`);
    } catch (e) {
      if (!controller.signal.aborted) props.onError(errorText(e));
      forgetDownloads(meta.id);
    } finally {
      setDownload(null);
    }
  };
  const share = (files: File[]) =>
    shareFiles(files)
      .then((done) => {
        setReady(null);
        // A browser download may still be reading them; the next start cleans up then.
        if (done) forgetDownloads(meta.id);
      })
      .catch((e) => props.onError(errorText(e)));

  return (
    <article className={open ? "card clip open" : "card clip"} onClick={open ? undefined : props.onOpen}>
      <div className="clip-head">
        <span className={decrypted ? "clip-title" : "clip-title muted"}>{title}</span>
        <span className="clip-meta">{detail}</span>
      </div>
      {open && (
        <>
          <div className="clip-body">
            {!loaded || loaded.state === "loading" ? (
              <p className="muted">Decrypting…</p>
            ) : loaded.state === "error" ? (
              <p className="error">{loaded.message}</p>
            ) : loaded.state === "gone" ? (
              <p className="muted">This clip expired or was deleted.</p>
            ) : (
              <ClipPreview clip={loaded.clip} />
            )}
            {download && (
              <TransferProgress verb="Downloading" progress={download}>
                Keep this screen open until the download finishes.
              </TransferProgress>
            )}
            {ready && (
              <p className="muted small">
                {isIos() ? "Downloaded. Tap Share to save it to Files or Photos, or send it to an app." : "Downloaded. Tap Save to keep it."}
              </p>
            )}
          </div>
          <div className="clip-actions">
            {(!decrypted || canCopy(decrypted.clip)) && (
              <button className="primary" onClick={copy} disabled={!decrypted}>
                Copy
              </button>
            )}
            {decrypted && canSave(decrypted.clip) && !download && (
              <button
                className={canCopy(decrypted.clip) ? "ghost" : "primary"}
                onClick={() =>
                  ready ? share(ready)
                  : stream ? fetchFiles()
                  : shareFiles(savable(decrypted.clip, `yacs-${meta.id}`)).catch((e) => props.onError(errorText(e)))
                }
              >
                {ready ? (isIos() ? "Share" : "Save")
                : stream ? `Download ${formatSize(streamTotal(stream))}`
                : isIos() ? "Save / Share"
                : "Save"}
              </button>
            )}
            <button className="ghost danger" onClick={props.onDelete}>
              Delete
            </button>
            <span className="clip-meta right">{expires}</span>
          </div>
        </>
      )}
    </article>
  );
}

function ClipPreview({ clip }: { clip: Decrypted }) {
  const files = clip.view.files;
  const imageFile = files.length === 1 && isImageMime(files[0].mime) ? pick(clip.clip, "File") : undefined;
  const image = pick(clip.clip, "Image") ?? imageFile;
  const blob = useMemo(
    () => (image ? new Blob([image.data as Uint8Array<ArrayBuffer>], { type: image.mime }) : null),
    [image],
  );
  const url = useObjectUrl(blob);
  const kind = previewKind(clip.view);
  if (kind === "files") {
    return (
      <>
        {imageFile && url && <img className="clip-image" src={url} alt="" />}
        <ul className="clip-files">
          {files.map((f, i) => (
            <li key={i}>
              <span className="file-name">{f.name}</span>
              <span className="muted">{formatSize(f.size)}</span>
            </li>
          ))}
        </ul>
      </>
    );
  }
  if (kind === "image") return url ? <img className="clip-image" src={url} alt="" /> : null;
  if (kind === "html" && clip.view.html !== null) {
    return <iframe className="clip-html" sandbox="" srcDoc={previewDocument(clip.view.html)} title="Preview" />;
  }
  if (kind === "text") return <pre className="clip-text">{clip.view.text}</pre>;
  return <p className="muted">Rich text without a preview. Copy works in apps that take it.</p>;
}

function SettingsSheet(props: {
  stored: Stored;
  client: WebClient;
  config: ServerConfig | null;
  limits: SpaceLimits | null;
  onClose: () => void;
  onChange: (s: Stored) => void;
}) {
  const space = props.stored.spaces[0];
  const [name, setName] = useState(space.name);
  const [deviceName, setDeviceName] = useState(props.stored.deviceName);
  const { onClose } = props;
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => e.key === "Escape" && onClose();
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);
  return (
    <div className="sheet-backdrop" onClick={onClose}>
      <div className="sheet" role="dialog" aria-modal="true" aria-labelledby="sheet-title" onClick={(e) => e.stopPropagation()}>
        <div className="sheet-head">
          <h2 id="sheet-title">Settings</h2>
          <button className="link" onClick={onClose}>
            Close
          </button>
        </div>
        <label>
          <span>Space name <span className="muted">only on this device</span></span>
          <input value={name} onChange={(e) => setName(e.target.value)} maxLength={64} />
        </label>
        <label>
          Device name
          <input value={deviceName} onChange={(e) => setDeviceName(e.target.value)} />
        </label>
        <button
          className="primary"
          onClick={() => {
            props.onChange(saveDeviceName(renameSpace(props.stored, name), deviceName));
            props.onClose();
          }}
        >
          Save
        </button>
        <InviteDevice client={props.client} spaceName={space.name} />
        {describeLimits(props.limits) && <p className="muted small">Free plan: {describeLimits(props.limits)}.</p>}
        <p className="muted small">
          Install YACS: in Safari tap Share → Add to Home Screen; in Chrome use “Install app”. On Android, installed
          YACS shows up in the share sheet.
        </p>
        <button
          className="ghost danger"
          onClick={() => {
            if (!confirm(`Leave “${space.name}”? To come back, you'll need an invite from one of its devices.`)) return;
            props.onChange(leaveSpace(props.stored));
          }}
        >
          Leave this space
        </button>
        <LegalLinks config={props.config} />
      </div>
    </div>
  );
}

/** Hands out the invite link: share it to a messenger, or copy it for a computer's Settings. */
/**
 * Makes a one-time invite, then hands its link out: share it to a messenger,
 * or copy it for a computer's Settings. Two taps, since browsers only share
 * and copy straight from a tap, not after waiting for the relay.
 */
function InviteDevice({ client, spaceName }: { client: WebClient; spaceName: string }) {
  const [url, setUrl] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [status, setStatus] = useState<string | null>(null);
  const create = async () => {
    setBusy(true);
    setStatus(null);
    try {
      setUrl(inviteUrl(await client.invite(spaceName)));
    } catch (e) {
      setStatus(errorText(e));
    } finally {
      setBusy(false);
    }
  };
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(url!);
      setStatus("Copied. Paste it into Settings → Invite link on the other device.");
    } catch {
      setStatus("Couldn't copy here (the clipboard needs HTTPS).");
    }
  };
  const share = async () => {
    try {
      await navigator.share({ title: `Join “${spaceName}” in YACS`, url: url! });
    } catch (e) {
      if (!(e instanceof DOMException && e.name === "AbortError")) setStatus(errorText(e));
    }
  };
  return (
    <div className="invite">
      <span>Invite a device</span>
      <p className="muted small">
        {url
          ? "The link works once, within 24 hours. Whoever opens it first joins, so send it only to the device you mean."
          : "Makes a link that adds one device to this space."}
      </p>
      <div className="row">
        {!url ? (
          <button type="button" onClick={create} disabled={busy}>
            {busy ? "Making a link…" : "Make an invite link"}
          </button>
        ) : (
          <>
            {"share" in navigator && (
              <button type="button" onClick={share}>
                Share link
              </button>
            )}
            <button type="button" onClick={copy}>
              Copy link
            </button>
          </>
        )}
      </div>
      {status && <p className="muted small">{status}</p>}
    </div>
  );
}

// ── Bits ────────────────────────────────────────────────────────────────

function Screen({ children }: { children: ReactNode }) {
  return <main className="screen">{children}</main>;
}

function TransferProgress(props: { verb: string; progress: Progress; children: ReactNode }) {
  const { done, total, cancel } = props.progress;
  const percent = total > 0 ? Math.floor((done / total) * 100) : 0;
  return (
    <div className="transfer">
      <div className="transfer-text">
        <span>
          {props.verb} · {percent}%
        </span>
        <span className="muted">
          {formatSize(done)} of {formatSize(total)}
        </span>
        <button className="link" onClick={cancel}>
          Cancel
        </button>
      </div>
      <div className="transfer-track">
        <div className="transfer-fill" style={{ width: `${percent}%` }} />
      </div>
      <p className="muted small">{props.children}</p>
    </div>
  );
}

/** Uploads and downloads running now; the app doesn't reload itself during one. */
let transfers = 0;

/**
 * For a transfer while `active`: counts it, and keeps the screen on, since
 * phones pause pages whose screen turns off.
 */
function useTransfer(active: boolean) {
  useEffect(() => {
    if (!active) return;
    transfers++;
    return () => {
      transfers--;
    };
  }, [active]);
  useEffect(() => {
    if (!active || !("wakeLock" in navigator)) return;
    let lock: WakeLockSentinel | null = null;
    let stopped = false;
    const acquire = async () => {
      if (stopped || document.visibilityState !== "visible") return;
      try {
        lock = await navigator.wakeLock.request("screen");
      } catch {
        // Not allowed right now; the transfer goes on regardless.
      }
    };
    acquire();
    // The lock goes when the page is hidden; take it again on return.
    document.addEventListener("visibilitychange", acquire);
    return () => {
      stopped = true;
      document.removeEventListener("visibilitychange", acquire);
      lock?.release().catch(() => {});
    };
  }, [active]);
}

/**
 * Whether the relay runs another version than this page: the phone kept an
 * old copy of the app (a home screen app can stay open for days), which may
 * not read newer clips. Reloads by itself once per version when `idle` and
 * on screen; otherwise the page offers a Reload button.
 */
function useOutdated(config: ServerConfig | null, idle: boolean): boolean {
  const relay = config?.version;
  const outdated = relay !== undefined && relay !== __APP_VERSION__;
  useEffect(() => {
    if (!outdated || !idle || import.meta.env.DEV) return;
    const key = "yacs.reloadedFor";
    const reload = () => {
      if (document.visibilityState !== "visible" || transfers > 0) return;
      try {
        // Once: if the reload didn't help (a cache in the way), don't loop.
        if (sessionStorage.getItem(key) === relay) return;
        sessionStorage.setItem(key, relay);
      } catch {
        return;
      }
      location.reload();
    };
    reload();
    document.addEventListener("visibilitychange", reload);
    return () => document.removeEventListener("visibilitychange", reload);
  }, [outdated, idle, relay]);
  return outdated;
}

/** Roughly what a clip's items weigh on the wire. */
function itemBytes(items: ClipItem[]): number {
  return items.reduce((sum, item) => {
    if ("Image" in item) return sum + item.Image.data.length;
    if ("File" in item) return sum + item.File.data.length;
    if ("Text" in item) return sum + item.Text.length;
    return sum;
  }, 0);
}

function useObjectUrl(blob: Blob | null) {
  const [url, setUrl] = useState<string | null>(null);
  useEffect(() => {
    if (!blob) return setUrl(null);
    const next = URL.createObjectURL(blob);
    setUrl(next);
    return () => URL.revokeObjectURL(next);
  }, [blob]);
  return url;
}

function errorText(e: unknown): string {
  return e instanceof Error ? e.message : String(e);
}

function GearIcon() {
  return (
    <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}
