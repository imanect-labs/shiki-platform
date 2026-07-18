//! スタブプロバイダの決定的ツール駆動（ドキュメント下書き/編集の e2e 用）。
//!
//! `StubProvider` の本体（[`super::stub`]）から切り出した、既知プレフィックス/選択デリミタに
//! 応じて 1 回だけツールを呼ぶロジック。実 LLM 無しで chat run・承認ゲート・Yjs 反映まで
//! パイプライン全体を決定的に叩くための入口（issue #282 / #328 / Task 11.3・11.10）。

use super::stub::tool_call_stream;
use super::stub_fixtures::genui_spec;
use crate::model::GenerateRequest;
use crate::provider::DeltaStream;

/// ドキュメント下書き/編集ツールの決定的駆動（issue #282 / Task 11.3 の e2e）。
/// `savenote:<name>` → save_note（下書き）、`docembed:<node_id>` → document.embed（genui chart）、
/// `saveslide:<name>` → save_slide（下書きスライド・固定 3 枚）、
/// `savecsv:<name>` → save_csv（下書き CSV・固定 3 列×3 行）。
pub(super) fn note_tool_call(
    req: &GenerateRequest,
    user_text: &str,
    prompt_tokens: u64,
) -> Option<DeltaStream> {
    let call = |tool: &str, input: serde_json::Value| {
        req.tools
            .iter()
            .find(|t| t.name == tool)
            .map(|t| tool_call_stream(t.name.clone(), input, prompt_tokens))
    };
    if let Some(name) = user_text.strip_prefix("savenote:").map(str::trim) {
        return call(
            "save_note",
            serde_json::json!({ "name": name, "markdown": format!("# {name}\n\nAI が用意した下書き本文。\n") }),
        );
    }
    if let Some(id) = user_text.strip_prefix("docembed:").map(str::trim) {
        return call(
            "document.embed",
            serde_json::json!({ "node_id": id, "spec": genui_spec("chart") }),
        );
    }
    if let Some(name) = user_text.strip_prefix("savecsv:").map(str::trim) {
        return call(
            "save_csv",
            serde_json::json!({
                "name": name,
                "csv": "商品,数量,単価\nりんご,10,120\nみかん,24,80\nぶどう,3,540\n"
            }),
        );
    }
    if let Some(name) = user_text.strip_prefix("saveslide:").map(str::trim) {
        return call(
            "save_slide",
            serde_json::json!({
                "name": name,
                "theme_id": "plain",
                "slides": [
                    { "html": format!("<div style=\"padding:80px\"><h1>{name}</h1><p>AI が用意した下書きスライド</p></div>"), "notes": "表紙の挨拶" },
                    { "html": "<div style=\"padding:64px\"><h2>アジェンダ</h2><ul><li>背景</li><li>提案</li><li>次の一歩</li></ul></div>" },
                    { "html": "<div style=\"padding:64px\"><h2>まとめ</h2><p>ご清聴ありがとうございました。</p></div>" }
                ]
            }),
        );
    }
    // ノートの選択（node_id 付き）＋編集の依頼キーワードがあれば document.edit を呼ぶ。
    // モックの生成結果として整形テキストを末尾へ追記する（AI 編集パイプラインは本物を通す）。
    if let Some(node_id) = selection_node_id(user_text, "note_selection") {
        if wants_edit(user_text) {
            return call(
                "document.edit",
                serde_json::json!({
                    "node_id": node_id,
                    "ops": [{ "op": "append", "markdown": MOCK_NOTE_EDIT_MD }],
                }),
            );
        }
    }
    // スライドの選択（node_id ＋ locator の slide_id 付き）＋編集キーワード → slide.edit。
    // 表示中スライドを ReplaceSlide で丸ごと差し替え、GrapesJS キャンバスへのライブ反映
    // （observeDeep → slide:load → setComponents）を実パイプラインで検証する（#328・Task 11.10）。
    if let (Some(node_id), Some(slide_id)) = (
        selection_node_id(user_text, "slide_selection"),
        selection_locator_field(user_text, "slide_selection", "slide_id"),
    ) {
        if wants_edit(user_text) {
            return call(
                "slide.edit",
                serde_json::json!({
                    "node_id": node_id,
                    "ops": [{ "op": "replace_slide", "slide_id": slide_id, "html": MOCK_SLIDE_EDIT_HTML }],
                }),
            );
        }
    }
    None
}

