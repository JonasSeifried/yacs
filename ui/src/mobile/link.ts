// Invite links: `https://relay/#pair=v1.<channel>.<key>&token=…&name=…`, made
// by the desktop's "Invite a device" QR code and `yacs invite`. The fragment
// never reaches the relay (browsers don't send it), so neither the key nor the
// token shows up in logs.

export interface InviteLink {
  secret: string;
  token: string | null;
  /** The inviter's name for the space, as a suggestion. */
  name: string | null;
}

export function parseInviteLink(hash: string): InviteLink | null {
  const params = new URLSearchParams(hash.replace(/^#/, ""));
  const secret = params.get("pair");
  if (!secret) return null;
  return { secret, token: params.get("token") || null, name: params.get("name")?.trim() || null };
}

/**
 * A scanned QR code or a pasted link: must be an invite for this relay, since
 * the app only talks to the relay it's served from.
 */
export function inviteLinkFromCode(text: string, origin = location.origin): InviteLink | { error: string } {
  let url: URL;
  try {
    url = new URL(text.trim());
  } catch {
    return { error: "That's not a YACS invite." };
  }
  const link = parseInviteLink(url.hash);
  if (!link) return { error: "That's not a YACS invite." };
  if (url.origin !== origin) return { error: `That invite is for ${url.host}. Open YACS there to join with it.` };
  return link;
}

/** The link other devices join with, for the relay at `base` (this app's own URL). */
export function inviteUrl(link: InviteLink, base = location.origin + location.pathname): string {
  const params = new URLSearchParams();
  if (link.token) params.set("token", link.token);
  if (link.name) params.set("name", link.name);
  const rest = params.toString();
  // The secret is base64url and dots: nothing to escape.
  return `${base.replace(/\/+$/, "")}/#pair=${link.secret}${rest ? `&${rest}` : ""}`;
}

/** Called right after reading the link, so the secret doesn't linger in the address bar or history. */
export function forgetInviteLink() {
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
 * storage, so a space joined here doesn't carry over to one.
 */
export function isIosBrowserTab(ua = navigator.userAgent): boolean {
  const ios = isIos(ua);
  const standalone =
    (navigator as { standalone?: boolean }).standalone === true || matchMedia("(display-mode: standalone)").matches;
  return ios && !standalone;
}

/** iPhone and iPad, including iPads that claim to be a Mac. */
export function isIos(ua = navigator.userAgent): boolean {
  return /iPad|iPhone/.test(ua) || (/Macintosh/.test(ua) && navigator.maxTouchPoints > 1);
}
