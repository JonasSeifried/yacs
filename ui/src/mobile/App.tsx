import { type FormEvent, type ReactNode, useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  type Decrypted,
  LiveUnsupported,
  type StoredPairing,
  WebClient,
  generatePhrase,
  pair,
  saveDeviceName,
  storedPairing,
  unpair,
} from "../platform/web";
import { EAGER_BYTES, EAGER_CONCURRENCY, runLimited } from "../shared/async";
import { clipTitle, isImageMime, previewDocument, previewKind } from "../shared/clip";
import { formatDuration, formatSize, ttlChoices } from "../shared/time";
import type { ClipItem, ClipMeta, ServerConfig } from "../shared/types";
import { canCopy, canSave, copyClip, fileItem, pick, readClipboard, savable, shareFiles } from "./clipboard";
import { type PairLink, forgetPairLink, guessDeviceName, isIosBrowserTab, pairLinkFromCode, parsePairLink } from "./link";
import { canScan, qrDecoder } from "./qr";

const TTL_KEY = "yacs.ttl";
/** While the app is open without a live connection, look for new clips this often. */
const POLL_MS = 10_000;
const LIVE_RETRY_MIN_MS = 1_000;
const LIVE_RETRY_MAX_MS = 60_000;

// Read once at startup, then removed from the address bar.
const initialLink = parsePairLink(location.hash);
if (initialLink) forgetPairLink();

export function App() {
  const [stored, setStored] = useState<StoredPairing | null>(storedPairing);
  const [link, setLink] = useState<PairLink | null>(initialLink);

  // A pairing link opened while the app is already open only changes the hash.
  useEffect(() => {
    const onHash = () => {
      const next = parsePairLink(location.hash);
      if (!next) return;
      forgetPairLink();
      setLink(next);
    };
    window.addEventListener("hashchange", onHash);
    return () => window.removeEventListener("hashchange", onHash);
  }, []);

  if (link) {
    return (
      <PairFromLink
        link={link}
        replacing={stored !== null}
        onDone={(next) => {
          setLink(null);
          if (next) setStored(next);
        }}
      />
    );
  }
  if (!stored) return <PairForm onPaired={setStored} onLink={setLink} />;
  return <Home key={stored.secret} stored={stored} onChange={setStored} />;
}

// ── Pairing ────────────────────────────────────────────────────────────────

function PairFromLink(props: { link: PairLink; replacing: boolean; onDone: (s: StoredPairing | null) => void }) {
  const [deviceName, setDeviceName] = useState(guessDeviceName);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      props.onDone(await pair({ secret: props.link.secret }, props.link.token, deviceName));
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  };

  return (
    <Screen>
      <form className="card pair" onSubmit={submit}>
        <h1>Pair this device</h1>
        <p className="muted">
          Clips will sync through <b>{location.host}</b>, end-to-end encrypted.
          {props.replacing && " This replaces the pairing this device has now."}
        </p>
        <IosHomeScreenHint />
        <label>
          <span>Device name <span className="muted">shown to your other devices</span></span>
          <input value={deviceName} onChange={(e) => setDeviceName(e.target.value)} required />
        </label>
        {error && <p className="error">{error}</p>}
        <button className="primary" disabled={busy}>
          {busy ? "Pairing…" : "Pair"}
        </button>
        <button type="button" className="ghost" onClick={() => props.onDone(null)} disabled={busy}>
          Cancel
        </button>
      </form>
    </Screen>
  );
}

function PairForm({ onPaired, onLink }: { onPaired: (s: StoredPairing) => void; onLink: (l: PairLink) => void }) {
  const [scanning, setScanning] = useState(false);
  const [phrase, setPhrase] = useState("");
  const [token, setToken] = useState("");
  const [deviceName, setDeviceName] = useState(guessDeviceName);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      onPaired(await pair({ phrase }, token, deviceName));
    } catch (e) {
      setError(errorText(e));
      setBusy(false);
    }
  };

  if (scanning) return <Scanner onLink={onLink} onCancel={() => setScanning(false)} />;

  return (
    <Screen>
      <form className="card pair" onSubmit={submit}>
        <h1>Pair with your devices</h1>
        <p className="muted">
          Scan the QR code in YACS on your computer (Settings → Pair another device), or type the pairing phrase.
        </p>
        <IosHomeScreenHint />
        {canScan() && (
          <button type="button" className="primary" onClick={() => setScanning(true)}>
            Scan QR code
          </button>
        )}
        <label>
          Pairing phrase
          <textarea
            value={phrase}
            onChange={(e) => setPhrase(e.target.value)}
            rows={2}
            required
            autoCapitalize="none"
            autoCorrect="off"
            autoComplete="off"
            spellCheck={false}
            placeholder="six words from your other device"
          />
        </label>
        <button type="button" className="link" onClick={async () => setPhrase(await generatePhrase())}>
          Start a new pairing instead
        </button>
        <label>
          <span>Access token <span className="muted">if the relay needs one</span></span>
          <input value={token} onChange={(e) => setToken(e.target.value)} autoCapitalize="none" autoComplete="off" />
        </label>
        <label>
          <span>Device name <span className="muted">shown to your other devices</span></span>
          <input value={deviceName} onChange={(e) => setDeviceName(e.target.value)} required />
        </label>
        {error && <p className="error">{error}</p>}
        <button className="primary" disabled={busy}>
          {busy ? "Deriving key…" : "Pair"}
        </button>
      </form>
    </Screen>
  );
}

