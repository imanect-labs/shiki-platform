/// ファイル名の拡張子まわり（改名で拡張子を変えさせない・#617）。
///
/// 判定はサーバ（StorageService の `ensure_same_extension`）と揃える: 最後の `.` より後ろが拡張子
/// （`.docx` のような先頭ドットだけの名前も含む・開く画面の判定と同じ読み方）。末尾がドットの
/// ものは拡張子なし。大文字小文字は区別しない。

export function fileExtension(name: string): string | null {
  const i = name.lastIndexOf(".");
  if (i < 0 || i === name.length - 1) return null;
  return name.slice(i + 1);
}

/// 改名後の名前が拡張子を保っているか。保っていなければ利用者向けの理由を返す。
export function extensionChangeError(current: string, next: string): string | null {
  const a = fileExtension(current);
  const b = fileExtension(next.trim());
  if (a === null && b === null) return null;
  if (a !== null && b !== null && a.toLowerCase() === b.toLowerCase()) return null;
  return a !== null
    ? `拡張子は変更できません（.${a} のままにしてください）`
    : "拡張子のないファイルに拡張子は付けられません";
}

/// 改名ダイアログで最初に選択しておく範囲（拡張子の手前まで）。
export function stemRange(name: string): [number, number] {
  const ext = fileExtension(name);
  return [0, ext === null ? name.length : name.length - ext.length - 1];
}
