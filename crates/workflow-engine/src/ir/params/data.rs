//! 構造化データ（data）ノードの params 契約（Task 15.7・data-platform §11）。
//!
//! - **クエリは data の [`DataQuery`] をそのまま使う**（変換層・面ごとの独自フィルタ表現を作らない）。
//!   条件木の語彙は workflow IR の `Condition` と同じで、意味論も同じ 2 値論理（query-optimizer.md §3.1・
//!   `control/eval.rs` と共通の表で固定）。比較値（`cmp.value`・`in` の要素）には `$from` / `$template` を
//!   書け、実行直前に解決してから data へ渡す（値の差し込みであり、条件の表現は変えない）。
//! - 認可はチョークポイント（data の DataStore）側。engine は scope 天井（`data.read` / `data.write`）だけを
//!   担保する（二重ゲート）。schedule / event の run は委譲元ユーザーの権限で行が絞られる（ポート実装）。
//! - 書込（作成・更新・削除・遷移・一括 upsert）は EngineDedup（effect_journal の冪等キーで高々 1 回・PIT-31）。
//! - status は `data.transition` でしか動かせない（FSM の不変条件・`data.record.update` で status を書くと 422）。
//! - 必須の欄（テーブル・値・レコード・次の状態・行）は、項目が無くても保存の契約では誤りにしない（置いたばかりの
//!   ブロックを保存できるように・D-83・BUG-151）。空のまま（項目なし・空の文字・null・中身の無い項目のまとまりや
//!   リスト）なら、保存時の警告にとどめ、実行と自動実行のオンの前に止める（`validate::readiness`）。項目が無いときは
//!   null として読む（実行の前に止まるので、実行で使われることはない）。

use data::DataQuery;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::ParamsIssue;
use crate::ir::expr::ValueExpr;

/// `data.bulk_upsert` の 1 ノードあたりの行数上限（同期で取込を最後まで進めるため・step 出力 256KB と釣り合う）。
pub const MAX_WORKFLOW_BULK_ROWS: usize = 5_000;

/// `data.query` — テーブルから条件に合うレコードを読む（集計も書ける・viewer・行述語とフィールドマスクは実行主体の
/// 権限で適用・pure）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DataQueryParams {
    /// 対象テーブル id（UUID に解決される値）。
    #[serde(default = "super::unset")]
    pub table: ValueExpr,
    /// クエリ（data の `DataQuery`・OpenAPI の `DataQuery` と同一型）。省略時は既定（更新の新しい順に 50 件）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(
        type = "import(\"./api\").components[\"schemas\"][\"DataQuery\"]",
        optional
    )]
    pub query: Option<DataQuery>,
}

impl DataQueryParams {
    /// 読む件数の上限（`query.limit`）は 1〜[`data::MAX_QUERY_LIMIT`]。範囲の外は保存で止める（BUG-108。data は
    /// 黙って 200 件に切り詰めるので、201 以上を書いても読める件数は増えない）。
    pub(crate) fn check_cross_fields(&self) -> Result<(), ParamsIssue> {
        let max = data::MAX_QUERY_LIMIT;
        match self.query.as_ref().and_then(|q| q.limit) {
            Some(limit) if !(1..=max).contains(&limit) => Err(ParamsIssue {
                path: "/params/query/limit".to_string(),
                message: format!("「読む件数の上限」は 1〜{max} 件にしてください。"),
            }),
            _ => Ok(()),
        }
    }
}

/// `data.record.create` — レコードを 1 件作る（editor・EngineDedup）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DataRecordCreateParams {
    /// 対象テーブル id。
    #[serde(default = "super::unset")]
    pub table: ValueExpr,
    /// レコードの値（参照名 key → 値のオブジェクトに解決される値）。
    #[serde(default = "super::unset")]
    pub data: ValueExpr,
}

/// `data.record.update` — レコードを merge patch で更新する（editor ＋書込述語・EngineDedup）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DataRecordUpdateParams {
    /// 対象テーブル id。
    #[serde(default = "super::unset")]
    pub table: ValueExpr,
    /// 対象レコード id。
    #[serde(default = "super::unset")]
    pub record: ValueExpr,
    /// 変更するフィールドだけのオブジェクト（`null` はフィールド除去）。
    #[serde(default = "super::unset")]
    pub patch: ValueExpr,
    /// 楽観ロックの rev（省略時は実行時点の rev を読んでから書く）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expected_rev: Option<ValueExpr>,
    /// 足す数（数の列の参照名 → 足す数・負の数で減らす）。いまの値に足した値を書く（GAP-16）。読んだ rev を前提に
    /// 書き、間にほかの書込があれば読み直して足し直すので、足した数は失われない（`expected_rev` を書いたときは
    /// 読み直さず、その版でなければ失敗する）。`patch` と同じ列は書けない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub add: Option<ValueExpr>,
}

