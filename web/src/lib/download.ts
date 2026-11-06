/// Blob を名前つきでダウンロードさせる（ノート・スライド・チャットの回答の書き出しで共用）。
///
/// どこからでも軽く読めるよう、このモジュールは何にも依存させない（エディタの直列化などを
/// 引き込むと、使う画面の初回読み込みが重くなる）。
export function downloadBlob(blob: Blob, filename: string): void {
  const url = URL.createObjectURL(blob);
  const a = document.createElement("a");
  a.href = url;
  a.download = filename;
  document.body.appendChild(a);
  a.click();
  a.remove();
  // 次の tick で revoke（Safari が click 前に revoke すると失敗するため遅延する）。
  setTimeout(() => URL.revokeObjectURL(url), 1000);
}
