// YACS service worker. It makes the app installable and open offline, and
// receives Android's "Share → YACS". It never touches /api: clips are
// always fetched live and nothing decrypted is ever cached here.

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
  const image = form.get("image");
  if (image instanceof File && image.size > 0) {
    await cache.put("/shared/image", new Response(image, { headers: { "content-type": image.type } }));
  } else {
    await cache.delete("/shared/image");
  }
  return Response.redirect("/?shared=1", 303);
}
