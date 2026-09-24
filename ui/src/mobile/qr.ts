// Reads QR codes from the camera. Uses the browser's BarcodeDetector where
// there is one (Chrome on Android), and jsQR otherwise (Safari, Firefox),
// loaded only when needed.

export type Decode = (video: HTMLVideoElement) => Promise<string | null>;

// Not in TypeScript's DOM types yet.
interface BarcodeDetectorLike {
  detect(source: CanvasImageSource): Promise<{ rawValue: string }[]>;
}
interface BarcodeDetectorClass {
  new (options: { formats: string[] }): BarcodeDetectorLike;
  getSupportedFormats(): Promise<string[]>;
}

/** Longest side frames are scaled to before jsQR looks at them. */
const JSQR_MAX_SIDE = 800;

export async function qrDecoder(): Promise<Decode> {
  const Detector = (window as unknown as { BarcodeDetector?: BarcodeDetectorClass }).BarcodeDetector;
  if (Detector) {
    try {
      if ((await Detector.getSupportedFormats()).includes("qr_code")) {
        const detector = new Detector({ formats: ["qr_code"] });
        return async (video) => (await detector.detect(video))[0]?.rawValue ?? null;
      }
    } catch {
      // Fall through to jsQR.
    }
  }

  const { default: jsQR } = await import("jsqr");
  const canvas = document.createElement("canvas");
  const ctx = canvas.getContext("2d", { willReadFrequently: true });
  if (!ctx) throw new Error("This browser can't read QR codes.");
  return async (video) => {
    const { videoWidth: w, videoHeight: h } = video;
    if (!w || !h) return null;
    const scale = Math.min(1, JSQR_MAX_SIDE / Math.max(w, h));
    canvas.width = Math.round(w * scale);
    canvas.height = Math.round(h * scale);
    ctx.drawImage(video, 0, 0, canvas.width, canvas.height);
    const { data, width, height } = ctx.getImageData(0, 0, canvas.width, canvas.height);
    return jsQR(data, width, height, { inversionAttempts: "dontInvert" })?.data ?? null;
  };
}

export function canScan(): boolean {
  return !!navigator.mediaDevices?.getUserMedia;
}
