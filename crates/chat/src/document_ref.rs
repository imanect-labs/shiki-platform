//! AI が作成/編集した文書への参照ペイロード（document_ref・#381）。
//!
//! 現状 `office.live_edit` / `office.edit` / `document.edit` / `csv.patch` は観測テキストを
//! 返すだけで、UI はそれを描画しない（#358）。結果として「AI が Excel を 2 回編集して v3 に
//! した」作業が思考プロセス内の畳まれたチップだけになり、成果物への導線がどこにも無かった。
//!
//! ここで作る `{id, name, kind, version}` が `AgentEvent::DocumentRef` →
//! `StreamEventKind::DocumentRef` → `ContentBlock::DocumentRef` を通ってカードになる
//! （`note_ref` と同型の経路）。**kind の判定はサーバ側 1 箇所**に置き、フロントは
//! それを見て遷移先を決めるだけにする（拡張子判定を UI に散らさない）。

use uuid::Uuid;

/// 参照カードの遷移先種別（フロントの href 分岐の正本）。
///
/// - `office`: docx/xlsx/pptx → `/office/{id}`（Collabora）
/// - `note`: md → `/notes/{id}`
/// - `slide`: .slide → `/slides/{id}`
/// - `csv`: csv → `/csv/{id}`
/// - `file`: それ以外（ドライブのプレビューへ落とす）
pub fn kind_for(name: &str) -> &'static str {
    let ext = std::path::Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    match ext.as_deref() {
        Some("docx" | "xlsx" | "pptx") => "office",
        Some("md") => "note",
        Some("slide") => "slide",
        Some("csv") => "csv",
        _ => "file",
    }
}

/// `document_ref` ブロックのペイロードを組む。
///
/// `version` は分かるときだけ載せる（ライブ編集の保存が未確認な場合など、嘘の版を
/// 載せるくらいなら省く方が正直）。
pub fn payload(node_id: Uuid, name: &str, version: Option<i64>) -> serde_json::Value {
    serde_json::json!({
        "id": node_id.to_string(),
        "name": name,
        "kind": kind_for(name),
        "version": version,
        "created": false,
    })
}

/// **新規作成**した文書の参照（`created: true`）。
///
/// フロントはこのフラグを見たときだけエディタへ自動遷移する。編集の参照で遷移すると
/// 会話中のユーザーを勝手に画面外へ連れて行くことになるため、両者を必ず区別する。
pub fn created_payload(node_id: Uuid, name: &str, version: Option<i64>) -> serde_json::Value {
    let mut v = payload(node_id, name, version);
    v["created"] = serde_json::Value::Bool(true);
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_maps_extensions_to_editors() {
        assert_eq!(kind_for("報告.docx"), "office");
        assert_eq!(kind_for("売上.XLSX"), "office");
        assert_eq!(kind_for("提案.pptx"), "office");
        assert_eq!(kind_for("議事録.md"), "note");
        assert_eq!(kind_for("deck.slide"), "slide");
        assert_eq!(kind_for("data.csv"), "csv");
        assert_eq!(kind_for("photo.png"), "file");
        assert_eq!(kind_for("拡張子なし"), "file");
    }

    /// version 未確定は null で載る（欠落キーにせず「不明」を明示する）。
    #[test]
    fn payload_carries_kind_and_optional_version() {
        let id = Uuid::nil();
        let with = payload(id, "報告.docx", Some(3));
        assert_eq!(with["kind"], "office");
        assert_eq!(with["version"], 3);
        assert_eq!(with["id"], id.to_string());
        let without = payload(id, "報告.docx", None);
        assert!(without["version"].is_null());
    }

    /// 編集は自動遷移しない・作成だけが遷移する（created で区別する）。
    #[test]
    fn only_creation_is_marked_for_navigation() {
        let id = Uuid::nil();
        assert_eq!(payload(id, "報告.docx", None)["created"], false);
        assert_eq!(created_payload(id, "報告.docx", None)["created"], true);
    }
}
