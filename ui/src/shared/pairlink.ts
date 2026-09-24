// A pairing link pasted into the desktop's pair form: `https://relay/#pair=v1.…&token=…`,
// from "Pair another device" on a paired computer.

export interface PastedLink {
  serverUrl: string;
  secret: string;
  token: string | null;
}

export function readPairLink(text: string): PastedLink | null {
  let url: URL;
  try {
    url = new URL(text.trim());
  } catch {
    return null;
  }
  const params = new URLSearchParams(url.hash.replace(/^#/, ""));
  const secret = params.get("pair");
  if (!secret || !/^https?:$/.test(url.protocol)) return null;
  return {
    serverUrl: (url.origin + url.pathname).replace(/\/+$/, ""),
    secret,
    token: params.get("token") || null,
  };
}