/// モック AI が追記する整形テキスト（決定的）。
const MOCK_NOTE_EDIT_MD: &str = "\n## サマリー\n\n- 全社売上は前年同期比 +18%。新規顧客の獲得が牽引。\n- 既存顧客の継続率も 92% → 95% へ改善。\n\n## 課題と次アクション\n\n- 西日本エリアが横ばい（競合の価格施策の影響）。\n- 来期は単価改善とチャネル戦略の見直しを実施する。\n";

/// モック AI が表示中スライドへ差し替える決定的な本文 HTML（e2e が `data-ai-edited` で検出）。
const MOCK_SLIDE_EDIT_HTML: &str =
    "<div style=\"padding:64px\" data-ai-edited=\"1\"><h2>AI が改訂したスライド</h2>\
     <p>選択範囲を踏まえて要点を整理しました。</p><ul><li>結論を先頭に</li><li>数値の根拠を明示</li></ul></div>";

/// 依頼テキストに編集意図のキーワードが含まれるか（要約・質問だけの依頼と区別する）。
fn wants_edit(user_text: &str) -> bool {
    const KEYWORDS: &[&str] = &[
        "整えて",
        "整理",
        "書き直",
        "追記",
        "追加",
        "編集",
        "直して",
        "リライト",
        "まとめて",
    ];
    KEYWORDS.iter().any(|k| user_text.contains(k))
}

/// 注入された選択デリミタ `<selection kind="<kind>" node_id="UUID" ...>` から node_id を取る。
fn selection_node_id(user_text: &str, kind: &str) -> Option<String> {
    let marker = format!("kind=\"{kind}\"");
    let start = user_text.find(&marker)?;
    let after = &user_text[start..];
    let id_start = after.find("node_id=\"")? + "node_id=\"".len();
    let id = &after[id_start..];
    let end = id.find('"')?;
    let candidate = &id[..end];
    // UUID 形（36 文字・ハイフン 4 本）の緩い検証。
    if candidate.len() == 36 && candidate.matches('-').count() == 4 {
        Some(candidate.to_string())
    } else {
        None
    }
}

/// 選択デリミタの `locator={...}`（JSON）から指定フィールドの文字列値を取る
/// （例: slide_selection の `"slide_id":"UUID"`）。history.rs は locator を
/// serde_json の compact Display で織り込むためキーは `"field":"value"` 形になる。
fn selection_locator_field(user_text: &str, kind: &str, field: &str) -> Option<String> {
    let marker = format!("kind=\"{kind}\"");
    let start = user_text.find(&marker)?;
    let after = &user_text[start..];
    let needle = format!("\"{field}\":\"");
    let val_start = after.find(&needle)? + needle.len();
    let rest = &after[val_start..];
    let end = rest.find('"')?;
    Some(rest[..end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slide_selection_extracts_node_and_slide_id() {
        // history.rs が織り込む形（node_id 属性 ＋ locator の compact JSON）。
        let text = "ユーザーは次の選択範囲を参照しています。\n\
            <selection kind=\"slide_selection\" node_id=\"11111111-2222-3333-4444-555555555555\" \
            locator={\"slide_id\":\"abcdef01-2345-6789-abcd-ef0123456789\"}>\n<h1>表紙</h1>\n</selection>\n\
            このスライドを書き直して";
        assert_eq!(
            selection_node_id(text, "slide_selection").as_deref(),
            Some("11111111-2222-3333-4444-555555555555")
        );
        assert_eq!(
            selection_locator_field(text, "slide_selection", "slide_id").as_deref(),
            Some("abcdef01-2345-6789-abcd-ef0123456789")
        );
        assert!(wants_edit(text));
    }

    #[test]
    fn note_selection_without_edit_keyword_is_ignored() {
        let text =
            "<selection kind=\"note_selection\" node_id=\"11111111-2222-3333-4444-555555555555\" \
            locator={}>本文</selection>\nこれは何を意味しますか？";
        // 編集キーワードが無ければ document.edit を呼ばない（質問は編集しない）。
        assert!(!wants_edit(text));
    }
}
