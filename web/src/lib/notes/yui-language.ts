/// ノートのコードブロックの言語（情報文字列）を Yui（#530）の図の言語へ寄せる。
///
/// ```ui bar / ```UI 棒 / ```yui:Bar のように種類が付いた情報文字列を `ui:bar`（小文字・
/// 正規の種類名・class 1 つ）にする。`ui:bar` は Markdown の往復でも HTML の
/// `class="language-ui:bar"` でも 1 語のまま残るので、種類が落ちない。
///
/// 種類が `@imanect/yui` の resolveKind で分からないもの（`ui a```b` など）は寄せない
/// （undefined を返す）。呼び出し側は従来どおり 1 語目だけを残す。

import { resolveKind } from "@imanect/yui";

const YUI_INFO = /^(ui|yui)(?:\s+|:)(\S+)/i;

/// 情報文字列（`ui bar`・`UI:Bar`）を正規の言語名（`ui:bar`）にする。種類が分からなければ undefined。
export function normalizeYuiInfo(info: string): string | undefined {
  const m = YUI_INFO.exec(info.trim());
  if (!m) return undefined;
  const kind = resolveKind(m[2]);
  return kind ? `${m[1].toLowerCase()}:${kind}` : undefined;
}
