/// Shift_JIS（CP932）のバイト列を作る e2e の手助け（GAP-45）。日本語の Windows の Excel は、CSV を既定で
/// Shift_JIS で保存する。Node の TextEncoder は UTF-8 しか書かないので、テストで使う文字のコードを表で持つ
/// （表は Python の `"字".encode("cp932")` で作った）。表に無い文字を渡すと投げる。

const CODES: Record<string, [number, number]> = {
  中: [0x92, 0x86],
  交: [0x8c, 0xf0],
  件: [0x8c, 0x8f],
  出: [0x8f, 0x6f],
  名: [0x96, 0xbc],
  宿: [0x8f, 0x68],
  張: [0x92, 0xa3],
  態: [0x91, 0xd4],
  承: [0x8f, 0xb3],
  旅: [0x97, 0xb7],
  泊: [0x94, 0x91],
  状: [0x8f, 0xf3],
  申: [0x90, 0x5c],
  経: [0x8c, 0x6f],
  認: [0x94, 0x46],
  請: [0x90, 0xbf],
  費: [0x94, 0xef],
  通: [0x92, 0xca],
  金: [0x8b, 0xe0],
  額: [0x8a, 0x7a],
};

export function shiftJis(text: string): Buffer {
  const bytes: number[] = [];
  for (const ch of text) {
    const code = CODES[ch];
    if (code) bytes.push(...code);
    else if (ch.charCodeAt(0) < 0x80) bytes.push(ch.charCodeAt(0));
    else throw new Error(`Shift_JIS の表に無い文字です: ${ch}`);
  }
  return Buffer.from(bytes);
}
