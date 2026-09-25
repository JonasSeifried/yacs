// YACS service worker. It makes the app installable and open offline,
// receives Android's "Share → YACS", and streams big downloads to the
// browser. It never touches /api: clips are always fetched live and nothing
// decrypted is ever cached here.

const SHELL = "yacs-shell-v1";
const SHARED = "yacs-shared";

self.addEventListener("install", () => self.skipWaiting());

self.addEventListener("activate", (event) => {
  event.waitUntil(
    (async () => {
      for (const key of await caches.keys()) if (key.startsWith("yacs-shell-") && key !== SHELL) await caches.delete(key);
      await self.clients.claim();
    })(),
  );
});

self.addEventListener("fetch", (event) => {
  const url = new URL(event.request.url);
  if (url.origin !== location.origin || url.pathname.startsWith("/api/")) return;

  if (event.request.method === "POST" && url.pathname === "/share") {
    event.respondWith(receiveShare(event.request));
    return;
  }
  if (url.pathname.startsWith("/download/")) {
    event.respondWith(streamDownload(url.pathname.slice("/download/".length)));
    return;
  }
  if (event.request.method !== "GET") return;

  if (url.pathname.startsWith("/assets/")) {
    // Content-hashed: never changes, cache forever.
    event.respondWith(cacheFirst(event.request));
  } else {
    // The page, manifest and icons: fresh when online, cached when not.
    event.respondWith(networkFirst(event.request));
  }
});

async function cacheFirst(request) {
  const cached = await caches.match(request);
  if (cached) return cached;
  const response = await fetch(request);
  if (response.ok) (await caches.open(SHELL)).put(request, response.clone());
  return response;
}

async function networkFirst(request) {
  try {
    const response = await fetch(request);
    if (response.ok) (await caches.open(SHELL)).put(request, response.clone());
    return response;
  } catch (e) {
    const cached = await caches.match(request, { ignoreSearch: true });
    if (cached) return cached;
    throw e;
  }
}

// The shared content waits in a cache until the page picks it up
// (`?shared=1`), then the page deletes it. It's plaintext, so it's only held
// for that hand-over.
async function receiveShare(request) {
  const form = await request.formData();
  const text = [form.get("title"), form.get("text"), form.get("url")].filter((v) => typeof v === "string" && v).join("\n");
  const cache = await caches.open(SHARED);
  await cache.put("/shared/text", new Response(text));
  for (const request of await cache.keys()) {
    if (new URL(request.url).pathname.startsWith("/shared/file/")) await cache.delete(request);
  }
  // "image" is what older installs still send, until the browser updates the manifest.
  const files = [...form.getAll("files"), ...form.getAll("image")].filter((f) => f instanceof File && f.size > 0);
  for (const [i, file] of files.entries()) {
    const headers = { "content-type": file.type, "x-yacs-name": encodeURIComponent(file.name) };
    await cache.put(`/shared/file/${i}`, new Response(file, { headers }));
  }
  return Response.redirect("/?shared=1", 303);
}

// Big downloads. The page's download worker decrypts a file and hands it
// over through a MessagePort, one piece per pull, so it never sits in memory
// here as a whole. The page registers the download with this message, then
// loads `/download/<token>` in a hidden iframe, which the browser saves as a
// file. The key never comes here.
const downloads = new Map();

self.addEventListener("message", (event) => {
  const message = event.data;
  if (message?.type !== "download") return; // "keepalive" just wakes this worker
  const [port, ack] = event.ports;
  downloads.set(message.token, { name: message.name, size: message.size, port });
  ack.postMessage("ready");
});

function streamDownload(token) {
  const download = downloads.get(token);
  if (!download) return new Response("This download has expired.", { status: 404 });
  downloads.delete(token);
  const { port } = download;
  let waiting = null;
  const body = new ReadableStream(
    {
      start(controller) {
        port.onmessage = ({ data }) => {
          if (data.type === "data") controller.enqueue(data.bytes);
          else if (data.type === "end") controller.close();
          else if (data.type === "error") controller.error(new Error(data.message));
          waiting?.();
          waiting = null;
        };
      },
      pull() {
        return new Promise((resolve) => {
          waiting = resolve;
          port.postMessage({ type: "pull" });
        });
      },
      cancel() {
        port.postMessage({ type: "cancel" });
      },
    },
    { highWaterMark: 1 },
  );
  const ascii = download.name.replace(/[^\x20-\x7e]|["\\]/g, "_");
  return new Response(body, {
    headers: {
      // Not the sender's type: this response comes from our origin, and must
      // never render (as HTML, say) instead of downloading.
      "content-type": "application/octet-stream",
      "content-length": String(download.size),
      "content-disposition": `attachment; filename="${ascii}"; filename*=UTF-8''${encodeURIComponent(download.name)}`,
      "x-content-type-options": "nosniff",
    },
  });
}
