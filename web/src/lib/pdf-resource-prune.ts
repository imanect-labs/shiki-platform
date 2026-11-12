import {
  decodePDFRawStream,
  PDFArray,
  PDFDict,
  PDFDocument,
  PDFName,
  PDFObject,
  PDFRawStream,
  PDFRef,
} from "pdf-lib";
import ParserModule from "pdf-lib/cjs/core/parser/PDFObjectParser.js";
import StreamModule from "pdf-lib/cjs/core/parser/ByteStream.js";
const PDFObjectParser =
  (ParserModule as unknown as { default?: typeof ParserModule }).default ??
  ParserModule;
const ByteStream =
  (StreamModule as unknown as { default?: typeof StreamModule }).default ??
  StreamModule;

const unsupported = () =>
  new Error(
    "このPDFにはページ削除に未対応の描画形式があります。別のPDFとして保存してからお試しください。",
  );
const whitespace = (c: number) => [0, 9, 10, 12, 13, 32].includes(c);
const delimiter = (c: number) =>
  whitespace(c) || [40, 41, 60, 62, 91, 93, 123, 125, 47, 37].includes(c);
/// オペランドはpdf-lib自身の構文解析器へ渡す。文字列・コメント中の名前をリソース参照と誤認しない。
class ContentParser extends PDFObjectParser {
  private depth = 0;
  private objects = 0;
  parseObject(): PDFObject {
    if (++this.objects > 100000 || ++this.depth > 100) throw unsupported();
    try {
      return super.parseObject();
    } finally {
      this.depth--;
    }
  }
  walk(visit: (operator: string, operands: PDFObject[]) => void) {
    let operands: PDFObject[] = [];
    while (!this.bytes.done()) {
      this.skipWhitespaceAndComments();
      if (this.bytes.done()) break;
      const c = this.bytes.peek();
      if ([40, 60, 91, 47, 43, 45, 46].includes(c) || (c >= 48 && c <= 57)) {
        operands.push(this.parseObject());
        continue;
      }
      let operator = "";
      while (!this.bytes.done() && !delimiter(this.bytes.peek()))
        operator += String.fromCharCode(this.bytes.next());
      if (!operator) throw unsupported();
      if (operator === "BI" || operator === "ID" || operator === "EI")
        throw unsupported();
      visit(operator, operands);
      operands = [];
    }
  }
}
/// 残すページの演算子が使うリソースのみを共有辞書へ残す。入れ子のForm/Pattern/SMask/注釈も追跡する。
export function prunePageResources(source: PDFDocument, indices: number[]) {
  const resolve = (o: PDFObject | undefined) =>
    o instanceof PDFRef ? source.context.lookup(o) : o;
  const dict = (o: PDFObject | undefined) => {
    const d = resolve(o);
    return d instanceof PDFDict ? d : undefined;
  };
  const used = new Map<PDFDict, Map<string, Set<PDFName>>>();
  const work: { streams: PDFRawStream[]; resources: PDFDict }[] = [];
  const visited = new Map<PDFObject, Set<PDFDict>>();
  const decoded = new Map<PDFRawStream, Uint8Array>();
  let budget = 50 * 1024 * 1024;
  const queue = (value: PDFObject | undefined, resources: PDFDict) => {
    const item = resolve(value);
    if (!(item instanceof PDFRawStream) && !(item instanceof PDFArray)) return;
    if (
      item instanceof PDFRawStream &&
      item.dict.get(PDFName.of("Subtype")) === PDFName.of("Image")
    )
      return;
    const scope =
      item instanceof PDFRawStream
        ? (dict(item.dict.get(PDFName.of("Resources"))) ?? resources)
        : resources;
    let seen = visited.get(item);
    if (!seen) {
      seen = new Set();
      visited.set(item, seen);
    }
    if (seen.has(scope)) return;
    seen.add(scope);
    const streams: PDFRawStream[] = [];
    if (item instanceof PDFRawStream) streams.push(item);
    else
      for (const value of item.asArray()) {
        const stream = resolve(value);
        if (!(stream instanceof PDFRawStream)) throw unsupported();
        streams.push(stream);
      }
    work.push({ streams, resources: scope });
  };
  const mark = (
    resources: PDFDict,
    category: string,
    name: PDFObject | undefined,
  ) => {
    // パーサのCJS版とブラウザのESM版はクラスが別になるため、公開名で正規化する。
    if (
      !name ||
      !name.toString().startsWith("/") ||
      !("decodeText" in name) ||
      typeof name.decodeText !== "function"
    )
      return;
    name = PDFName.of(name.decodeText());
    let categories = used.get(resources);
    if (!categories) {
      categories = new Map();
      used.set(resources, categories);
    }
    let names = categories.get(category);
    if (!names) {
      names = new Set();
      categories.set(category, names);
    }
    if (names.has(name as PDFName)) return;
    names.add(name as PDFName);
    const value = dict(resources.get(PDFName.of(category)))?.get(
      name as PDFName,
    );
    const object = resolve(value);
    if (category === "XObject" || category === "Pattern")
      queue(value, resources);
    const color = (raw: PDFObject | undefined, depth = 0): void => {
      if (depth > 50) throw unsupported();
      const entry = resolve(raw);
      if (entry instanceof PDFName) mark(resources, "ColorSpace", entry);
      else if (entry instanceof PDFArray && entry.size()) {
        const type = entry.get(0).toString();
        if (
          (type === "/Indexed" || type === "/I" || type === "/Pattern") &&
          entry.size() > 1
        )
          color(entry.get(1), depth + 1);
        else if (
          (type === "/Separation" || type === "/DeviceN") &&
          entry.size() > 2
        )
          color(entry.get(2), depth + 1);
      }
    };
    if (category === "ColorSpace") color(value);
    if (object instanceof PDFRawStream) {
      color(object.dict.get(PDFName.of("ColorSpace")));
      const group = dict(object.dict.get(PDFName.of("Group")));
      if (group) color(group.get(PDFName.of("CS")));
    }
    if (object instanceof PDFDict) {
      color(object.get(PDFName.of("ColorSpace")));
      const shading = dict(object.get(PDFName.of("Shading")));
      if (shading) color(shading.get(PDFName.of("ColorSpace")));
    }

    if (
      category === "Font" &&
      object instanceof PDFDict &&
      object.get(PDFName.of("Subtype")) === PDFName.of("Type3")
    )
      throw unsupported();
    if (category === "ExtGState" && object instanceof PDFDict) {
      const mask = dict(object.get(PDFName.of("SMask")));
      if (mask) queue(mask.get(PDFName.of("G")), resources);
    }
  };
  for (const index of indices) {
    const page = source.getPages()[index];
    const resources =
      page.node.Resources() ?? PDFDict.withContext(source.context);
    if (!used.has(resources)) used.set(resources, new Map());
    queue(page.node.get(PDFName.of("Contents")), resources);
    const annots = page.node.lookupMaybe(PDFName.of("Annots"), PDFArray);
    if (annots)
      for (const value of annots.asArray()) {
        const annotation = dict(value);
        const appearance = annotation
          ? dict(annotation.get(PDFName.of("AP")))
          : undefined;
        if (appearance)
          for (const [, state] of appearance.entries()) {
            const states = dict(state);
            if (states)
              for (const [, v] of states.entries()) queue(v, resources);
            else queue(state, resources);
          }
      }
  }
  while (work.length) {
    const { streams, resources } = work.pop()!;
    if (!used.has(resources)) used.set(resources, new Map());
    // デバイス色空間と初期の描画状態から暗黙に使われる既定色空間を保持する。
    for (const name of ["DefaultRGB", "DefaultGray", "DefaultCMYK"])
      mark(resources, "ColorSpace", PDFName.of(name));
    const chunks: Uint8Array[] = [];
    let length = 0;
    for (const stream of streams) {
      let bytes = decoded.get(stream);
      if (!bytes) {
        const raw = decodePDFRawStream(stream).getBytes(
          Math.min(budget, 10 * 1024 * 1024) + 1,
        );
        bytes = new Uint8Array(raw.buffer, raw.byteOffset, raw.byteLength);
        decoded.set(stream, bytes);
      }
      length += bytes.length;
      budget -= bytes.length;
      if (budget < 0 || length > 10 * 1024 * 1024) throw unsupported();
      chunks.push(bytes);
    }
    const bytes = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) {
      bytes.set(chunk, offset);
      offset += chunk.length;
    }
    const parser = new ContentParser(
      ByteStream.of(
        new Uint8Array(bytes.buffer, bytes.byteOffset, bytes.byteLength),
      ),
      source.context,
    );
    parser.walk((op, args) => {
      const last = args[args.length - 1];
      if (op === "Do") mark(resources, "XObject", last);
      else if (op === "Tf") mark(resources, "Font", args[args.length - 2]);
      else if (op === "gs") mark(resources, "ExtGState", last);
      else if (op === "CS" || op === "cs") mark(resources, "ColorSpace", last);
      else if (op === "SCN" || op === "scn") mark(resources, "Pattern", last);
      else if (op === "sh") mark(resources, "Shading", last);
      else if (op === "BDC" || op === "DP") mark(resources, "Properties", last);
    });
  }
  // Resourcesの別辞書が同じFont等のサブ辞書を共有する場合も、使用集合を合算してから整理する。
  const shared = new Map<PDFDict, Set<PDFName>>();
  for (const [resources, categories] of used)
    for (const category of [
      "XObject",
      "Font",
      "ExtGState",
      "ColorSpace",
      "Pattern",
      "Shading",
      "Properties",
    ]) {
      const values = dict(resources.get(PDFName.of(category)));
      if (!values) continue;
      let names = shared.get(values);
      if (!names) {
        names = new Set();
        shared.set(values, names);
      }
      for (const name of categories.get(category) ?? []) names.add(name);
    }
  for (const [values, names] of shared)
    for (const [name] of values.entries())
      if (!names.has(name)) values.delete(name);
}