impl DataRecordUpdateParams {
    /// 「足す数」と「変更する値」に同じ列を書いていないこと（リテラルで書いた分だけ・参照は実行時に確かめる）。
    pub(crate) fn check_cross_fields(&self) -> Result<(), ParamsIssue> {
        check_add(self.add.as_ref(), Some(&self.patch), "変更する値", None)
    }
}

/// `data.record.upsert` — 選んだ列の値が同じレコードがあれば書き換え、なければ追加する（GAP-16・EngineDedup）。
///
/// 探すのは `match_on` の列（重複を許さない列・保存時に照合する）の値が、`data` の同じ列の値と等しいレコード
/// （実行主体に見えるものだけ）。見つかれば `data` から探す列を除いた値で書き換え（1 件の更新と同じ入口・書き換えの
/// 権限・読んだ rev を前提に書く）、無ければ `data` で追加する（1 件の作成と同じ入口・追加の権限）。同じ値で並行に
/// 追加しても、列の一意が 2 件目を断るので、断られた側は読み直して書き換えに回る（2 件にならない）。`add` の数の列は、
/// いまの値（追加なら 0）に足して書く。出力は書いたレコードと `created`（追加したら true）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DataRecordUpsertParams {
    /// 対象テーブル id。
    #[serde(default = "super::unset")]
    pub table: ValueExpr,
    /// 同じレコードを探す列の参照名（重複を許さない列）。置いたばかりのブロックは空。
    #[serde(default)]
    pub match_on: String,
    /// 入れる値（参照名 → 値のオブジェクトに解決される値・探す列の値を含む）。
    #[serde(default = "super::unset")]
    pub data: ValueExpr,
    /// 足す数（数の列の参照名 → 足す数）。書き換えるときはいまの値に、追加するときは 0 に足す。`data` と同じ列・
    /// 探す列は書けない。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub add: Option<ValueExpr>,
}

impl DataRecordUpsertParams {
    /// リテラルで書いた「入れる値」に探す列があること、「足す数」が「入れる値」・探す列と重ならないこと。
    pub(crate) fn check_cross_fields(&self) -> Result<(), ParamsIssue> {
        let key = self.match_on.trim();
        if !key.is_empty() {
            if let ValueExpr::Literal(Value::Object(m)) = &self.data {
                if !m.contains_key(key) {
                    return Err(ParamsIssue {
                        path: "/params/data".to_string(),
                        message: format!(
                            "「入れる値」に、同じレコードを探す列「{key}」の値を入れてください。"
                        ),
                    });
                }
            }
        }
        check_add(
            self.add.as_ref(),
            Some(&self.data),
            "入れる値",
            (!key.is_empty()).then_some(key),
        )
    }
}