function Scanner({ onLink, onCancel }: { onLink: (l: PairLink) => void; onCancel: () => void }) {
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
            const link = pairLinkFromCode(code);
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
        <p className="muted">On your computer: YACS → Settings → Pair another device.</p>
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
  if (name === "NotAllowedError") return "YACS isn't allowed to use the camera. Allow it in your browser's settings, or type the phrase instead.";
  if (name === "NotFoundError" || name === "OverconstrainedError") return "No camera found. Type the phrase instead.";
  return errorText(e);
}

/** Pairing in an iPhone browser tab doesn't carry over to the home screen app. */
function IosHomeScreenHint() {
  if (!isIosBrowserTab()) return null;
  return (
    <p className="muted small">
      Want YACS on your home screen? Add it there first (Share → Add to Home Screen), then pair from inside it: the
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

type List = { state: "loading" } | { state: "ok"; clips: ClipMeta[] } | { state: "error"; message: string };
type Loaded = { state: "loading" } | { state: "ok"; clip: Decrypted } | { state: "gone" } | { state: "error"; message: string };
type Toast = { kind: "ok" | "error"; text: string };

function Home({ stored, onChange }: { stored: StoredPairing; onChange: (s: StoredPairing | null) => void }) {
  const client = useMemo(() => new WebClient(stored), [stored]);
  const [config, setConfig] = useState<ServerConfig | null>(null);
  const [list, setList] = useState<List>({ state: "loading" });
  const [loaded, setLoaded] = useState<Record<string, Loaded>>({});
  const [openId, setOpenId] = useState<string | null>(null);
  const [ttl, setTtl] = useState(() => Number(localStorage.getItem(TTL_KEY)) || 15 * 60);
  const [toast, setToast] = useState<Toast | null>(null);
  const [settings, setSettings] = useState(false);
  const [now, setNow] = useState(Date.now());
  const requested = useRef(new Set<string>());
  const toastTimer = useRef<ReturnType<typeof setTimeout>>(undefined);

  const notify = useCallback((kind: Toast["kind"], text: string) => {
    clearTimeout(toastTimer.current);
    setToast({ kind, text });
    toastTimer.current = setTimeout(() => setToast(null), kind === "ok" ? 2500 : 6000);
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
    try {
      const listed = await client.list();
      setList({ state: "ok", clips: listed });
      const ids = new Set(listed.map((c) => c.id));
      for (const id of requested.current) if (!ids.has(id)) requested.current.delete(id);
      setLoaded((l) => Object.fromEntries(Object.entries(l).filter(([id]) => ids.has(id))));
      if (listed[0]) load(listed[0].id);
      runLimited(
        listed.filter((c) => c.size <= EAGER_BYTES).map((c) => () => load(c.id)),
        EAGER_CONCURRENCY,
      );
    } catch (e) {
      setList({ state: "error", message: errorText(e) });
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

  const clips = list.state === "ok" ? list.clips : [];
  const open = clips.find((c) => c.id === openId) ?? clips[0] ?? null;
  useEffect(() => {
    if (open) load(open.id);
  }, [open, load]);

  const changeTtl = (secs: number) => {
    setTtl(secs);
    localStorage.setItem(TTL_KEY, String(secs));
  };

  const send = async (items: ClipItem[]) => {
    const sent = await client.send(items, ttl);
    const id = sent.view.meta.id;
    requested.current.add(id);
    setLoaded((l) => ({ ...l, [id]: { state: "ok", clip: sent } }));
    setList((l) => ({ state: "ok", clips: [sent.view.meta, ...(l.state === "ok" ? l.clips : [])] }));
    setOpenId(id);
    notify("ok", `Sent · expires in ${formatDuration(ttl * 1000)}`);
  };

  const remove = async (id: string) => {
    try {
      await client.delete(id);
      setList((l) => (l.state === "ok" ? { state: "ok", clips: l.clips.filter((c) => c.id !== id) } : l));
    } catch (e) {
      notify("error", errorText(e));
    }
  };

  return (
    <Screen>
      <header className="top">
        <span className="brand">YACS</span>
        <span className="server">{location.host}</span>
        <button className="icon" aria-label="Settings" onClick={() => setSettings(true)}>
          <GearIcon />
        </button>
      </header>

      <Composer
        ttl={ttl}
        ttlChoices={ttlChoices(ttl, config?.max_ttl_secs)}
        maxBytes={config?.max_size_bytes ?? null}
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
          />
        ))}
      </section>

      {toast && <div className={`toast ${toast.kind}`}>{toast.text}</div>}
      {settings && <SettingsSheet stored={stored} onClose={() => setSettings(false)} onChange={onChange} />}
    </Screen>
  );
}

function Composer(props: {
  ttl: number;
  ttlChoices: { secs: number; label: string }[];
  /** The relay's limit per clip, once known. */
  maxBytes: number | null;
  onTtl: (secs: number) => void;
  onSend: (items: ClipItem[]) => Promise<void>;
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

  const run = async (items: () => Promise<ClipItem[]>) => {
    setBusy(true);
    try {
      const got = await items();
      if (got.length === 0) throw new Error("Nothing to send: the clipboard is empty.");
      await props.onSend(got);
      setText("");
      setFiles([]);
    } catch (e) {
      props.onError(errorText(e));
    } finally {
      setBusy(false);
    }
  };

  const total = files.reduce((sum, f) => sum + f.size, 0);
  const tooLarge = props.maxBytes !== null && total > props.maxBytes;
  const drafted = text.trim() !== "" || files.length > 0;
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
          {tooLarge && (
            <p className="error small">
              {formatSize(total)} is more than this relay takes per clip ({formatSize(props.maxBytes!)}).
            </p>
          )}
          <button
            className="primary"
            disabled={busy || tooLarge}
            onClick={() =>
              run(async () => [
                ...(text.trim() ? [{ Text: text }] : []),
                ...(await Promise.all(files.map(fileItem))),
              ])
            }
          >
            {busy ? "Sending…" : "Send"}
          </button>
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
}) {
  const { meta, loaded, now, open } = props;
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
          </div>
          <div className="clip-actions">
            {(!decrypted || canCopy(decrypted.clip)) && (
              <button className="primary" onClick={copy} disabled={!decrypted}>
                Copy
              </button>
            )}
            {decrypted && canSave(decrypted.clip) && (
              <button
                className={canCopy(decrypted.clip) ? "ghost" : "primary"}
                onClick={() => shareFiles(savable(decrypted.clip, `yacs-${meta.id}`)).catch((e) => props.onError(errorText(e)))}
              >
                Save / Share
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

function SettingsSheet(props: { stored: StoredPairing; onClose: () => void; onChange: (s: StoredPairing | null) => void }) {
  const [deviceName, setDeviceName] = useState(props.stored.deviceName);
  return (
    <div className="sheet-backdrop" onClick={props.onClose}>
      <div className="sheet" onClick={(e) => e.stopPropagation()}>
        <h2>Settings</h2>
        <label>
          Device name
          <input value={deviceName} onChange={(e) => setDeviceName(e.target.value)} />
        </label>
        <button
          className="primary"
          onClick={() => {
            props.onChange(saveDeviceName(props.stored, deviceName));
            props.onClose();
          }}
        >
          Save
        </button>
        <p className="muted small">
          Install YACS: in Safari tap Share → Add to Home Screen; in Chrome use “Install app”. On Android, installed
          YACS shows up in the share sheet.
        </p>
        <button
          className="ghost danger"
          onClick={() => {
            if (!confirm("Unpair this device? You'll need the phrase or QR code to pair it again.")) return;
            unpair();
            props.onChange(null);
          }}
        >
          Unpair this device
        </button>
      </div>
    </div>
  );
}

// ── Bits ────────────────────────────────────────────────────────────────

function Screen({ children }: { children: ReactNode }) {
  return <main className="screen">{children}</main>;
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
