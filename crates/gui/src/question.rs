//! 質問カード（Claude Code の AskUserQuestion 相当・PR4）。
//!
//! AI がユーザーへ質問を提示し回答を集める。フロントは**1 問ずつステップ表示**（別ページの
//! ような体験）し、各選択肢は**ラベル＋説明**のカードで示す。選択肢に無い回答は `allow_other`
//! の自由記述（テキストエリア・長文可）で受ける。短い質問（見出し＋数個の選択肢）も長い質問
//! （説明の多い選択肢や自由記述）も同じ型で表せる。回答は宣言済みアクション（`chat.submit`）
//! へまとめて送信され次ターンの発話になる。信頼境界はフォームと同じ（閉じた集合・
//! `deny_unknown_fields`・任意 URL/コード不可）。数値入力にスライダーは使わない（自由記述か
//! 選択肢で表現する）。

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::spec::ActionRef;

/// AI からユーザーへの質問カード（複数問・ステップ提示）。
///
/// `id` はフォームと同一名前空間で重複検査する（どちらも送信可能な単位のため）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct QuestionCardProps {
    /// カード id（文書内で一意）。
    pub id: String,
    /// 見出し（省略時はフロントが「AI からの質問」を表示）。
    #[serde(default)]
    pub title: Option<String>,
    /// 導入文（何のための質問か・任意）。
    #[serde(default)]
    pub intro: Option<String>,
    /// 送信先アクション（宣言済み束縛の参照のみ）。
    pub submit: ActionRef,
    /// 質問（1 問ずつ提示する）。
    pub questions: Vec<QuestionItem>,
    /// 送信ボタンのラベル（省略時「回答する」）。
    #[serde(default)]
    pub submit_label: Option<String>,
}

/// 1 問。`options` が空なら自由記述（テキストエリア）のみ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct QuestionItem {
    /// 回答値のキー（カード内で一意）。
    pub id: String,
    /// 短い見出しチップ（例「予算」「日程」・任意）。
    #[serde(default)]
    pub header: Option<String>,
    /// 質問文（短くても長くてもよい）。
    pub question: String,
    /// 選択肢（各カードはラベル＋説明。空なら自由記述のみ）。
    #[serde(default)]
    pub options: Vec<QuestionOption>,
    /// 複数選択可（既定は単一選択）。
    #[serde(default)]
    pub multi_select: bool,
    /// 選択肢外の自由記述（「その他」）を許す。
    #[serde(default)]
    pub allow_other: bool,
    /// 自由記述欄のプレースホルダ（options 無し or その他選択時）。
    #[serde(default)]
    pub placeholder: Option<String>,
}

/// 選択肢 1 件（ラベル＋任意の説明）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct QuestionOption {
    /// 選択肢のラベル（回答値になる）。
    pub label: String,
    /// 選択肢の説明（カードの補足行・任意）。
    #[serde(default)]
    pub description: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Serialize/Deserialize の両方向を通す（型のみモジュールのカバレッジ確保）。
    #[test]
    fn question_card_props_roundtrip() {
        let props = QuestionCardProps {
            id: "trip".into(),
            title: Some("AI からの質問".into()),
            intro: Some("旅程を詰めるために教えてください。".into()),
            submit: ActionRef {
                action: "answer".into(),
            },
            questions: vec![
                QuestionItem {
                    id: "purpose".into(),
                    header: Some("目的".into()),
                    question: "今回の旅行の主な目的は何ですか？".into(),
                    options: vec![
                        QuestionOption {
                            label: "観光・レジャー".into(),
                            description: Some("名所や自然を楽しむのが中心".into()),
                        },
                        QuestionOption {
                            label: "出張・ビジネス".into(),
                            description: None,
                        },
                    ],
                    multi_select: false,
                    allow_other: true,
                    placeholder: None,
                },
                QuestionItem {
                    id: "notes".into(),
                    header: None,
                    question: "その他、希望や制約があれば自由にお書きください。".into(),
                    options: vec![],
                    multi_select: false,
                    allow_other: false,
                    placeholder: Some("例: 車椅子で移動します".into()),
                },
            ],
            submit_label: Some("回答する".into()),
        };
        let json = serde_json::to_value(&props).unwrap();
        assert_eq!(
            json["questions"][0]["options"][0]["label"],
            "観光・レジャー"
        );
        assert_eq!(json["questions"][0]["allow_other"], true);
        assert!(json["questions"][1]["options"]
            .as_array()
            .unwrap()
            .is_empty());
        let back: QuestionCardProps = serde_json::from_value(json).unwrap();
        assert_eq!(back, props);
    }
}
