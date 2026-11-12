import {
  PDFArray,
  PDFDict,
  PDFDocument,
  PDFName,
  PDFNumber,
  PDFStream,
} from "pdf-lib";

/// pdf-libのflattenは注釈の表示フラグやRectへの適合変換を適用しない。
/// 保存済み外観をそのまま配置して表示を維持できる入力だけを受け付ける。
export function flattenVisibleForm(source: PDFDocument) {
  const unsupported = () =>
    new Error(
      "このPDFの入力欄の表示には結合が未対応です。入力欄を固定したPDFとして保存してからお試しください。",
    );
  // getForm()自体がXFAを削除するため、呼び出す前に元の辞書を検査する。
  if (
    source.catalog
      .lookupMaybe(PDFName.of("AcroForm"), PDFDict)
      ?.has(PDFName.of("XFA"))
  )
    throw unsupported();
  const form = source.getForm();
  const fields = form.getFields();
  if (fields.length && source.catalog.has(PDFName.of("OCProperties")))
    throw unsupported();
  const numbers = (array: PDFArray | undefined, count: number) => {
    if (!array || array.size() !== count) throw unsupported();
    return array.asArray().map((v) => {
      const n = source.context.lookup(v);
      if (!(n instanceof PDFNumber) || !Number.isFinite(n.asNumber()))
        throw unsupported();
      return n.asNumber();
    });
  };
  for (const field of fields)
    for (const widget of field.acroField.getWidgets()) {
      // 非表示・画面非表示・拡大や回転に追随しない注釈を本文へ露出させない。
      if (
        !(widget.getFlags() & 4) ||
        widget.getFlags() & (1 | 2 | 8 | 16 | 32 | 256)
      )
        throw unsupported();
      if (widget.dict.has(PDFName.of("OC"))) throw unsupported();
      const rect = widget.getRectangle();
      if (
        ![rect.x, rect.y, rect.width, rect.height].every(Number.isFinite) ||
        rect.width <= 0 ||
        rect.height <= 0
      )
        throw unsupported();
      const normal = widget.getAppearances()?.normal;
      const appearances =
        normal instanceof PDFDict
          ? normal.entries().map(([, value]) => source.context.lookup(value))
          : [normal];
      if (!appearances.length) throw unsupported();
      for (const appearance of appearances) {
        if (!(appearance instanceof PDFStream)) throw unsupported();
        if (appearance.dict.has(PDFName.of("OC"))) throw unsupported();
        const box = numbers(
          appearance.dict.lookupMaybe(PDFName.of("BBox"), PDFArray),
          4,
        );
        const rawMatrix = appearance.dict.lookupMaybe(
          PDFName.of("Matrix"),
          PDFArray,
        );
        const [a, b, c, d, e, f] = rawMatrix
          ? numbers(rawMatrix, 6)
          : [1, 0, 0, 1, 0, 0];
        const points = [
          [box[0], box[1]],
          [box[0], box[3]],
          [box[2], box[1]],
          [box[2], box[3]],
        ].map(([x, y]) => [a * x + c * y + e, b * x + d * y + f]);
        const actual = [
          Math.min(...points.map((p) => p[0])),
          Math.min(...points.map((p) => p[1])),
          Math.max(...points.map((p) => p[0])),
          Math.max(...points.map((p) => p[1])),
        ];
        const expected = [0, 0, rect.width, rect.height];
        if (actual.some((v, i) => Math.abs(v - expected[i]) > 0.001))
          throw unsupported();
      }
    }
  form.flatten({ updateFieldAppearances: false });
}
