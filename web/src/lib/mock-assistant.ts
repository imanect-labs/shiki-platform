/// チャット backend（#70 / Phase 3）未実装のあいだ、issue #68 の「テキストを打って
/// 送信するとダミーデータで本番相当の動作」を満たすためのモック応答生成。
///
/// 本物の LLM ストリーミング（SSE）を差し替えやすいよう、`streamMockReply` は
/// トークンを順次 push するコールバック型にしてある。#70 では EventSource/SSE の
/// onmessage からこの onToken/onDone を呼ぶ形に置換できる。

/// ユーザー入力に対する定型応答（プレビュー用とわかる文面）。
export function mockReplyText(userText: string): string {
  const topic = userText.trim().replace(/\s+/g, " ");
  const quoted = topic.length > 24 ? `${topic.slice(0, 24)}…` : topic;
  return [
    `「${quoted}」について承りました。`,
    "",
    "これは Shiki のチャット UI のプレビュー応答です。現在は実際の言語モデルには接続しておらず、権限考慮 RAG と自律エージェントによる本実装は今後のアップデートで有効になります。",
    "",
    "本実装後はここに、あなたがアクセスできるドキュメントだけを根拠にした回答と引用が表示されます。",
  ].join("\n");
}

/// 応答を擬似的にストリーミングする。日本語は単語境界が曖昧なため文字単位で送る。
/// 返り値はキャンセル関数（アンマウント時に呼んでタイマーを止める）。
export function streamMockReply(
  fullText: string,
  onToken: (partial: string) => void,
  onDone: () => void,
  opts: { charsPerTick?: number; intervalMs?: number } = {},
): () => void {
  const charsPerTick = opts.charsPerTick ?? 2;
  const intervalMs = opts.intervalMs ?? 16;
  let index = 0;
  let cancelled = false;

  const timer = setInterval(() => {
    if (cancelled) return;
    index = Math.min(index + charsPerTick, fullText.length);
    onToken(fullText.slice(0, index));
    if (index >= fullText.length) {
      clearInterval(timer);
      onDone();
    }
  }, intervalMs);

  return () => {
    cancelled = true;
    clearInterval(timer);
  };
}
