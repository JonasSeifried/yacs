import { type ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { platform } from "../platform";
import { EAGER_BYTES, EAGER_CONCURRENCY, runLimited } from "../shared/async";
import { clipTitle, isImageMime, previewDocument, previewKind } from "../shared/clip";
import { guessOs, modKey } from "../shared/hotkey";
import { formatDuration, formatSize, ttlChoices } from "../shared/time";
import type { ClipMeta, ClipView, FileInfo, ServerConfig, Status } from "../shared/types";

type List = { state: "loading" } | { state: "ok"; clips: ClipMeta[] } | { state: "error"; message: string };
type Loaded = { state: "loading" } | { state: "ok"; clip: ClipView } | { state: "gone" } | { state: "error"; message: string };
type Notice = { kind: "ok" | "error"; text: string };

/** How long "Sent" stays up before Spotlight gets out of the way. */
const SENT_HIDE_MS = 5000;

export function Spotlight() {
  const [status, setStatus] = useState<Status | null>(null);
  const [config, setConfig] = useState<ServerConfig | null>(null);
  const [list, setList] = useState<List>({ state: "loading" });
  const [loaded, setLoaded] = useState<Record<string, Loaded>>({});
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [ttl, setTtl] = useState(15 * 60);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [busy, setBusy] = useState(false);
  /** Just sent: another ⌘V would send the same clipboard again, so it closes instead. */
  const [sent, setSent] = useState(false);
  const hideTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const [now, setNow] = useState(Date.now());
  const requested = useRef(new Set<string>());
  const os = status?.os ?? guessOs();

  const hideAfter = useCallback((ms: number) => {
    clearTimeout(hideTimer.current);
    hideTimer.current = setTimeout(() => platform.hideSpotlight(), ms);
  }, []);
  const cancelHide = useCallback(() => {
    clearTimeout(hideTimer.current);
    setSent(false);
  }, []);

  const clips = list.state === "ok" ? list.clips : [];
  const selected = clips.find((c) => c.id === selectedId) ?? clips[0] ?? null;

  const load = useCallback(async (id: string) => {
    if (requested.current.has(id)) return;
    requested.current.add(id);
    setLoaded((l) => ({ ...l, [id]: { state: "loading" } }));
    let result: Loaded;
    try {
      const clip = await platform.getClip(id);
      result = clip ? { state: "ok", clip } : { state: "gone" };
    } catch (e) {
      requested.current.delete(id); // retry on the next refresh
      result = { state: "error", message: String(e) };
    }
    setLoaded((l) => ({ ...l, [id]: result }));
  }, []);

  const reloadList = useCallback(async () => {
    setNow(Date.now());
    try {
      const listed = await platform.listClips();
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
      setList({ state: "error", message: String(e) });
    }
  }, [load]);

  const refresh = useCallback(async () => {
    const s = await platform.status();
    setStatus(s);
    setTtl(s.defaultTtlSecs);
    if (!s.paired) return;
    platform.serverConfig().then(setConfig, () => {});
    await reloadList();
  }, [reloadList]);

  useEffect(() => {
    refresh();
    const onShown = () => {
      cancelHide();
      setSelectedId(null); // back to the newest
      setNotice(null);
      setBusy(false);
      refresh();
    };
    const subscriptions = [
      platform.onSpotlightShown(onShown),
      platform.onStatusChanged(refresh),
      // Live updates from the relay: keeps the chosen expiry and selection.
      platform.onClipsChanged(reloadList),
    ];
    return () => subscriptions.forEach((s) => s.then((unsubscribe) => unsubscribe()));
  }, [refresh, reloadList, cancelHide]);

  useEffect(() => {
    if (selected) load(selected.id);
  }, [selected, load]);

  const choices = ttlChoices(ttl, config?.max_ttl_secs);
  const changeTtl = useCallback((secs: number) => {
    setTtl(secs);
    platform.setDefaultTtl(secs).catch((e) => setNotice({ kind: "error", text: String(e) }));
  }, []);

  const copy = useCallback(async () => {
    if (!selected || busy) return;
    setBusy(true);
    try {
      await platform.copyClip(selected.id);
      platform.hideSpotlight(); // paste right away in the app that was in front
    } catch (e) {
      setNotice({ kind: "error", text: String(e) });
    } finally {
      setBusy(false);
    }
  }, [selected, busy]);

  const send = useCallback(async () => {
    if (busy) return;
    setBusy(true);
    setNotice({ kind: "ok", text: "Encrypting and sending…" });
    try {
      const clip = await platform.sendClipboard(ttl);
      const id = clip.meta.id;
      requested.current.add(id);
      setLoaded((l) => ({ ...l, [id]: { state: "ok", clip } }));
      setList((l) => ({ state: "ok", clips: [clip.meta, ...(l.state === "ok" ? l.clips : [])] }));
      setSelectedId(id);
      setNotice({ kind: "ok", text: `Sent · expires in ${formatDuration(ttl * 1000)}` });
      setSent(true);
      hideAfter(SENT_HIDE_MS);
    } catch (e) {
      setNotice({ kind: "error", text: String(e) });
      setBusy(false);
    }
  }, [busy, ttl, hideAfter]);

  const remove = useCallback(async () => {
    if (!selected || busy) return;
    const index = clips.indexOf(selected);
    try {
      await platform.deleteClip(selected.id);
      const rest = clips.filter((c) => c.id !== selected.id);
      setList({ state: "ok", clips: rest });
      setSelectedId(rest[Math.min(index, rest.length - 1)]?.id ?? null);
      setNotice(null);
    } catch (e) {
      setNotice({ kind: "error", text: String(e) });
    }
  }, [selected, clips, busy]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = os === "macos" ? e.metaKey : e.ctrlKey;
      const key = e.key.toLowerCase();
      const handled = () => e.preventDefault();

      if (e.key === "Escape" || (sent && mod && key === "v")) {
        handled();
        platform.hideSpotlight();
        return;
      }
      // Doing anything else keeps Spotlight open.
      if (!["Shift", "Meta", "Control", "Alt"].includes(e.key)) cancelHide();

      if (mod && key === ",") {
        handled();
        platform.openSettings();
      } else if (!status?.paired) {
        if (e.key === "Enter" && status) {
          handled();
          platform.openSettings();
        }
      } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        handled();
        if (!selected) return;
        const next = clips.indexOf(selected) + (e.key === "ArrowDown" ? 1 : -1);
        if (clips[next]) setSelectedId(clips[next].id);
      } else if (e.key === "Enter" || (mod && key === "c")) {
        handled();
        copy();
      } else if (mod && key === "v") {
        handled();
        send();
      } else if (e.key === "Delete" || (os === "macos" && e.metaKey && e.key === "Backspace")) {
        // Deletes for every device, so a stray Backspace doesn't: macOS uses ⌘⌫ like Finder.
        handled();
        remove();
      } else if (e.key === "Tab") {
        handled();
        const i = choices.findIndex((o) => o.secs === ttl);
        const next = (i + (e.shiftKey ? -1 : 1) + choices.length) % choices.length;
        changeTtl(choices[next].secs);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [os, status, clips, selected, choices, ttl, sent, copy, send, remove, changeTtl, cancelHide]);

  // A click into the HTML preview moves focus into its iframe, where our keys
  // don't arrive. Take it straight back.
  useEffect(() => {
    const onBlur = () =>
      setTimeout(() => {
        if (document.activeElement instanceof HTMLIFrameElement) {
          document.activeElement.blur();
          window.focus();
        }
      });
    window.addEventListener("blur", onBlur);
    return () => window.removeEventListener("blur", onBlur);
  }, []);

  const paired = status?.paired ?? false;
  const selectedLoaded = selected ? loaded[selected.id] : undefined;
  const selectedHasFiles = selectedLoaded?.state === "ok" && selectedLoaded.clip.files.length > 0;
  return (
    <main className="panel">
      <header className="panel-header">
        <span className="brand">YACS</span>
        {paired && status?.serverUrl && <span className="server">{new URL(status.serverUrl).host}</span>}
        <button className="icon-button" title={`Settings (${modKey(os, ",")})`} onClick={() => platform.openSettings()}>
          <GearIcon />
        </button>
      </header>

      <section className="panel-body">
        {status && !paired ? (
          <Empty title="This device isn't paired yet" detail="Pair it with a relay to share clips with your other devices.">
            <button className="primary" onClick={() => platform.openSettings()}>
              Open Settings <kbd>↵</kbd>
            </button>
          </Empty>
        ) : list.state === "error" ? (
          <Empty title="Can't reach the relay" detail={list.message} />
        ) : list.state === "ok" && clips.length === 0 ? (
          <Empty
            title="No clips yet"
            detail={`Press ${modKey(os, "V")} to send what's on your clipboard, or copy something on another device.`}
          />
        ) : list.state === "ok" && selected ? (
          <div className="split">
            <ul className="clips">
              {clips.map((clip) => (
                <ClipRow
                  key={clip.id}
                  meta={clip}
                  loaded={loaded[clip.id]}
                  now={now}
                  selected={clip.id === selected.id}
                  onSelect={() => setSelectedId(clip.id)}
                  onCopy={copy}
                />
              ))}
            </ul>
            <Preview meta={selected} loaded={loaded[selected.id]} now={now} />
          </div>
        ) : null}
      </section>

      {notice && <div className={`notice ${notice.kind}`}>{notice.text}</div>}

      <footer className="panel-footer">
        {paired && (
          <>
            {selected && (
              <>
                <Hint keys="↵" label={selectedHasFiles ? "save & copy" : "copy"} />
                <Hint keys={os === "macos" ? "⌘⌫" : "Del"} label="delete" />
              </>
            )}
            <Hint keys={modKey(os, "V")} label="send clipboard" />
            <label className="ttl" title="Tab / Shift+Tab to change">
              expires in
              <select value={ttl} onChange={(e) => changeTtl(Number(e.target.value))} tabIndex={-1}>
                {choices.map((o) => (
                  <option key={o.secs} value={o.secs}>
                    {o.label}
                  </option>
                ))}
              </select>
              <kbd>⇥</kbd>
            </label>
          </>
        )}
        {!paired && <Hint keys="esc" label="close" />}
      </footer>
    </main>
  );
}

function ClipRow(props: {
  meta: ClipMeta;
  loaded: Loaded | undefined;
  now: number;
  selected: boolean;
  onSelect: () => void;
  onCopy: () => void;
}) {
  const { meta, loaded, now, selected } = props;
  const ref = useRef<HTMLLIElement>(null);
  useEffect(() => {
    if (selected) ref.current?.scrollIntoView({ block: "nearest" });
  }, [selected]);

  const ago = `${formatDuration(now - meta.created_at_ms)} ago`;
  let title: ReactNode;
  let detail = `${ago} · ${formatSize(meta.size)}`;
  if (loaded?.state === "ok") {
    title = clipTitle(loaded.clip);
    detail = `${loaded.clip.deviceName} · ${ago}`;
  } else if (loaded?.state === "error") {
    title = <span className="muted">Can't decrypt this clip</span>;
  } else if (loaded?.state === "gone") {
    title = <span className="muted">Expired</span>;
  } else {
    title = <span className="muted">Encrypted clip</span>;
  }
  return (
    <li ref={ref} className={selected ? "clip selected" : "clip"} onMouseDown={props.onSelect} onDoubleClick={props.onCopy}>
      <span className="clip-title">{title}</span>
      <span className="clip-meta">{detail}</span>
    </li>
  );
}

function Preview({ meta, loaded, now }: { meta: ClipMeta; loaded: Loaded | undefined; now: number }) {
  const expires = `expires in ${formatDuration(meta.expires_at_ms - now)}`;
  if (!loaded || loaded.state === "loading") {
    return <PreviewShell footer={expires}><p className="preview-note">Decrypting…</p></PreviewShell>;
  }
  if (loaded.state === "gone") {
    return <PreviewShell footer="">
      <p className="preview-note">This clip expired or was deleted.</p>
    </PreviewShell>;
  }
  if (loaded.state === "error") {
    return <PreviewShell footer={expires}><p className="preview-note error">{loaded.message}</p></PreviewShell>;
  }

  const { clip } = loaded;
  const files = clip.files.length && (clip.files.length === 1 ? "file" : `${clip.files.length} files`);
  const formats = [files, clip.text !== null && "text", (clip.html !== null || clip.rtf) && "formatted", clip.image && "image"]
    .filter(Boolean)
    .join(", ");
  const footer = `${expires} · ${formatSize(meta.size)} · ${formats}`;
  const kind = previewKind(clip);
  return (
    <PreviewShell footer={footer}>
      {kind === "html" && clip.html !== null && (
        <iframe className="preview-html" sandbox="" srcDoc={previewDocument(clip.html)} title="Preview" tabIndex={-1} />
      )}
      {kind === "text" && (
        <pre className="preview-text">
          {clip.text}
          {clip.textTruncated && <span className="muted">{"\n"}… (preview cut short; copy gets everything)</span>}
        </pre>
      )}
      {kind === "image" && <ImagePreview id={meta.id} />}
      {kind === "files" && <FilesPreview id={meta.id} files={clip.files} />}
      {kind === "none" && <p className="preview-note">Rich text without a preview. Copy it to paste with formatting.</p>}
    </PreviewShell>
  );
}

function PreviewShell({ footer, children }: { footer: string; children: ReactNode }) {
  return (
    <div className="preview">
      <div className="preview-content">{children}</div>
      {footer && <div className="preview-footer">{footer}</div>}
    </div>
  );
}

function ImagePreview({ id }: { id: string }) {
  const [url, setUrl] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let objectUrl: string | null = null;
    let cancelled = false;
    setUrl(null);
    setError(null);
    platform.clipImage(id).then(
      (blob) => {
        if (cancelled) return;
        objectUrl = URL.createObjectURL(blob);
        setUrl(objectUrl);
      },
      (e) => !cancelled && setError(String(e)),
    );
    return () => {
      cancelled = true;
      if (objectUrl) URL.revokeObjectURL(objectUrl);
    };
  }, [id]);
  if (error) return <p className="preview-note error">{error}</p>;
  return url ? <img className="preview-image" src={url} alt="" /> : <p className="preview-note">Loading image…</p>;
}

