export const PDF_FILE_LIMIT = 50 * 1024 * 1024;
export const PDF_TOTAL_LIMIT = 100 * 1024 * 1024;
export function checkPdfBudget(existing: number, incoming: number) {
  if (!Number.isFinite(incoming) || incoming < 0 || incoming > PDF_FILE_LIMIT)
    throw new Error("追加するPDFは1ファイル50MBまでです。");
  if (existing + incoming > PDF_TOTAL_LIMIT)
    throw new Error("一度に結合できるPDFは合計100MBまでです。");
}
/// Content-Lengthのない応答でも、予算を超えた時点で転送を中止する。
export async function readPdfResponse(
  response: Response,
  limit: number,
): Promise<Uint8Array> {
  if (!response.ok)
    throw new Error(`PDFの取得に失敗しました（${response.status}）。`);
  const length = Number(response.headers.get("Content-Length"));
  if (length > limit) {
    await response.body?.cancel();
    throw new Error("PDFの容量が上限を超えています（各50MB・合計100MBまで）。");
  }
  if (!response.body) throw new Error("PDFの本文を取得できませんでした。");
  const reader = response.body.getReader();
  const chunks: Uint8Array[] = [];
  let total = 0;
  try {
    while (true) {
      const { value, done } = await reader.read();
      if (done) break;
      total += value.byteLength;
      if (total > limit) {
        await reader.cancel();
        throw new Error(
          "PDFの容量が上限を超えています（各50MB・合計100MBまで）。",
        );
      }
      chunks.push(value);
    }
  } finally {
    reader.releaseLock();
  }
  const bytes = new Uint8Array(total);
  let offset = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, offset);
    offset += chunk.byteLength;
  }
  return bytes;
}
