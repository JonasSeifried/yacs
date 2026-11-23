import { type MouseEvent, type ReactNode, useCallback, useEffect, useRef, useState } from "react";
import { platform } from "../platform";
import { EAGER_CONCURRENCY, loadsEagerly, runLimited } from "../shared/async";
import { type ClipIcon, clipIcon, clipTitle, isImageMime, previewDocument, previewKind } from "../shared/clip";
import { guessOs, modKey } from "../shared/hotkey";
import { maxTtlSecs } from "../shared/plan";
import { formatDuration, formatSize, ttlChoices } from "../shared/time";
import type { ClipMeta, ClipView, FileInfo, ServerConfig, SpaceLimits, Status, Transfer } from "../shared/types";
import { type Copied, loadCopied, markCopied, pickAfterClick, pruneCopied, saveCopied, stepPick } from "../shared/picks";
import { onFocusLeft } from "./focus";

/** A failed refresh keeps the clips it had, with `error` set, instead of hiding them. */
type List =
  | { state: "loading" }
  | { state: "ok"; clips: ClipMeta[]; error?: string }
  | { state: "error"; message: string };
type Loaded = { state: "loading" } | { state: "ok"; clip: ClipView } | { state: "gone" } | { state: "error"; message: string };
/** `undo`: the clips just deleted, which ⌘Z brings back. */
type Notice = { kind: "ok" | "info" | "error"; text: string; undo?: string[] };

/** One picked clip is just the selected one: none are left picked. */
const several = (ids: string[]) => (ids.length > 1 ? ids : []);

/** How long "Sent" stays up before Spotlight gets out of the way. */
const SENT_HIDE_MS = 5000;
/** A delete waits this long for ⌘Z before it goes to the relay (for every device). */
const UNDO_MS = 5000;
/** Sends bigger than this show a progress bar; smaller ones are over in a blink. */
const SENDING_BAR_BYTES = 256 * 1024;

