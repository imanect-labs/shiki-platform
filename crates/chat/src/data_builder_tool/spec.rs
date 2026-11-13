//! テーブルを作る・直す道具の入力（列・行の決まり・列の見せ方・状態の進め方）と説明文。
//!
//! 列は data の [`FieldDef`] をそのまま受ける（道具の側に列の写しを持たない）。受ける前に、AI の経路で作らせない
//! もの（対称のリンク・R6）を断る。直すときの列は「足す列と変える列」だけを受け、今の列に参照名で重ねる
//! （[`merge_fields`]・書かなかった列は今のまま）。モデルが全部の列を書き写すと、サーバだけが持つ値（リンクの
//! 逆方向の列・派生列の材料化）を落とすため。

use std::fmt::Write as _;

use data::{FieldDef, FieldPolicy, FieldType, FsmBody, FsmTransition, RowPolicy, TableSchema};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::tool_args::ObjectArg;

/// 状態の進め方（`fsm`）。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct FsmSpec {
    /// 状態の列（選択肢の列の参照名）。選択肢は `states` に置き換わる。
    pub status_field: String,
    /// 状態の一覧（先頭が最初の状態）。
    pub states: Vec<String>,
    /// 進め方（from → to と進められる人）。
    pub transitions: Vec<FsmTransition>,
    /// 「最初の状態を過ぎたら、申請者は書き換えられない」決まりの申請者の列（作成者の列かメンバーの列）。
    #[serde(default)]
    pub applicant_write_lock: Option<String>,
}

impl FsmSpec {
    pub(super) fn body(&self) -> FsmBody {
        FsmBody {
            states: self.states.clone(),
            transitions: self.transitions.clone(),
        }
    }
}

/// `data_table_create` の入力。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CreateInput {
    pub name: String,
    pub fields: Vec<Value>,
    #[serde(default)]
    pub fsm: Option<FsmSpec>,
    #[serde(default)]
    pub row_policy: Option<RowPolicy>,
    #[serde(default)]
    pub field_policy: Option<Vec<FieldPolicy>>,
}

/// `data_table_update` の入力。
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct UpdateInput {
    pub table_id: String,
    pub expected_schema_version: i64,
    #[serde(default)]
    pub fields: Option<Vec<Value>>,
    #[serde(default)]
    pub fsm: Option<FsmSpec>,
    #[serde(default)]
    pub row_policy: Option<RowPolicy>,
    #[serde(default)]
    pub field_policy: Option<Vec<FieldPolicy>>,
}

/// object の引数（JSON の文字列で来ても 1 段だけ読む・GAP-130）。
pub(super) const OBJECT_ARGS: [ObjectArg; 2] = [
    ObjectArg {
        key: "fsm",
        shape: "status_field・states・transitions を持つオブジェクト",
        sql_arg: None,
    },
    ObjectArg {
        key: "row_policy",
        shape: "read と write（省略可）の述語を持つオブジェクト",
        sql_arg: None,
    },
];

/// 列の定義（object）の参照名（`key`・v1 の別名 `name`）。
fn key_of(raw: &Value) -> Option<&str> {
    raw.get("key")
        .or_else(|| raw.get("name"))
        .and_then(Value::as_str)
}

/// AI の経路で受けない列の書き方を断る（対称のリンク・逆方向の列）。
fn reject_server_only(raw: &Value) -> Result<(), String> {
    let key = key_of(raw).unwrap_or("?");
    let Some(link) = raw.get("link").filter(|l| !l.is_null()) else {
        return Ok(());
    };
    if link.get("symmetric_key").is_some_and(|v| !v.is_null()) {
        return Err(format!(
            "列「{key}」: 対称のリンク（link.symmetric_key）は作れません。参照先のテーブルの列も増えるためです。\
             片方向のリンク（symmetric_key を書かない record_ref）にしてください。逆向きの列が要るなら、参照先の\
             持ち主が画面で作ります。"
        ));
    }
    if link.get("reverse").and_then(Value::as_bool) == Some(true) {
        return Err(format!(
            "列「{key}」: 逆方向の列（link.reverse）は書けません（サーバが作る列です）。"
        ));
    }
    Ok(())
}