/// 「足す数」（`add`）の静的な照合（リテラルで書いた分だけ）: 数でない値・値の欄と同じ列・探す列を断る。
///
/// 文章の組み立て（`$template`）は、全体でも列ごとの値でも断る。組み立てた値はいつも文字になる
/// （`control::eval::resolve_deep`）ので、実行で必ず「数を入れてください」で失敗する（BUG-245）。参照（`$from`）は
/// 実行時に数か確かめる。
fn check_add(
    add: Option<&ValueExpr>,
    values: Option<&ValueExpr>,
    values_label: &str,
    match_on: Option<&str>,
) -> Result<(), ParamsIssue> {
    let add = match add {
        Some(ValueExpr::Literal(add)) => add,
        Some(ValueExpr::Template(_)) => {
            return Err(ParamsIssue {
                path: "/params/add".to_string(),
                message: "「足す数」に文章の組み立ては使えません（組み立てた値は文字になります）。列ごとに数を並べてください。".to_string(),
            });
        }
        _ => return Ok(()),
    };
    let Some(add) = add.as_object() else {
        if add.is_null() {
            return Ok(());
        }
        return Err(ParamsIssue {
            path: "/params/add".to_string(),
            message: "「足す数」は、列ごとに数を並べてください。".to_string(),
        });
    };
    let written = match values {
        Some(ValueExpr::Literal(Value::Object(m))) => Some(m),
        _ => None,
    };
    for (k, v) in add {
        let at = format!("/params/add/{k}");
        if match_on == Some(k.as_str()) {
            return Err(ParamsIssue {
                path: at,
                message: format!("同じレコードを探す列「{k}」には足せません。"),
            });
        }
        if written.is_some_and(|m| m.contains_key(k)) {
            return Err(ParamsIssue {
                path: at,
                message: format!(
                    "「{k}」が「{values_label}」と「足す数」の両方にあります。どちらか一方にしてください。"
                ),
            });
        }
        // 文章の組み立ては文字になるので足せない（実行で必ず失敗する・BUG-245）。
        if matches!(
            serde_json::from_value::<ValueExpr>(v.clone()),
            Ok(ValueExpr::Template(_))
        ) {
            return Err(ParamsIssue {
                path: at,
                message: format!(
                    "「足す数」の「{k}」に文章の組み立ては使えません（組み立てた値は文字になり、足せません）。数か、数を指す参照（入力・前のブロックの出力）にしてください。減らすときは負の数を参照します。"
                ),
            });
        }
        // 参照（$from）は実行時に数か確かめる（形の崩れた値式は V5 が言う）。
        let is_expr = v
            .as_object()
            .is_some_and(|o| o.contains_key("$from") || o.contains_key("$template"));
        if !v.is_number() && !is_expr {
            return Err(ParamsIssue {
                path: at,
                message: format!("「足す数」の「{k}」には数を入れてください。"),
            });
        }
    }
    Ok(())
}

/// `data.record.delete` — レコードを削除する（editor ＋書込述語・EngineDedup）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DataRecordDeleteParams {
    /// 対象テーブル id。
    #[serde(default = "super::unset")]
    pub table: ValueExpr,
    /// 対象レコード id。
    #[serde(default = "super::unset")]
    pub record: ValueExpr,
    /// 楽観ロックの rev（省略時は実行時点の rev を読んでから消す）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expected_rev: Option<ValueExpr>,
}

/// `data.transition` — FSM の遷移 API を叩いて status を進める（ガード・actor 述語は data 側で再評価・EngineDedup）。
///
/// FSM の遷移は「現在の status → `to`」で定義される（data の `FsmBody`）。遷移名ではなく行き先の
/// status を指定する（定義外の遷移は 422）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DataTransitionParams {
    /// 対象テーブル id。
    #[serde(default = "super::unset")]
    pub table: ValueExpr,
    /// 対象レコード id。
    #[serde(default = "super::unset")]
    pub record: ValueExpr,
    /// 遷移先の status。
    #[serde(default = "super::unset")]
    pub to: ValueExpr,
    /// 楽観ロックの rev（省略時は実行時点の rev を読んでから遷移する）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub expected_rev: Option<ValueExpr>,
}

/// `data.bulk_upsert` — 行をまとめて作成・更新する（data の一括書込パス・EngineDedup）。
///
/// `rows` は `{ "id"?: uuid, "rev"?: number, "key"?: string, "data": {...} }` の配列に解決される値。
/// `id` のある行は既存レコードの更新（`data` は merge patch）、無い行は新規作成。更新行は 1 件の
/// 更新と同じ入口（行述語つきロック・書込述語・rev・マスク列・検証器・outbox）を通り、誤りはその行だけを
/// 拒否する。出力は件数と拒否行（行番号・行キー・理由）。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(export)]
pub struct DataBulkUpsertParams {
    /// 対象テーブル id。
    #[serde(default = "super::unset")]
    pub table: ValueExpr,
    /// 行の配列（最大 5,000 行）。
    #[serde(default = "super::unset")]
    pub rows: ValueExpr,
    /// 不合格の行が 1 つでもあれば何も適用しない（既定は合格した行だけを適用する）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub abort_on_invalid: Option<bool>,
    /// 同じレコードを探す列の参照名（重複を許さない列・GAP-16）。書くと、`id` の無い行はこの列の値が同じレコード
    /// （実行主体に見えるもの）を探し、あれば更新、なければ追加する（再実行しても同じ値の行が 2 件にならない）。
    /// 行の `data` にこの列の値が要り、同じ値の行が 2 つあると何も登録しない。省略・空は従来どおり（`id` の無い行は
    /// すべて追加）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub match_on: Option<String>,
}