/** An image file is shown; the rest are listed. */
function FilesPreview({ id, files }: { id: string; files: FileInfo[] }) {
  const image = files.length === 1 && isImageMime(files[0].mime);
  return (
    <div className="preview-files">
      {image && <ImagePreview id={id} />}
      <ul>
        {files.map((file, i) => (
          <li key={i}>
            <span className="file-name">{file.name}</span>
            <span className="muted">{formatSize(file.size)}</span>
          </li>
        ))}
      </ul>
      <p className="muted small">↵ saves {files.length === 1 ? "it" : "them"} to Downloads and puts {files.length === 1 ? "it" : "them"} on the clipboard.</p>
    </div>
  );
}

function Hint({ keys, label }: { keys: string; label: string }) {
  return (
    <span className="hint">
      <kbd>{keys}</kbd> {label}
    </span>
  );
}

function Empty({ title, detail, children }: { title: string; detail: string; children?: ReactNode }) {
  return (
    <div className="empty">
      <p className="empty-title">{title}</p>
      <p className="empty-detail">{detail}</p>
      {children}
    </div>
  );
}

function GearIcon() {
  return (
    <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true">
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 1 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06A1.65 1.65 0 0 0 4.68 15a1.65 1.65 0 0 0-1.51-1H3a2 2 0 1 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06A1.65 1.65 0 0 0 9 4.68a1.65 1.65 0 0 0 1-1.51V3a2 2 0 1 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06A1.65 1.65 0 0 0 19.4 9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 1 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
  );
}
