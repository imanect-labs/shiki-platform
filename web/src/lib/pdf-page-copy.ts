import {
  PDFArray,
  PDFDict,
  PDFDocument,
  PDFName,
  PDFNull,
  PDFNumber,
  PDFObject,
  PDFPageLeaf,
  PDFRef,
} from "pdf-lib";
import { namedDestinations } from "./pdf-destinations";

/// copyPagesは内部リンク先のページも辿る。削除ページとその注釈への参照をコピー前に除去する。
export async function copyKeptPages(
  source: PDFDocument,
  output: PDFDocument,
  order: number[],
) {
  if (source.catalog.has(PDFName.of("OCProperties")))
    throw new Error(
      "レイヤーの表示設定を持つPDFのページ抽出・削除・結合には未対応です。レイヤーを固定したPDFとして保存してからお試しください。",
    );
  const destination = namedDestinations(source);
  const all = source.getPages();
  const { prunePageResources } = await import("./pdf-resource-prune");
  prunePageResources(source, order);
  let markerIndex = 0;
  while (all.some((p) => p.node.has(PDFName.of(`ShikiPageMap${markerIndex}`))))
    markerIndex++;
  const marker = PDFName.of(`ShikiPageMap${markerIndex}`);
  all.forEach((p, i) => p.node.set(marker, PDFNumber.of(i)));
  const keep = new Set(order);
  const removed = new Set<string>();
  const removedObjects = new Set<PDFObject>();
  for (const [index, page] of all.entries())
    if (!keep.has(index)) {
      removed.add(page.ref.toString());
      removedObjects.add(page.node);
      const annots = page.node.lookupMaybe(PDFName.of("Annots"), PDFArray);
      if (annots)
        for (const value of annots.asArray()) {
          if (value instanceof PDFRef) removed.add(value.toString());
          else removedObjects.add(value);
        }
    }
  const resolve = (value: PDFObject) =>
    value instanceof PDFRef ? source.context.lookup(value) : value;
  const forbidden = (value: PDFObject) =>
    removedObjects.has(value) ||
    (value instanceof PDFRef && removed.has(value.toString()));
  const deletedDestination = (value: PDFObject) => {
    const target = destination(value);
    return (
      target instanceof PDFArray &&
      target.size() > 0 &&
      forbidden(target.get(0))
    );
  };
  const stack: PDFObject[] = order.map((i) => all[i].node);
  const seen = new Set<PDFObject>();
  while (stack.length) {
    const raw = stack.pop()!;
    if (forbidden(raw)) continue;
    const item = resolve(raw);
    if (!item || seen.has(item)) continue;
    seen.add(item);
    if (item instanceof PDFDict) {
      for (const name of item.get(PDFName.of("S")) === PDFName.of("GoTo")
        ? ["Dest", "D"]
        : ["Dest"]) {
        const value = item.get(PDFName.of(name));
        const target = value ? destination(value) : undefined;
        if (target) item.set(PDFName.of(name), target);
      }
      if (item.get(PDFName.of("Subtype")) === PDFName.of("Widget")) {
        // 入力フォームのルートを引き継がないページ抽出では、他ページのwidgetへ遡らない。
        item.delete(PDFName.of("Parent"));
        item.delete(PDFName.of("Kids"));
      }
      const dest = item.get(PDFName.of("Dest"));
      if (dest && deletedDestination(dest)) item.delete(PDFName.of("Dest"));
      for (const [key, value] of item.entries()) {
        if (item instanceof PDFPageLeaf && key === PDFName.of("Parent"))
          continue;
        const resolved = resolve(value);
        const actionTarget =
          resolved instanceof PDFDict &&
          resolved.get(PDFName.of("S")) === PDFName.of("GoTo")
            ? resolved.get(PDFName.of("D"))
            : undefined;
        if (
          forbidden(value) ||
          (actionTarget && deletedDestination(actionTarget))
        )
          item.delete(key);
        else stack.push(value);
      }
    } else if (item instanceof PDFArray) {
      for (let i = 0; i < item.size(); i++) {
        const value = item.get(i);
        if (forbidden(value)) item.set(i, PDFNull);
        else stack.push(value);
      }
    }
  }
  const copied = await output.copyPages(source, order);
  // 残すページへのリンクは、copyPagesが作る隠れた同一ページ参照ではなく出力ページへ向ける。
  const pageRefs = new Map(copied.map((p, i) => [order[i], p.ref]));
  const visited = new Set<PDFObject>();
  const pending: PDFObject[] = copied.map((p) => p.node);
  while (pending.length) {
    const raw = pending.pop()!;
    const item = raw instanceof PDFRef ? output.context.lookup(raw) : raw;
    if (!item || visited.has(item)) continue;
    visited.add(item);
    if (item instanceof PDFDict) {
      for (const name of ["Dest", "D"]) {
        const rawDest = item.get(PDFName.of(name));
        const dest =
          rawDest instanceof PDFRef ? output.context.lookup(rawDest) : rawDest;
        if (dest instanceof PDFArray && dest.size()) {
          const value = dest.get(0);
          const target =
            value instanceof PDFRef ? output.context.lookup(value) : value;
          const original =
            target instanceof PDFDict ? target.get(marker) : undefined;
          const ref =
            original instanceof PDFNumber
              ? pageRefs.get(original.asNumber())
              : undefined;
          if (ref) dest.set(0, ref);
        }
      }
      for (const [key, value] of item.entries())
        if (!(item instanceof PDFPageLeaf && key === PDFName.of("Parent")))
          pending.push(value);
    } else if (item instanceof PDFArray)
      for (const value of item.asArray()) pending.push(value);
  }
  for (const [, object] of output.context.enumerateIndirectObjects())
    if (object instanceof PDFDict) object.delete(marker);
  for (const page of all) page.node.delete(marker);
  return copied;
}
