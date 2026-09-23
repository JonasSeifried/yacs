// Pairing links: `https://relay/#pair=v1.<channel>.<key>&token=…`, made by the
// desktop's "Pair a phone" QR code. The fragment never reaches the relay
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
