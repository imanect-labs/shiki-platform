//! deep research スタブが出す genui スペック（`stub_deep_research.rs` から分割・行数規約）。
//!
//! 質問カード・計画カード・出典カードの固定データだけを持つ。フェーズの進み方は
//! `stub_deep_research.rs` が決める。

/// 質問カード（最大 3 問・1 ターン・自由記述あり）。
pub(super) fn deep_research_question_spec() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "actions": [{ "type": "handler", "id": "answer", "handler": "chat.submit" }],
        "root": {
            "component": "question_card",
            "id": "dr-clarify",
            "title": "調査の前に確認させてください",
            "intro": "答えが変わる軸だけ伺います（すべて任意）。",
            "submit": { "action": "answer" },
            "submit_label": "この条件で進める",
            "questions": [
                {
                    "id": "scope",
                    "header": "対象範囲",
                    "question": "どの範囲を対象にしますか？",
                    "options": [
                        { "label": "国内のみ", "description": "日本市場に絞る" },
                        { "label": "国内＋海外", "description": "海外の比較も含める" },
                        { "label": "特に希望なし", "description": "こちらで判断する" }
                    ],
                    "allow_other": true
                },
                {
                    "id": "depth",
                    "header": "深さ",
                    "question": "どこまで踏み込みますか？",
                    "options": [
                        { "label": "概観", "description": "全体像がつかめれば十分" },
                        { "label": "意思決定用", "description": "数値の出所と反対意見まで" },
                        { "label": "特に希望なし", "description": "こちらで判断する" }
                    ],
                    "allow_other": true
                }
            ]
        }
    })
}

/// 計画カード（開始ボタンで承認を取る）。
///
/// **中身は「この依頼固有の問い」**にする。手順（視点を分ける・証拠台帳を作る・節ごとに執筆）は
/// どの調査でも同じでユーザーの判断材料にならないため、見本にも置かない
/// （instructions の「悪い例」と同じものをスタブが出していては回帰点にならない）。
pub(super) fn deep_research_plan_spec() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "actions": [{ "type": "handler", "id": "start", "handler": "chat.submit" }],
        "root": {
            "component": "plan_card",
            "id": "dr-plan",
            "title": "2026 年の国内 SaaS 市場規模と成長率の検証",
            "intro": "「いくらか」だけでなく、公表値がなぜ食い違うのかまで押さえて幅で答えます。                      海外市場と 2027 年以降の予測は対象外です（必要なら書き足してください）。",
            "submit": { "action": "start" },
            "submit_label": "この計画で開始",
            "allow_revise": true,
            "steps": [
                {
                    "title": "市場規模の「定義」を揃える",
                    "description": "調査会社ごとに SaaS の範囲（PaaS・受託開発の扱い）が違う。主要 3 社の定義差を先に押さえ、比較可能な数字だけを採用する"
                },
                {
                    "title": "2026 年の実数はいくらか",
                    "description": "総務省の通信利用動向調査と主要ベンダの決算（開示ベース）を突き合わせ、公表値の幅（下限〜上限）として出す"
                },
                {
                    "title": "成長率は鈍化しているか",
                    "description": "2022〜2026 の前年比を並べ、伸びが価格転嫁か新規需要かを ARPU と契約社数の内訳で切り分ける"
                },
                {
                    "title": "国内固有の要因は何か",
                    "description": "SI 経由の商流・オンプレ回帰の議論・為替が価格に与える影響を、日本語一次資料で確認する"
                },
                {
                    "title": "強気な予測の根拠は妥当か",
                    "description": "予測値を出している主体と前提条件を並べ、過去予測の的中度で信頼度を評価する"
                },
                {
                    "title": "社内資料と食い違わないか",
                    "description": "過去の市場調査・事業計画の前提と突き合わせ、差分があれば明示する"
                }
            ]
        }
    })
}

/// 出典カード（本文で引用したものだけ・一次を上に）。
pub(super) fn deep_research_source_spec() -> serde_json::Value {
    serde_json::json!({
        "version": 1,
        "root": {
            "component": "source_card",
            "title": "出典",
            "sources": [
                {
                    "title": "国内 SaaS 市場調査（2026 年版）",
                    "snippet": "市場規模は 1 兆 2000 億円に達した",
                    "url": "https://example.com/stub-1",
                    "label": "一次"
                },
                {
                    "title": "別集計（2026 年 3 月時点）",
                    "snippet": "9800 億円と推計",
                    "url": "https://example.org/stub-2",
                    "label": "二次"
                }
            ]
        }
    })
}