fn parse_field(raw: Value) -> Result<FieldDef, String> {
    if !raw.is_object() {
        return Err(
            "fields の各要素は列の定義のオブジェクトにしてください（key・type など）。".into(),
        );
    }
    reject_server_only(&raw)?;
    let key = key_of(&raw).unwrap_or("?").to_string();
    serde_json::from_value(raw).map_err(|e| format!("列「{key}」の定義を読めません: {e}"))
}

/// 新しいテーブルの列。
pub(super) fn parse_fields(raw: Vec<Value>) -> Result<Vec<FieldDef>, String> {
    raw.into_iter().map(parse_field).collect()
}

/// 直すときの列の重ね方の結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Merged {
    pub fields: Vec<FieldDef>,
    pub added: Vec<String>,
    pub changed: Vec<String>,
}

/// 今の列に、足す列と変える列を参照名で重ねる（書かなかった列は今のまま）。
///
/// 変える列は、今の定義の JSON に、書いたキーだけを上から重ねる（`id`・`key` は今のまま）。足す列は新しい列として
/// 読む。どちらも対称のリンクは断る。同じ参照名を 2 回書いたら断る。
pub(super) fn merge_fields(current: &[FieldDef], raw: Vec<Value>) -> Result<Merged, String> {
    let mut out = Merged {
        fields: current.to_vec(),
        ..Merged::default()
    };
    let mut seen = std::collections::HashSet::new();
    for item in raw {
        let Some(key) = key_of(&item).map(str::to_string) else {
            return Err("fields の各要素に参照名（key）を書いてください。".into());
        };
        if !seen.insert(key.clone()) {
            return Err(format!(
                "列「{key}」を 2 回書いています。1 つにまとめてください。"
            ));
        }
        reject_server_only(&item)?;
        let Some(at) = out.fields.iter().position(|f| f.key == key) else {
            out.fields.push(parse_field(item)?);
            out.added.push(key);
            continue;
        };
        let mut base = serde_json::to_value(&out.fields[at])
            .map_err(|e| format!("列「{key}」の今の定義を読めません: {e}"))?;
        if let (Value::Object(base), Value::Object(over)) = (&mut base, item) {
            for (k, v) in over {
                if matches!(k.as_str(), "id" | "key" | "name") {
                    continue;
                }
                base.insert(k, v);
            }
        }
        out.fields[at] = serde_json::from_value(base)
            .map_err(|e| format!("列「{key}」の定義を読めません: {e}"))?;
        out.changed.push(key);
    }
    Ok(out)
}

/// 新しいテーブルのスキーマ（状態の進め方は作った後に [`data::FsmStore::save_for_table`] で付ける）。
pub(super) fn new_schema(
    fields: Vec<FieldDef>,
    row_policy: Option<RowPolicy>,
    field_policy: Option<Vec<FieldPolicy>>,
) -> TableSchema {
    TableSchema {
        fields,
        status_field: None,
        row_policy,
        field_policy: field_policy.unwrap_or_default(),
        aggregate_min_rows: None,
        fsm_ref: None,
        rag: None,
    }
}

/// 列の型の説明（説明文に載せる・型の列挙は [`FieldType::ALL`] から作る）。
fn type_note(t: FieldType) -> &'static str {
    match t {
        FieldType::Text => "文字",
        FieldType::Number => "数（小数の桁と範囲は numeric_spec: {scale, min, max}）",
        FieldType::Date => "日付 YYYY-MM-DD",
        FieldType::DateTime => "日時（RFC3339）",
        FieldType::Select => "選択肢から 1 つ（options 必須）",
        FieldType::MultiSelect => "選択肢から複数（options 必須）",
        FieldType::UserRef => "メンバー（ユーザー）",
        FieldType::RoleRef => "ロール（部署）",
        FieldType::FileRef => "ドライブのファイル",
        FieldType::RecordRef => "別のテーブルのレコード（ref_table にテーブル id。片方向のリンク）",
        FieldType::CreatedBy => {
            "レコードの作成者（自動で入る・書き換えられない。申請者の列に使う）"
        }
        // 派生列は受けるが勧めない（v1・D-10 の設計 §5.1）。
        FieldType::Lookup | FieldType::Computed | FieldType::Rollup | FieldType::Formula => {
            "（派生列・通常は使わない）"
        }
    }
}

