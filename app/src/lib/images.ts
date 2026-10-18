// 聊天图片附件预处理：统一为各 ACP 适配器/模型都接受的格式与体量。
// - 只保留 png / jpeg / webp / gif（DeepSeek 桥与 Claude API 仅接受这四种，bmp/tiff 等转 PNG）
// - 长边超过 MAX_EDGE 缩小（Claude 单图上限 8000px、多图时 2000px；也避免 base64 撑爆 JSON-RPC）
// - 单图编码后超过 MAX_BYTES 改用 JPEG 并逐步降质/缩小（Claude 单图 5MB 上限）
export const MAX_IMAGES = 8;
const MAX_EDGE = 2000;
const MAX_BYTES = 3_500_000;
const KEEP = new Set(["image/png", "image/jpeg", "image/webp", "image/gif"]);

export interface PreparedImage { data: string; mime: string; preview: string }

function readDataUrl(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const rd = new FileReader();
    rd.onload = () => resolve(String(rd.result ?? ""));
    rd.onerror = () => reject(rd.error ?? new Error("read failed"));
    rd.readAsDataURL(blob);
  });
}

function toPrepared(url: string, fallbackMime: string): PreparedImage | null {
  const m = /^data:([^;,]+)?(;base64)?,(.*)$/s.exec(url);
  if (!m || !m[3]) return null;
  return { data: m[3], mime: m[1] || fallbackMime, preview: url };
}

function canvasBlob(c: HTMLCanvasElement, mime: string, q?: number): Promise<Blob | null> {
  return new Promise((resolve) => c.toBlob((b) => resolve(b), mime, q));
}

export async function prepareImage(file: Blob): Promise<PreparedImage> {
  const mime = (file.type || "image/png").toLowerCase().replace("image/jpg", "image/jpeg");
  let bmp: ImageBitmap | null = null;
  try { bmp = await createImageBitmap(file); } catch { bmp = null; }
  if (!bmp) {
    // 解码失败（如 svg/heic）：格式受支持且体量合适时原样发送，否则报错
    if (KEEP.has(mime) && file.size <= MAX_BYTES) {
      const p = toPrepared(await readDataUrl(file), mime);
      if (p) return p;
    }
    throw new Error("无法读取该图片格式（支持 PNG / JPEG / WebP / GIF）");
  }
  const { width, height } = bmp;
  const fits = Math.max(width, height) <= MAX_EDGE && file.size <= MAX_BYTES;
  if (KEEP.has(mime) && fits) {
    bmp.close();
    const p = toPrepared(await readDataUrl(file), mime);
    if (p) return p;
    throw new Error("读取图片失败");
  }
  let scale = Math.min(1, MAX_EDGE / Math.max(width, height));
  const canvas = document.createElement("canvas");
  let blob: Blob | null = null;
  for (let attempt = 0; attempt < 6; attempt++) {
    canvas.width = Math.max(1, Math.round(width * scale));
    canvas.height = Math.max(1, Math.round(height * scale));
    const g = canvas.getContext("2d");
    if (!g) break;
    const asJpeg = attempt > 0 || mime === "image/jpeg";
    if (asJpeg) { g.fillStyle = "#fff"; g.fillRect(0, 0, canvas.width, canvas.height); }
    g.drawImage(bmp, 0, 0, canvas.width, canvas.height);
    blob = await canvasBlob(canvas, asJpeg ? "image/jpeg" : "image/png", asJpeg ? 0.86 : undefined);
    if (blob && blob.size <= MAX_BYTES) break;
    if (attempt > 0) scale *= 0.75;
  }
  bmp.close();
  if (!blob || blob.size > MAX_BYTES) throw new Error("图片过大，压缩后仍超过 3.5MB");
  const p = toPrepared(await readDataUrl(blob), blob.type || "image/png");
  if (!p) throw new Error("读取图片失败");
  return p;
}
