/// stub の LLM（crates/llm-gateway/src/providers/stub*.rs）を動かす語を組む。実 LLM は呼ばない。
///
/// 駆動の語は入力と fixture にだけ置き、本番の IR には入れない（例外は spec に明記する: APP-10 の slice の script が
/// json: の prompt を組む・APP-11 の同時押しの確かめ用の版）。model はいつも stub の id を明示する（BUG-114・D-196・
/// world.ts の `{{model.chat}}`・`{{model.decide}}`）。
///
/// - ai.decide（stub_decider.rs）: 材料に `<質問のキー>:yes` / `:no`・選択肢のキーがあれば確からしさ 0.9、
///   `stub:fail` で失敗、`stub:malformed` で形の崩れた答え。
/// - 形を指定した llm.invoke（stub_structured.rs）: どこかの行の頭に `json:{…}`（後ろに本文が続いてよい）・
///   `jsonalways:`（修復の依頼にも毎回同じ）・`refuse:`（拒否）・`truncate:`（max_tokens で切れる）。
///   選択肢の一覧と指示は llm.invoke の `system` に置き、prompt にはデータだけを入れる（stub は user の本文に先に
///   出た enum の値を選ぶ・system は見ない）。
/// - 形を指定しない llm.invoke（stub.rs）: 既定は「回答: <prompt>」をそのまま返す。`fail:`（どこかの行の頭）で
///   送り直されない誤り、`truncate:`（本文の頭）で打ち切り、`slow:N` で N 秒待つ。

/// ai.decide の boolean の質問に「はい」と答えさせる語。
export function decideYes(key: string): string {
  return `${key}:yes`;
}

/// ai.decide の boolean の質問に「いいえ」と答えさせる語。
export function decideNo(key: string): string {
  return `${key}:no`;
}

/// ai.decide を失敗させる語。
export const DECIDE_FAIL = "stub:fail";

/// ai.decide に形の崩れた答えを返させる語。
export const DECIDE_MALFORMED = "stub:malformed";

/// 形を指定した llm.invoke に `value` をそのまま返させる行（`trailing` は後ろに続く本文・本物に近い prompt にする）。
export function jsonLine(value: unknown, trailing = ""): string {
  return `json:${JSON.stringify(value)}${trailing ? `\n${trailing}` : ""}`;
}

/// 修復の依頼にも毎回同じ本文を返させる行（修復しても直らない経路）。
export function jsonAlwaysLine(body: string): string {
  return `jsonalways:${body}`;
}

/// 拒否で終わらせる行。
export const REFUSE_LINE = "refuse:";

/// 途中で max_tokens に達して終わらせる行（形を指定した呼び出し）／本文の頭に置いて打ち切る（形を指定しない呼び出し）。
export function truncateLine(rest = ""): string {
  return `truncate:${rest}`;
}

/// 形を指定しない llm.invoke を、送り直されない誤りで失敗させる行。
export function failLine(reason = ""): string {
  return `fail:${reason}`;
}

/// 応答を `sec` 秒遅らせる語（本文の頭・短い timeout_sec と組み合わせて時間切れを通す）。
export function slow(sec: number, rest = ""): string {
  return `slow:${sec}${rest ? ` ${rest}` : ""}`;
}

/// 形を指定しない llm.invoke の stub の既定の答え（prompt をそのまま折り返す）。
export function echoOf(prompt: string): string {
  return `回答: ${prompt}`;
}