/// 説明文の列の型の一覧（派生列は載せない）。
pub(super) fn type_list() -> String {
    let mut out = String::new();
    for t in FieldType::ALL.into_iter().filter(|t| !t.is_derived()) {
        let _ = writeln!(out, "- {}: {}", t.wire_name(), type_note(t));
    }
    out
}

/// 行の決まり（`row_policy`）の例。テストで data の検証に通して固定する。
pub(super) const ROW_POLICY_EXAMPLE: &str = r#"{"read":{"any":[{"field_cmp":{"field":"applicant","op":"eq","value":"user_id"}},{"has_role":{"role":"経理","subtree":true}}]}}"#;

/// 状態の進め方（`fsm`）の例。テストで data の検証に通して固定する。
pub(super) const FSM_EXAMPLE: &str = r#"{"status_field":"status","states":["申請中","承認","却下"],"transitions":[{"from":"申請中","to":"承認","actor":{"has_role":{"role":"経理","subtree":true}}},{"from":"申請中","to":"却下","actor":{"has_role":{"role":"経理","subtree":true}}}],"applicant_write_lock":"applicant"}"#;

/// 述語（進められる人・行の決まり）の書き方。
pub(super) const PREDICATE_NOTE: &str = "述語の書き方（閉じた文法・否定は無い）:\n\
- \"public\" テーブルを見られる人全員 / \"is_owner\" レコードを作った人\n\
- {\"has_role\":{\"role\":\"<ロール>\",\"subtree\":true}} そのロール（配下を含む）の人\n\
- {\"field_cmp\":{\"field\":\"<列>\",\"op\":\"eq\"|\"ne\"|\"in\",\"value\":\"user_id\"|{\"lit\":<値>}}} 行の値との比べ（\"user_id\" は話している本人）\n\
- {\"any\":[…]} どれか / {\"all\":[…]} 全部\n";

/// 観測に載せる列の 1 行（参照名・表示名・型・必須・選択肢）。値はこちらが受けて data が検証したもの。
pub(super) fn field_line(f: &FieldDef) -> String {
    let mut s = format!(
        "{}（{}・{}",
        f.key,
        super::shown(&f.display_name),
        f.field_type.wire_name()
    );
    if f.required {
        s.push_str("・必須");
    }
    if f.unique {
        s.push_str("・重複不可");
    }
    if !f.options.is_empty() {
        let opts: Vec<String> = f.options.iter().map(|o| super::shown(o)).collect();
        let _ = write!(s, "・選択肢: {}", opts.join("/"));
    }
    s.push('）');
    s
}

/// object でない `fields` の要素などを含む入力を断る文（serde の誤りを道具の言葉に）。
pub(super) fn invalid_input(e: &serde_json::Error) -> String {
    format!("入力を読めません: {e}。説明の入力の形に合わせて呼び直してください。")
}

/// 入力を読む（object の引数の文字列を 1 段だけ読み、未知のキーは断る）。
pub(super) fn read_input<T: for<'de> Deserialize<'de>>(mut input: Value) -> Result<T, String> {
    crate::tool_args::accept_object_args(&mut input, &OBJECT_ARGS)?;
    // fields（配列）が JSON の文字列で来たら 1 段だけ読む（object の引数と同じ扱い）。
    if let Some(slot) = input.get_mut("fields") {
        let parsed = match slot {
            Value::String(raw) => serde_json::from_str::<Value>(raw).ok(),
            _ => None,
        };
        if let Some(parsed @ Value::Array(_)) = parsed {
            *slot = parsed;
        }
    }
    serde_json::from_value(input).map_err(|e| invalid_input(&e))
}

/// 列の一覧の比較に使う形（作成の冪等・R1）。参照名・型・必須・重複不可・選択肢・参照先。
pub(super) fn field_shape(f: &FieldDef) -> Value {
    let mut m = Map::new();
    m.insert("key".into(), Value::String(f.key.clone()));
    m.insert("type".into(), Value::String(f.field_type.wire_name()));
    m.insert("required".into(), Value::Bool(f.required));
    m.insert("unique".into(), Value::Bool(f.unique));
    m.insert(
        "options".into(),
        Value::Array(f.options.iter().cloned().map(Value::String).collect()),
    );
    m.insert(
        "ref_table".into(),
        f.ref_table
            .map_or(Value::Null, |t| Value::String(t.to_string())),
    );
    Value::Object(m)
}
