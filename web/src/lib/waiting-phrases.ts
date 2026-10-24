/// 生成中に「次のイベントが来ない」間の見出し。
///
/// モデルが大きなツール入力（Office 文書の中身など）を書き出している間は、イベントが数十秒
/// 届かない。表示が止まると「固まった」と読まれるので、待った時間に応じて文言を進める。
/// フロントには何を書いているかが分からないため、どの局面でも嘘にならない言い方に留める。

/// 待っている局面（直前に何が起きたか）。
export type WaitingContext =
  /// まだ何も届いていない。
  | "start"
  /// ツールの結果が返った後（次の手順を決めている／次の呼び出しを書いている）。
  | "after_tool"
  /// 本文の途中で止まった。
  | "after_text";

/// `[この秒数以上待ったら, 出す文言]`。秒数の昇順。
const PHRASES: Record<WaitingContext, readonly (readonly [number, string])[]> = {
  start: [
    [0, "準備しています"],
    [8, "少し時間がかかっています"],
    [20, "もう少しお待ちください"],
  ],
  after_tool: [
    [0, "結果を確認しています"],
    [4, "次の手順を考えています"],
    [10, "内容を組み立てています"],
    [18, "書き出す内容をまとめています"],
    [35, "時間がかかっています。もう少しお待ちください"],
  ],
  after_text: [
    [0, "続きを考えています"],
    [6, "内容を組み立てています"],
    [15, "書き出す内容をまとめています"],
    [35, "時間がかかっています。もう少しお待ちください"],
  ],
};

/// 待った秒数に応じた文言（末尾の「…」は付けない）。
export function waitingPhrase(context: WaitingContext, idleSecs: number): string {
  const list = PHRASES[context];
  let phrase = list[0][1];
  for (const [at, text] of list) {
    if (idleSecs >= at) phrase = text;
  }
  return phrase;
}

/// 文言が切り替わる秒数（全局面の和集合・昇順）。待ち時間はこの刻みでだけ再描画すればよい。
export const WAITING_STEPS: readonly number[] = [
  ...new Set(Object.values(PHRASES).flatMap((list) => list.map(([at]) => at))),
].sort((a, b) => a - b);