export function Spotlight() {
  const [status, setStatus] = useState<Status | null>(null);
  const [config, setConfig] = useState<ServerConfig | null>(null);
  const [limits, setLimits] = useState<SpaceLimits | null>(null);
  const [list, setList] = useState<List>({ state: "loading" });
  const [loaded, setLoaded] = useState<Record<string, Loaded>>({});
  const [selectedId, setSelectedId] = useState<string | null>(null);
  /** ⌘/Ctrl- or Shift-clicked clips, in click order; two or more copy and delete together. */
  const [picked, setPicked] = useState<string[]>([]);
  /** When this computer copied which clips, to mark them. */
  const [copied, setCopied] = useState<Copied>(loadCopied);
  /** A clip whose big files are downloading: copied once they're done. */
  const pendingCopy = useRef<string | null>(null);
  /** Where a Shift-click range starts, and the picks it adds to (as in Finder and Explorer). */
  const rangeFrom = useRef<{ id: string; picks: string[] } | null>(null);
  const [ttl, setTtl] = useState(15 * 60);
  const [notice, setNotice] = useState<Notice | null>(null);
  const [busy, setBusy] = useState(false);
  /** A big upload or download running in the background. */
  const [transfer, setTransfer] = useState<Transfer | null>(null);
  /** How far sending the clipboard got, once it's big enough to show. */
  const [sending, setSending] = useState<{ done: number; total: number } | null>(null);
  /** Just sent: another ⌘V would send the same clipboard again, so it closes instead. */
  const [sent, setSent] = useState(false);
  const hideTimer = useRef<ReturnType<typeof setTimeout>>(undefined);
  const [now, setNow] = useState(Date.now());
  const requested = useRef(new Set<string>());
  /** Deleted here but not yet on the relay, with the timer that sends it. */
  const [deleting, setDeleting] = useState<string[]>([]);
  const pendingDeletes = useRef(new Map<string, ReturnType<typeof setTimeout>>());
  const os = status?.os ?? guessOs();

  const hideAfter = useCallback((ms: number) => {
    clearTimeout(hideTimer.current);
    hideTimer.current = setTimeout(() => platform.hideSpotlight(), ms);
  }, []);
  const cancelHide = useCallback(() => {
    clearTimeout(hideTimer.current);
    setSent(false);
  }, []);

  const clips = list.state === "ok" ? list.clips.filter((c) => !deleting.includes(c.id)) : [];
  const selected = clips.find((c) => c.id === selectedId) ?? clips[0] ?? null;
  const pickedClips = picked.flatMap((id) => clips.filter((c) => c.id === id));
  /** Picks still in the list: expired or deleted ones don't come back. */
  const pickedIds = pickedClips.map((c) => c.id);
  const many = pickedClips.length > 1;
  // Picks cleared (Esc, delete, expiry): a Shift-click starts afresh.
  useEffect(() => {
    if (!many) rangeFrom.current = null;
  }, [many]);

  const noteCopied = useCallback((ids: string[]) => {
    setCopied((c) => {
      const next = markCopied(c, ids, Date.now());
      saveCopied(next);
      return next;
    });
  }, []);

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
      setCopied((c) => {
        const next = pruneCopied(c, listed.map((clip) => clip.id));
        saveCopied(next);
        return next;
      });
      const ids = new Set(listed.map((c) => c.id));
      for (const id of requested.current) if (!ids.has(id)) requested.current.delete(id);
      setLoaded((l) => Object.fromEntries(Object.entries(l).filter(([id]) => ids.has(id))));
      if (listed[0]) load(listed[0].id);
      runLimited(
        listed.filter(loadsEagerly).map((c) => () => load(c.id)),
        EAGER_CONCURRENCY,
      );
    } catch (e) {
      const message = String(e);
      setList((l) => (l.state === "ok" ? { ...l, error: message } : { state: "error", message }));
    }
  }, [load]);

  const refresh = useCallback(async () => {
    const s = await platform.status();
    setStatus(s);
    platform.transferStatus().then(setTransfer, () => {});
    setTtl(s.defaultTtlSecs);
    if (!s.space) return;
    platform.serverConfig().then(setConfig, () => {});
    platform.spaceLimits().then(setLimits, () => {});
    await reloadList();
  }, [reloadList]);

  useEffect(() => {
    refresh();
    const onShown = () => {
      cancelHide();
      setSelectedId(null); // back to the newest
      setPicked([]);
      rangeFrom.current = null;
      setNotice(null);
      setBusy(false);
      refresh();
    };
    const subscriptions = [
      platform.onSpotlightShown(onShown),
      platform.onStatusChanged(refresh),
      // Live updates from the relay: keeps the chosen expiry and selection.
      platform.onClipsChanged(reloadList),
      platform.onTransferChanged(({ transfer, finished }) => {
        setTransfer(transfer);
        if (!finished) return;
        setNotice({ kind: finished.ok ? "ok" : finished.cancelled ? "info" : "error", text: finished.message });
        if (finished.ok && finished.direction === "upload") reloadList();
        if (finished.direction === "download") {
          if (finished.ok && pendingCopy.current) noteCopied([pendingCopy.current]);
          pendingCopy.current = null;
        }
        // The files are on the clipboard now: paste them where you were.
        if (finished.ok && finished.direction === "download") platform.hideSpotlight();
      }),
    ];
    return () => subscriptions.forEach((s) => s.then((unsubscribe) => unsubscribe()));
  }, [refresh, reloadList, cancelHide, noteCopied]);

  useEffect(() => {
    if (selected) load(selected.id);
  }, [selected, load]);

  const choices = ttlChoices(ttl, maxTtlSecs(config, limits));
  const changeTtl = useCallback((secs: number) => {
    setTtl(secs);
    platform.setDefaultTtl(secs).catch((e) => setNotice({ kind: "error", text: String(e) }));
  }, []);

  const copy = useCallback(async () => {
    if (!selected || busy) return;
    setBusy(true);
    try {
      if (many) {
        const ids = pickedClips.map((c) => c.id);
        await platform.copyClips(ids);
        noteCopied(ids);
        platform.hideSpotlight();
      } else if (await platform.copyClip(selected.id)) {
        noteCopied([selected.id]);
        platform.hideSpotlight(); // paste right away in the app that was in front
      } else {
        // Big files download first; the transfer bar shows how far.
        pendingCopy.current = selected.id;
      }
    } catch (e) {
      setNotice({ kind: "error", text: String(e) });
    } finally {
      setBusy(false);
    }
  }, [selected, busy, many, pickedClips, noteCopied]);

  const send = useCallback(async () => {
    if (busy) return;
    setBusy(true);
    setNotice({ kind: "info", text: "Encrypting and sending…" });
    try {
      const { clip, upload } = await platform.sendClipboard(ttl, (done, total) => {
        if (total > SENDING_BAR_BYTES) setSending({ done, total });
      });
      setSending(null);
      if (!clip) {
        // Big files: they upload in the background and show up when done.
        setTransfer(upload);
        setNotice(null);
        setBusy(false);
        return;
      }
      const id = clip.meta.id;
      requested.current.add(id);
      setLoaded((l) => ({ ...l, [id]: { state: "ok", clip } }));
      setList((l) => ({ state: "ok", clips: [clip.meta, ...(l.state === "ok" ? l.clips : [])] }));
      setSelectedId(id);
      setNotice({ kind: "ok", text: `Sent · expires in ${formatDuration(ttl * 1000)}` });
      setSent(true);
      setBusy(false); // `sent` keeps a second ⌘V from sending again
      hideAfter(SENT_HIDE_MS);
    } catch (e) {
      setSending(null);
      setNotice({ kind: "error", text: String(e) });
      setBusy(false);
    }
  }, [busy, ttl, hideAfter]);

  const commitDelete = useCallback(async (id: string) => {
    clearTimeout(pendingDeletes.current.get(id));
    pendingDeletes.current.delete(id);
    // Too late for ⌘Z now, so don't offer it.
    setNotice((n) => (n?.undo?.includes(id) ? null : n));
    try {
      await platform.deleteClip(id);
      setList((l) => (l.state === "ok" ? { ...l, clips: l.clips.filter((c) => c.id !== id) } : l));
    } catch (e) {
      setNotice({ kind: "error", text: String(e) });
    } finally {
      setDeleting((d) => d.filter((x) => x !== id)); // back in the list if it failed
    }
  }, []);

  /** Hidden at once; deleted for every device unless ⌘Z comes first. */
  const remove = useCallback(() => {
    if (!selected || busy) return;
    const ids = many ? pickedClips.map((c) => c.id) : [selected.id];
    setDeleting((d) => [...d, ...ids]);
    for (const id of ids) pendingDeletes.current.set(id, setTimeout(() => commitDelete(id), UNDO_MS));
    setPicked([]);
    // The selected clip stays if it wasn't deleted; else the next one that's left.
    if (ids.includes(selected.id)) {
      const index = clips.indexOf(selected);
      const after = clips.slice(index).find((c) => !ids.includes(c.id));
      const before = clips.slice(0, index).reverse().find((c) => !ids.includes(c.id));
      setSelectedId((after ?? before)?.id ?? null);
    }
    setNotice({ kind: "info", text: ids.length > 1 ? `Deleted ${ids.length} clips` : "Deleted", undo: ids });
  }, [selected, clips, busy, many, pickedClips, commitDelete]);

  const undoDelete = useCallback((ids: string[]) => {
    for (const id of ids) {
      clearTimeout(pendingDeletes.current.get(id));
      pendingDeletes.current.delete(id);
    }
    setDeleting((d) => d.filter((x) => !ids.includes(x)));
    setSelectedId(ids[0]);
    setPicked(ids.length > 1 ? ids : []);
    setNotice(null);
  }, []);

  /** ⌘-click (Ctrl-click off a Mac) picks one more or one less, Shift-click a range; a plain click just selects. */
  const click = (id: string, e: MouseEvent) => {
    const how = (os === "macos" ? e.metaKey : e.ctrlKey) ? "toggle" : e.shiftKey ? "range" : "only";
    if (how !== "only") {
      e.preventDefault(); // Shift-click would select text
      // ⌘C and ↵ then copy the picks, not text left highlighted in the preview.
      window.getSelection()?.removeAllRanges();
    }
    const order = clips.map((c) => c.id);
    if (how === "range") {
      const kept = rangeFrom.current && order.includes(rangeFrom.current.id) ? rangeFrom.current : null;
      const from = kept ?? { id: selected?.id ?? id, picks: many ? pickedIds : [] };
      rangeFrom.current = from;
      setPicked(several(pickAfterClick(from.picks, order, from.id, id, "range")));
      setSelectedId(id);
      return;
    }
    const next = how === "only" ? [] : several(pickAfterClick(pickedIds, order, selected?.id ?? null, id, how));
    // Unpicking down to one clip: that one is what ↵ copies now, not the one just dropped.
    const lone = how === "toggle" && next.length === 0 ? pickedIds.find((x) => x !== id) : undefined;
    rangeFrom.current = { id: lone ?? id, picks: lone ? [] : next.filter((x) => x !== id) };
    setPicked(next);
    setSelectedId(lone ?? id);
  };

  // Undo only lasts while Spotlight is open: hiding sends pending deletes now.
  // A click into the HTML preview doesn't count (see `onFocusLeft`).
  useEffect(() => {
    const flush = () => [...pendingDeletes.current.keys()].forEach(commitDelete);
    const onHidden = () => document.visibilityState === "hidden" && flush();
    const stop = onFocusLeft(flush);
    document.addEventListener("visibilitychange", onHidden);
    return () => {
      stop();
      document.removeEventListener("visibilitychange", onHidden);
    };
  }, [commitDelete]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = os === "macos" ? e.metaKey : e.ctrlKey;
      const key = e.key.toLowerCase();
      const handled = () => e.preventDefault();

      if (e.key === "Escape" && many) {
        handled();
        setPicked([]);
        return;
      }
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
      } else if (!status?.space) {
        if (e.key === "Enter" && status) {
          handled();
          platform.openSettings();
        }
      } else if (e.key === "ArrowDown" || e.key === "ArrowUp") {
        handled();
        if (!selected) return;
        const next = clips[clips.indexOf(selected) + (e.key === "ArrowDown" ? 1 : -1)];
        if (!next) return;
        // Shift+arrow picks as it goes, and unpicks going back.
        if (e.shiftKey) {
          setPicked(several(stepPick(pickedIds, selected.id, next.id)));
        } else {
          setPicked([]);
          rangeFrom.current = null;
        }
        setSelectedId(next.id);
      } else if (mod && key === "a" && !window.getSelection()?.toString()) {
        // With text highlighted in the preview, ⌘A selects text as usual.
        handled();
        setPicked(several(clips.map((c) => c.id)));
        // A Shift-click after picking all picks just that range.
        rangeFrom.current = selected ? { id: selected.id, picks: [] } : null;
      } else if (mod && key === "c" && !many && window.getSelection()?.toString()) {
        // Text selected in the preview: let ⌘C copy just that.
      } else if (e.key === "Enter" || (mod && key === "c")) {
        handled();
        copy();
      } else if (mod && key === "z" && notice?.undo) {
        handled();
        undoDelete(notice.undo);
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
  }, [os, status, clips, selected, many, pickedIds, choices, ttl, sent, notice, copy, send, remove, undoDelete, changeTtl, cancelHide]);

  const space = status?.space ?? null;
  const selectedLoaded = selected ? loaded[selected.id] : undefined;
  const selectedHasFiles = selectedLoaded?.state === "ok" && selectedLoaded.clip.files.length > 0;
  return (
    <main className="panel">
      <header className="panel-header">
        <span className="brand">
          <BrandIcon />
          YACS
        </span>
        {space && (
          <span className="tip tip-start server-tip" data-tip={`Through ${new URL(space.relay).host}`}>
            <span className="server">{space.name}</span>
          </span>
        )}
        {(status?.update || status?.updateInstalling) && (
          <button className="update-pill tip" data-tip="Install it in Settings" onClick={() => platform.openSettings()}>
            {status.updateInstalling ? `Installing ${status.updateInstalling}…` : `Update ${status.update} available`}
          </button>
        )}
        <button
          className="icon-button tip"
          data-tip={`Settings (${modKey(os, ",")})`}
          aria-label="Settings"
          onClick={() => platform.openSettings()}
        >
          <GearIcon />
        </button>
      </header>

      <section className="panel-body">
        {status && !space ? (
          <Empty title="Set up YACS" detail="Start here, or connect this computer to YACS on your other devices.">
            <button className="primary" onClick={() => platform.openSettings()}>
              Open Settings <kbd>↵</kbd>
            </button>
          </Empty>
        ) : list.state === "loading" && space ? (
          <Empty title="Loading…" detail="" />
        ) : list.state === "error" ? (
          <Empty title="Couldn't load your clips" detail={list.message} />
        ) : list.state === "ok" && clips.length === 0 ? (
          <Empty
            title="No clips yet"
            detail={`Press ${modKey(os, "V")} to send what's on your clipboard, or copy something on another device.`}
          />
        ) : list.state === "ok" && selected ? (
          <div className="split">
            <ul
              className="clips"
              role="listbox"
              aria-label="Clips"
              aria-multiselectable="true"
              aria-activedescendant={`clip-${selected.id}`}
            >
              {clips.map((clip) => (
                <ClipRow
                  key={clip.id}
                  meta={clip}
                  loaded={loaded[clip.id]}
                  now={now}
                  selected={clip.id === selected.id}
                  picked={many && picked.includes(clip.id)}
                  copiedAt={copied[clip.id]}
                  onSelect={(e) => click(clip.id, e)}
                  onCopy={copy}
                />
              ))}
            </ul>
            <Preview meta={selected} loaded={loaded[selected.id]} now={now} copiedAt={copied[selected.id]} />
          </div>
        ) : null}
      </section>

      {transfer && <TransferBar transfer={transfer} onCancel={() => platform.cancelTransfer()} />}
      {sending ? (
        <TransferBar transfer={{ direction: "upload", label: "clipboard", ...sending }} />
      ) : notice ? (
        <div className={`notice ${notice.kind}`} role="status">
          {notice.text}
          {notice.undo && (
            <button className="link" onClick={() => undoDelete(notice.undo!)} tabIndex={-1}>
              Undo <kbd>{modKey(os, "Z")}</kbd>
            </button>
          )}
        </div>
      ) : (
        list.state === "ok" &&
        list.error && (
          <div className="notice error" role="status">
            Couldn't refresh: {list.error}
          </div>
        )
      )}

      <footer className="panel-footer">
        {space && (
          <>
            {selected && many ? (
              <>
                <Hint keys="↵" label={`copy ${pickedClips.length}`} />
                <Hint keys={os === "macos" ? "⌘⌫" : "Del"} label={`delete ${pickedClips.length}`} />
                <Hint keys="esc" label="unpick" />
              </>
            ) : (
              selected && (
                <>
                  <Hint keys="↵" label={selectedHasFiles ? "save & copy" : "copy"} />
                  <Hint keys={os === "macos" ? "⌘⌫" : "Del"} label="delete" />
                </>
              )
            )}
            <Hint keys={modKey(os, "V")} label="send clipboard" />
            <label className="ttl tip tip-above" data-tip="Tab / Shift+Tab to change">
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
        {!space && <Hint keys="esc" label="close" />}
      </footer>
    </main>
  );
}

function ClipRow(props: {
  meta: ClipMeta;
  loaded: Loaded | undefined;
  now: number;
  selected: boolean;
  /** One of several picked clips. */
  picked: boolean;
  /** When this computer copied it, if it did. */
  copiedAt: number | undefined;
  onSelect: (e: MouseEvent) => void;
  onCopy: () => void;
}) {
  const { meta, loaded, now, selected, picked, copiedAt } = props;
  const ref = useRef<HTMLLIElement>(null);
  useEffect(() => {
    if (selected) ref.current?.scrollIntoView({ block: "nearest" });
  }, [selected]);

  const ago = `${formatDuration(now - meta.created_at_ms)} ago`;
  let title: ReactNode;
  let detail = `${ago} · ${formatSize(meta.size)}`;
  let icon: ClipIcon | "locked" = "locked";
  if (loaded?.state === "ok") {
    title = clipTitle(loaded.clip);
    detail = `${loaded.clip.deviceName} · ${ago}`;
    icon = clipIcon(loaded.clip);
  } else if (loaded?.state === "error") {
    title = <span className="muted">Can't decrypt this clip</span>;
  } else if (loaded?.state === "gone") {
    title = <span className="muted">Expired</span>;
  } else {
    // Too big to decrypt for the list: it opens when selected.
    title = `Large clip · ${formatSize(meta.size)}`;
    detail = `${ago} · ${loaded?.state === "loading" ? "decrypting…" : "select to open"}`;
  }
  return (
    <li
      ref={ref}
      id={`clip-${meta.id}`}
      role="option"
      aria-selected={selected || picked}
      className={["clip", selected && "selected", picked && "picked"].filter(Boolean).join(" ")}
      onMouseDown={props.onSelect}
      // Double-clicking copies; a quick ⌘/Ctrl- or Shift-click twice is picking.
      onDoubleClick={(e) => !(e.metaKey || e.ctrlKey || e.shiftKey) && props.onCopy()}
    >
      <span className="clip-icon">
        <KindIcon kind={icon} />
      </span>
      <span className="clip-text">
        <span className="clip-title">{title}</span>
        <span className="clip-meta">{detail}</span>
      </span>
      {copiedAt !== undefined && <span className="copied-tag">Copied</span>}
    </li>
  );
}

function Preview(props: { meta: ClipMeta; loaded: Loaded | undefined; now: number; copiedAt: number | undefined }) {
  const { meta, loaded, now, copiedAt } = props;
  const copiedAgo = copiedAt === undefined ? "" : ` · copied ${formatDuration(now - copiedAt)} ago`;
  const expires = `expires in ${formatDuration(meta.expires_at_ms - now)}${copiedAgo}`;
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
  const formats = [files, clip.text !== null && "text", (clip.hasHtml || clip.rtf) && "formatted", clip.image && "image"]
    .filter(Boolean)
    .join(", ");
  const size = clip.image?.width && clip.image.height ? ` · ${clip.image.width}×${clip.image.height}` : "";
  const footer = `${expires} · ${formatSize(meta.size)} · ${formats}${size}`;
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
      {kind === "files" && <FilesPreview id={meta.id} files={clip.files} chunked={meta.chunked === true} />}
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

/** An image file is shown; the rest are listed. Chunked files aren't fetched until ↵. */
function FilesPreview({ id, files, chunked }: { id: string; files: FileInfo[]; chunked: boolean }) {
  const image = !chunked && files.length === 1 && isImageMime(files[0].mime);
  const them = files.length === 1 ? "it" : "them";
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
      <p className="muted small">
        {chunked ? `↵ downloads ${them} to Downloads, then puts ${them} on the clipboard.`
        : `↵ saves ${them} to Downloads and puts ${them} on the clipboard.`}
      </p>
    </div>
  );
}

function TransferBar({ transfer, onCancel }: { transfer: Transfer; onCancel?: () => void }) {
  const { direction, label, done, total } = transfer;
  const percent = total > 0 ? Math.floor((done / total) * 100) : 100;
  const verb = direction === "upload" ? "Sending" : "Downloading";
  return (
    <div className="transfer">
      <div className="transfer-text">
        <span className="transfer-label">
          {verb} {label}
        </span>
        <span className="muted">
          {percent}% · {formatSize(done)} of {formatSize(total)}
        </span>
        {onCancel && (
          <button className="link" onClick={onCancel} tabIndex={-1}>
            Cancel
          </button>
        )}
      </div>
      <div className="transfer-track">
        <div className="transfer-fill" style={{ width: `${percent}%` }} />
      </div>
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

/** Feather-style outlines, drawn at 16 px in the history list. */
const KIND_PATHS: Record<ClipIcon | "locked", ReactNode> = {
  text: <path d="M4 6h16M4 12h16M4 18h10" />,
  formatted: (
    <>
      <path d="M4 20 9.5 5h1L16 20M6.2 14.5h7.6" />
      <path d="M17 11h4M17 15.5h4M17 20h4" />
    </>
  ),
  link: (
    <>
      <path d="M10 13a5 5 0 0 0 7.54.54l3-3a5 5 0 0 0-7.07-7.07l-1.72 1.71" />
      <path d="M14 11a5 5 0 0 0-7.54-.54l-3 3a5 5 0 0 0 7.07 7.07l1.71-1.71" />
    </>
  ),
  image: (
    <>
      <rect x="3" y="3" width="18" height="18" rx="2" />
      <circle cx="8.5" cy="8.5" r="1.5" />
      <path d="m21 15-5-5L5 21" />
    </>
  ),
  file: (
    <>
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <path d="M14 2v6h6" />
    </>
  ),
  locked: (
    <>
      <rect x="4" y="11" width="16" height="10" rx="2" />
      <path d="M8 11V7a4 4 0 0 1 8 0v4" />
    </>
  ),
};

function KindIcon({ kind }: { kind: ClipIcon | "locked" }) {
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {KIND_PATHS[kind]}
    </svg>
  );
}

/** The app icon: a card stacked on another, on the coral tile (apps/desktop/icon/generate.py). */
function BrandIcon() {
  return (
    <svg className="brand-icon" width="18" height="18" viewBox="0 0 100 100" aria-hidden="true">
      <rect width="100" height="100" rx="24" fill="#e2566f" />
      <mask id="brand-gap">
        <rect width="100" height="100" fill="#fff" />
        <rect x="42.8" y="42.8" width="34.3" height="34.3" rx="9" fill="#000" />
      </mask>
      <rect x="26.5" y="26.5" width="32.5" height="32.5" rx="7.2" fill="#fff" mask="url(#brand-gap)" />
      <rect x="46.4" y="46.4" width="27.1" height="27.1" rx="6.3" fill="#fff" />
    </svg>
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
