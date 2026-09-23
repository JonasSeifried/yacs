import { type ReactNode, useCallback, useEffect, useState } from "react";
import { platform } from "../platform";
import { formatAccelerator, guessOs, modKey } from "../shared/hotkey";
import { formatDuration, formatSize } from "../shared/time";
import type { ClipMeta, Status } from "../shared/types";

type Clips = { state: "loading" } | { state: "ok"; clips: ClipMeta[] } | { state: "error"; message: string };

export function Spotlight() {
  const [status, setStatus] = useState<Status | null>(null);
  const [clips, setClips] = useState<Clips>({ state: "loading" });
  const [now, setNow] = useState(Date.now());
  const os = status?.os ?? guessOs();

  const refresh = useCallback(async () => {
    setNow(Date.now());
    const s = await platform.status();
    setStatus(s);
    if (!s.paired) return;
    try {
      setClips({ state: "ok", clips: await platform.listClips() });
    } catch (e) {
      setClips({ state: "error", message: String(e) });
    }
  }, []);

  useEffect(() => {
    refresh();
    const subscriptions = [platform.onSpotlightShown(refresh), platform.onStatusChanged(refresh)];
    return () => subscriptions.forEach((s) => s.then((unsubscribe) => unsubscribe()));
  }, [refresh]);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      const mod = os === "macos" ? e.metaKey : e.ctrlKey;
      if (e.key === "Escape") {
        e.preventDefault();
        platform.hideSpotlight();
      } else if (mod && e.key === ",") {
        e.preventDefault();
        platform.openSettings();
      } else if (e.key === "Enter" && status && !status.paired) {
        e.preventDefault();
        platform.openSettings();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [os, status]);

  return (
    <main className="panel">
      <header className="panel-header">
        <span className="brand">YACS</span>
        {status?.paired && status.serverUrl && <span className="server">{new URL(status.serverUrl).host}</span>}
      </header>

      <section className="panel-body">
        {status && !status.paired ? (
          <Empty title="This device isn't paired yet" detail="Pair it with a relay to share clips with your other devices.">
            <button className="primary" onClick={() => platform.openSettings()}>
              Open Settings <kbd>↵</kbd>
            </button>
          </Empty>
        ) : clips.state === "error" ? (
          <Empty title="Can't reach the relay" detail={clips.message} />
        ) : clips.state === "ok" && clips.clips.length === 0 ? (
          <Empty title="No clips yet" detail="Clips sent from your other devices show up here." />
        ) : clips.state === "ok" ? (
          <ul className="clips">
            {clips.clips.map((clip, i) => (
              <li key={clip.id} className={i === 0 ? "clip selected" : "clip"}>
                <span className="clip-title">Encrypted clip</span>
                <span className="clip-meta">
                  {formatDuration(now - clip.created_at_ms)} ago · expires in {formatDuration(clip.expires_at_ms - now)} ·{" "}
                  {formatSize(clip.size)}
                </span>
              </li>
            ))}
          </ul>
        ) : null}
      </section>

      <footer className="panel-footer">
        <span>
          <kbd>esc</kbd> close
        </span>
        <span>
          <kbd>{modKey(os, ",")}</kbd> settings
        </span>
        {status && <span className="hotkey">{formatAccelerator(status.hotkey, os)}</span>}
      </footer>
    </main>
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
