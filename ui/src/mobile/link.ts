// Pairing links: `https://relay/#pair=v1.<channel>.<key>&token=…`, made by the
// desktop's "Pair another device" QR code. The fragment never reaches the relay
// (browsers don't send it), so neither the key nor the token shows up in logs.

export interface PairLink {
  secret: string;
  token: string | null;
}

export function parsePairLink(hash: string): PairLink | null {
  const params = new URLSearchParams(hash.replace(/^#/, ""));
  const secret = params.get("pair");
  if (!secret) return null;
  return { secret, token: params.get("token") || null };
}

/** A scanned QR code: must be a pairing link for this relay, since the app only talks to the relay it's served from. */
export function pairLinkFromCode(text: string, origin = location.origin): PairLink | { error: string } {
  let url: URL;
  try {
    url = new URL(text);
  } catch {
    return { error: "That's not a YACS pairing code." };
  }
  const link = parsePairLink(url.hash);
  if (!link) return { error: "That's not a YACS pairing code." };
  if (url.origin !== origin) return { error: `That code is for ${url.host}. Open YACS there to pair with it.` };
  return link;
}

/** Called right after reading the link, so the secret doesn't linger in the address bar or history. */
export function forgetPairLink() {
  history.replaceState(null, "", location.pathname + location.search);
}

export function guessDeviceName(ua = navigator.userAgent): string {
  if (/iPad/.test(ua) || (/Macintosh/.test(ua) && navigator.maxTouchPoints > 1)) return "iPad";
  if (/iPhone/.test(ua)) return "iPhone";
  if (/Android/.test(ua)) return "Android";
  return "Phone";
}

/**
 * iPhone and iPad in a browser tab. Their home screen apps keep their own
 * storage, so a pairing made here doesn't carry over to one.
 */
export function isIosBrowserTab(ua = navigator.userAgent): boolean {
  const ios = /iPad|iPhone/.test(ua) || (/Macintosh/.test(ua) && navigator.maxTouchPoints > 1);
  const standalone =
    (navigator as { standalone?: boolean }).standalone === true || matchMedia("(display-mode: standalone)").matches;
  return ios && !standalone;
}