/// `data.bulk_upsert` の `rows` の 1 行（GAP-131）。
///
/// 実行（`nodes::capability_data::parse_bulk_rows`）はこの型で行を読み、AI が読む params の形（[`super::params_shape`]）は
/// この型の宣言から `rows` に書く値の形を言う（形を説明文に写し書きしない）。値の種類（`id` は UUID・`rev` は整数）は
/// 読む側が確かめ、何行目が駄目かを言う。ここに無いキーは [`Self::unknown`] に集まり、実行で断る。TS には書き出さない
/// （`data` の型の表記は AI 向けの書き方）。
#[derive(Debug, Clone, Default, PartialEq, Deserialize, TS)]
pub struct DataBulkRow {
    /// 更新するレコードの id（無ければ追加）。null は書いていないのと同じ。
    #[serde(default)]
    #[ts(optional, type = "string")]
    pub id: Option<Value>,
    /// 更新の楽観ロックの rev（書いたときだけ照らす）。null は書いていないのと同じ。
    #[serde(default)]
    #[ts(optional, type = "number")]
    pub rev: Option<Value>,
    /// 行の印（誤りの報告に出る・取込の中で重ねない）。
    #[serde(default)]
    #[ts(optional, type = "string")]
    pub key: Option<Value>,
    /// 書く値（列の参照名 → 値。更新の行では書いた列だけを変える）。書いてあれば null も値として渡す。
    #[serde(default, deserialize_with = "written")]
    #[ts(type = "{ [列の参照名]: unknown }")]
    pub data: Option<Value>,
    /// 行に書けないキー（実行で断る）。
    #[serde(flatten)]
    #[ts(skip)]
    pub unknown: serde_json::Map<String, Value>,
}

/// 書いてある項目は null も値として受ける（`Option` の既定では null が「書いていない」になる）。
fn written<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

