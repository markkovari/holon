/**
 * A photograph, downscaled in the browser before it is uploaded.
 *
 * Not an optimisation: a modern phone camera produces 3–8 MB, the app holds the whole
 * body in memory before it forwards it, and base64 makes it a third larger again.
 * 1600px on the long edge is more than a vision model reads and roughly a tenth the
 * bytes.
 *
 * The quality is deliberately high (0.9) — JPEG artefacts on a card's set number are
 * exactly the detail the model has to read, and the prompt tells it the picture was
 * compressed so it does not report the compression as damage to the card.
 */
export async function downscale(file: File, maxEdge = 1600): Promise<{ media_type: string; data: string }> {
  const bitmap = await createImageBitmap(file);
  const scale = Math.min(1, maxEdge / Math.max(bitmap.width, bitmap.height));
  const w = Math.round(bitmap.width * scale);
  const h = Math.round(bitmap.height * scale);

  const canvas = document.createElement("canvas");
  canvas.width = w;
  canvas.height = h;
  const ctx = canvas.getContext("2d")!;
  ctx.imageSmoothingEnabled = true;
  ctx.imageSmoothingQuality = "high";
  ctx.drawImage(bitmap, 0, 0, w, h);
  bitmap.close?.();

  const url = canvas.toDataURL("image/jpeg", 0.9);
  const m = /^data:(.+?);base64,(.*)$/.exec(url);
  if (!m) throw new Error("could not read the photo");
  return { media_type: m[1], data: m[2] };
}
