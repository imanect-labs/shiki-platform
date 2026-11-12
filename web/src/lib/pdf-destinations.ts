import {
  PDFArray,
  PDFDict,
  PDFDocument,
  PDFHexString,
  PDFName,
  PDFObject,
  PDFRef,
  PDFString,
} from "pdf-lib";

/// 出力へカタログを移さないページコピーでは、入力固有の名前をページ参照へ解決する。
export function namedDestinations(source: PDFDocument) {
  const resolve = (value: PDFObject | undefined) =>
    value instanceof PDFRef ? source.context.lookup(value) : value;
  const text = (value: PDFObject | undefined) => {
    const v = resolve(value);
    return v instanceof PDFName ||
      v instanceof PDFString ||
      v instanceof PDFHexString
      ? v.decodeText()
      : undefined;
  };
  const destinations = new Map<string, PDFObject>();
  const tooMany = () =>
    new Error("リンク先の定義が多すぎるため、PDFを保存できません。");
  const add = (key: string, value: PDFObject) => {
    if (!destinations.has(key) && destinations.size >= 10000) throw tooMany();
    destinations.set(key, value);
  };
  const legacy = resolve(source.catalog.get(PDFName.of("Dests")));
  if (legacy instanceof PDFDict)
    for (const [name, value] of legacy.entries()) add(name.decodeText(), value);
  const names = resolve(source.catalog.get(PDFName.of("Names")));
  const pending =
    names instanceof PDFDict ? [names.get(PDFName.of("Dests"))] : [];
  const seen = new Set<PDFObject>();
  while (pending.length) {
    const node = resolve(pending.pop());
    if (!node || seen.has(node)) continue;
    seen.add(node);
    if (seen.size > 10000) throw tooMany();
    if (!(node instanceof PDFDict)) continue;
    const pairs = resolve(node.get(PDFName.of("Names")));
    if (pairs instanceof PDFArray)
      for (let i = 0; i + 1 < pairs.size(); i += 2) {
        const key = text(pairs.get(i));
        if (key !== undefined) add(key, pairs.get(i + 1));
      }
    const kids = resolve(node.get(PDFName.of("Kids")));
    if (kids instanceof PDFArray)
      for (const kid of kids.asArray()) {
        if (pending.length >= 10000) throw tooMany();
        pending.push(kid);
      }
  }
  return (value: PDFObject) => {
    const key = text(value);
    let dest = resolve(key === undefined ? value : destinations.get(key));
    if (dest instanceof PDFDict) dest = resolve(dest.get(PDFName.of("D")));
    return dest instanceof PDFArray ? dest : undefined;
  };
}