impl DataBulkUpsertParams {
    /// 照合に使う列（空白だけは書いていないのと同じ）。
    #[must_use]
    pub fn match_on(&self) -> Option<&str> {
        self.match_on
            .as_deref()
            .map(str::trim)
            .filter(|k| !k.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use crate::ir::params::check_params;
    use crate::vocab::NodeType;

    /// 置いたばかりで必須の欄（テーブル・値・レコード・次の状態・行）が無いブロックも、保存の契約は通す（実行と自動実行の
    /// オンの前に止める・BUG-151）。以前は「項目 'data' がありません」で保存が止まった。
    #[test]
    fn freshly_placed_blocks_pass_without_their_required_fields() {
        for nt in [
            NodeType::DataQuery,
            NodeType::DataRecordCreate,
            NodeType::DataRecordUpdate,
            NodeType::DataRecordUpsert,
            NodeType::DataRecordDelete,
            NodeType::DataTransition,
            NodeType::DataBulkUpsert,
        ] {
            assert!(check_params(nt, &json!({})).is_ok(), "{nt:?}");
            let parsed = crate::ir::params::parse::<super::DataRecordCreateParams>(&json!({}));
            assert!(parsed.is_ok(), "{parsed:?}");
        }
        // 知らない項目は、これまでどおり断る。
        assert!(check_params(NodeType::DataRecordCreate, &json!({ "rows": [] })).is_err());
    }

    /// 読む件数の上限は 1〜200。範囲の外は保存で止め、直す欄（`/params/query/limit`）を指す（BUG-108。以前は
    /// 201 以上も保存でき、実行で黙って 200 件に切り詰められていた）。
    #[test]
    fn query_limit_must_be_within_the_page_size() {
        let params = |limit: serde_json::Value| json!({ "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1", "query": { "limit": limit } });
        for ok in [1, 50, 200] {
            assert!(
                check_params(NodeType::DataQuery, &params(json!(ok))).is_ok(),
                "{ok}"
            );
        }
        assert!(check_params(
            NodeType::DataQuery,
            &json!({ "table": "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1" })
        )
        .is_ok());
        for bad in [0, 201, 1000] {
            let issue = check_params(NodeType::DataQuery, &params(json!(bad))).unwrap_err();
            assert_eq!(issue.path, "/params/query/limit", "{bad}");
            assert_eq!(
                issue.message,
                "「読む件数の上限」は 1〜200 件にしてください。"
            );
        }
    }

    /// キーで追加か更新（GAP-16）: リテラルで書いた「入れる値」に探す列が要り、「足す数」は数で、値の欄・探す列と
    /// 重ならない。参照（$from）の値は実行時に確かめる。置いたばかりのブロック（項目なし）は通す。
    #[test]
    fn upsert_and_add_are_checked_where_written_literally() {
        let table = "8c8a6f6e-2ab7-4a44-a815-9a2b53c4e9a1";
        let ok = json!({ "table": table, "match_on": "sku",
                         "data": { "sku": { "$from": "input", "path": "/sku" }, "name": "ねじ" },
                         "add": { "stock": { "$from": "input", "path": "/qty" }, "sold": 1 } });
        assert!(check_params(NodeType::DataRecordUpsert, &ok).is_ok());
        assert!(check_params(NodeType::DataRecordUpsert, &json!({})).is_ok());
        // 入れる値を参照で渡すときは、探す列の有無は実行時に確かめる。
        assert!(check_params(
            NodeType::DataRecordUpsert,
            &json!({ "table": table, "match_on": "sku", "data": { "$from": "input" } })
        )
        .is_ok());
        let missing = check_params(
            NodeType::DataRecordUpsert,
            &json!({ "table": table, "match_on": "sku", "data": { "name": "ねじ" } }),
        )
        .unwrap_err();
        assert_eq!(missing.path, "/params/data");
        assert!(missing.message.contains("「sku」"), "{}", missing.message);
        let on_key = check_params(
            NodeType::DataRecordUpsert,
            &json!({ "table": table, "match_on": "sku", "data": { "sku": "A" }, "add": { "sku": 1 } }),
        )
        .unwrap_err();
        assert_eq!(on_key.path, "/params/add/sku");
        let both = check_params(
            NodeType::DataRecordUpsert,
            &json!({ "table": table, "match_on": "sku", "data": { "sku": "A", "stock": 3 }, "add": { "stock": 1 } }),
        )
        .unwrap_err();
        assert_eq!(both.path, "/params/add/stock");
        let not_number = check_params(
            NodeType::DataRecordUpdate,
            &json!({ "table": table, "record": table, "add": { "stock": "1" } }),
        )
        .unwrap_err();
        assert_eq!(not_number.path, "/params/add/stock");
        assert_eq!(
            not_number.message,
            "「足す数」の「stock」には数を入れてください。"
        );
        let overlap = check_params(
            NodeType::DataRecordUpdate,
            &json!({ "table": table, "record": table, "patch": { "stock": 1 }, "add": { "stock": -1 } }),
        )
        .unwrap_err();
        assert_eq!(overlap.path, "/params/add/stock");
        assert!(check_params(
            NodeType::DataRecordUpdate,
            &json!({ "table": table, "record": table, "add": { "stock": -1 } })
        )
        .is_ok());
        // 文章の組み立ては文字になるので、足す数には書けない（列ごとでも全体でも・更新でも追加か更新でも・BUG-245）。
        // 参照は実行時に数か確かめる。
        let template =
            json!({ "$template": "-{q}", "vars": { "q": { "$from": "input", "path": "/qty" } } });
        for (nt, params) in [
            (
                NodeType::DataRecordUpdate,
                json!({ "table": table, "record": table, "add": { "stock": template.clone() } }),
            ),
            (
                NodeType::DataRecordUpsert,
                json!({ "table": table, "match_on": "sku", "data": { "sku": "A" },
                        "add": { "stock": template.clone() } }),
            ),
        ] {
            let issue = check_params(nt, &params).unwrap_err();
            assert_eq!(issue.path, "/params/add/stock", "{nt:?}");
            assert!(
                issue.message.contains("文章の組み立て") && issue.message.contains("負の数"),
                "{}",
                issue.message
            );
        }
        let whole = check_params(
            NodeType::DataRecordUpdate,
            &json!({ "table": table, "record": table, "add": template }),
        )
        .unwrap_err();
        assert_eq!(whole.path, "/params/add");
        for delta in [
            json!({ "$from": "input", "path": "/delta" }),
            json!({ "$from": "nodes.neg.output", "path": "/delta" }),
        ] {
            assert!(check_params(
                NodeType::DataRecordUpdate,
                &json!({ "table": table, "record": table, "add": { "stock": delta } })
            )
            .is_ok());
        }
        // まとめて登録の照合する列は、空白だけなら書いていないのと同じ。
        let p: super::DataBulkUpsertParams =
            serde_json::from_value(json!({ "table": table, "rows": [], "match_on": " " })).unwrap();
        assert_eq!(p.match_on(), None);
        let p: super::DataBulkUpsertParams =
            serde_json::from_value(json!({ "table": table, "rows": [], "match_on": "id_no" }))
                .unwrap();
        assert_eq!(p.match_on(), Some("id_no"));
    }
}
