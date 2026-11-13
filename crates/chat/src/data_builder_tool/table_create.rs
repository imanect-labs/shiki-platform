//! `data_table_create`: テーブルを作る（承認カードを経る・作った本人が持ち主・D-10 の T05）。
//!
//! 1. 本人がこの会話の run の間に同じ名前のテーブルを作っていれば作らない（作成の冪等・R1）。
//! 2. 1 回の会話の run で作ったテーブルが上限（[`super::MAX_TABLES_PER_RUN`]）なら作らない。
//! 3. [`DataStore::create_table`]（スキーマの検証・作成者を持ち主に・監査 `DataTableCreate`）。
//! 4. `fsm` があれば [`data::FsmStore::save_for_table`]（状態の列の選択肢を状態の一覧に合わせ、定義を作ってテーブルに
//!    指させる・書き込みの決まり）。空のテーブルなので承認待ちも書き換えられなくなるレコードも無い。
//!
//! 4 で断られたら、作れたテーブルを参照の行つきで返し、`data_table_update` で状態の進め方を足させる（作った空の
//! テーブルを消す補償はしない・消すのは人）。

use std::fmt::Write as _;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use async_trait::async_trait;
use authz::AuthContext;
use data::{DataTable, NewDataTable, SaveTableFsm};
use serde_json::{json, Value};

use super::spec::{
    field_line, field_shape, new_schema, parse_fields, read_input, type_list, CreateInput, FsmSpec,
    FSM_EXAMPLE, PREDICATE_NOTE, ROW_POLICY_EXAMPLE,
};
use super::{failure, shown, BuilderRun, DataBuilderDeps, MAX_TABLES_PER_RUN};
use crate::builder_obs::{already_created, created_differently, BuilderKind, BuilderRef};

/// テーブルを作る道具。
pub struct DataTableCreateTool {
    deps: DataBuilderDeps,
    run: BuilderRun,
    description: String,
}

impl DataTableCreateTool {
    #[must_use]
    pub fn new(deps: DataBuilderDeps, run: BuilderRun) -> Self {
        DataTableCreateTool {
            deps,
            run,
            description: description(),
        }
    }

    /// 本人がこの会話の run の間に作った同名のテーブル（作成の冪等・R1）。
    async fn own_same_name(
        &self,
        ctx: &AuthContext,
        name: &str,
        trace_id: Option<&str>,
    ) -> Result<Option<DataTable>, data::DataError> {
        let found = self
            .deps
            .data
            .own_tables_created_since(ctx, self.run.since, Some(name), trace_id)
            .await?;
        let Some(own) = found.into_iter().next() else {
            return Ok(None);
        };
        self.deps
            .data
            .get_table(ctx, own.id, trace_id)
            .await
            .map(Some)
    }
}

fn description() -> String {
    format!(
        "業務データのテーブル（表）を新しく作る。作ったテーブルの持ち主はユーザー本人で、ほかの人には共有されない\
         （共有はユーザーが画面で行う）。実行の前にユーザーが承認カードで列と決まりを確かめる。\n\
         \n\
         入力:\n\
         - name: テーブル名（組織の中で重ならない名前・128 字まで）\n\
         - fields: 列の配列。各列は {{key, display_name, type, required?, unique?, indexed?, searchable?, options?, ref_table?, description?, numeric_spec?}}。\
         key は英小文字・数字・_ の参照名（あとで変えられない）、display_name は画面の名前（日本語可）。\n\
         - fsm（省略可）: 状態の進め方。{{status_field, states, transitions:[{{from, to, actor}}], applicant_write_lock?}}。\
         status_field は select の列の key（選択肢は states に置き換わる・先頭が最初の状態）。actor は進められる人の述語。\
         applicant_write_lock に申請者の列（created_by か user_ref の列）を書くと、最初の状態を過ぎたら申請者は書き換えられない。\n\
         - row_policy（省略可）: 行の決まり {{read, write?}}。read はレコードを見られる人、write は書き換え・削除できる人（省略は read と同じ）。\
         省略するとテーブルを見られる人全員が全部の行を見られる。\n\
         - field_policy（省略可）: 列の見せ方 [{{field, readable_by}}]（readable_by は has_role・public とその any/all だけ）。\n\
         \n\
         列の型（type）:\n{types}\n\
         {PREDICATE_NOTE}\n\
         例（申請者本人と経理だけが見られる）: row_policy = {ROW_POLICY_EXAMPLE}\n\
         例（経理が承認・却下する）: fsm = {FSM_EXAMPLE}（applicant は type=created_by の列）\n\
         \n\
         - 対称のリンク（link.symmetric_key）は作れない。別のテーブルを指す列は片方向の record_ref にする。\n\
         - 1 回の応答で作れるテーブルは {MAX_TABLES_PER_RUN} つまで。\n\
         - 成功したら末尾の「参照:」の行の id を、続きの道具（data_view_save・data_table_update など）に渡す。\n\
         - 作った後で列を足す・決まりを変えるときは data_table_update を使う（同じ名前で作り直さない）。",
        types = type_list(),
    )
}

/// 作ったテーブルの観測（列・状態の進め方・参照の行）。
fn created(table: &DataTable, fsm: Option<&FsmSpec>) -> ToolOutcome {
    let r = BuilderRef::new(
        BuilderKind::DataTable,
        table.id,
        &table.name,
        table.schema_version,
    );
    let mut text = format!(
        "テーブル「{}」を作りました（id: {}, スキーマ v{}）。持ち主はあなたで、まだ誰にも共有されていません。\n列: {}",
        shown(&table.name),
        table.id,
        table.schema_version,
        table
            .schema
            .fields
            .iter()
            .map(field_line)
            .collect::<Vec<_>>()
            .join(", ")
    );
    if let Some(f) = fsm {
        let moves: Vec<String> = f
            .transitions
            .iter()
            .map(|t| format!("{}→{}", shown(&t.from), shown(&t.to)))
            .collect();
        let _ = write!(
            text,
            "\n状態の進め方: 列 {}・最初の状態「{}」・{}",
            f.status_field,
            f.states.first().map(|s| shown(s)).unwrap_or_default(),
            moves.join(", ")
        );
    }
    if table.schema.row_policy.is_some() {
        text.push_str("\n行の決まり: あり");
    }
    text.push('\n');
    text.push_str(&r.line());
    let mut outcome = ToolOutcome::ok(text);
    outcome.builder_refs.push(r.to_value());
    outcome
}

/// 今回の入力で作られるはずの列の形（状態の列の選択肢は状態の一覧になる）。
fn expected_shapes(fields: &[data::FieldDef], fsm: Option<&FsmSpec>) -> Vec<Value> {
    fields
        .iter()
        .map(|f| {
            let mut f = f.clone();
            if let Some(spec) = fsm.filter(|s| s.status_field == f.key) {
                f.options.clone_from(&spec.states);
            }
            field_shape(&f)
        })
        .collect()
}

#[async_trait]
impl Tool for DataTableCreateTool {
    fn name(&self) -> &str {
        ToolName::DataTableCreate.as_str()
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        let types: Vec<String> = data::FieldType::ALL
            .into_iter()
            .map(data::FieldType::wire_name)
            .collect();
        json!({
            "type": "object",
            "properties": {
                "name": { "type": "string", "description": "テーブル名" },
                "fields": {
                    "type": "array",
                    "description": "列の配列（並びが画面の並び）",
                    "items": {
                        "type": "object",
                        "properties": {
                            "key": { "type": "string", "description": "参照名（英小文字・数字・_・変えられない）" },
                            "display_name": { "type": "string" },
                            "type": { "type": "string", "enum": types },
                            "required": { "type": "boolean" },
                            "unique": { "type": "boolean" },
                            "indexed": { "type": "boolean", "description": "絞り込み・並べ替えに使う" },
                            "searchable": { "type": "boolean", "description": "部分一致で探す（text・multi_select）" },
                            "options": { "type": "array", "items": { "type": "string" } },
                            "ref_table": { "type": "string", "description": "record_ref の参照先テーブルの id" },
                            "description": { "type": "string" }
                        },
                        "required": ["key", "type"]
                    }
                },
                "fsm": { "type": "object", "description": "状態の進め方（説明を参照）" },
                "row_policy": { "type": "object", "description": "行の決まり {read, write?}" },
                "field_policy": { "type": "array", "items": { "type": "object" }, "description": "列の見せ方 [{field, readable_by}]" }
            },
            "required": ["name", "fields"]
        })
    }

    /// テーブルの作成は承認カードを経る（HD-1 の改訂 (c)・オートの自律では自動で承認する）。
    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: CreateInput = match read_input(input) {
            Ok(i) => i,
            Err(msg) => return Ok(ToolOutcome::error(msg)),
        };
        let fields = match parse_fields(input.fields) {
            Ok(f) => f,
            Err(msg) => return Ok(ToolOutcome::error(msg)),
        };
        let name = input.name.trim().to_string();
        let fsm = input.fsm;

        // 1. 本人がこの会話の run の間に作った同名（再生成・再試行で作り直さない・R1）。
        match self.own_same_name(ctx, &name, trace_id).await {
            Ok(Some(table)) => return Ok(on_own_same_name(&table, &fields, fsm.as_ref())),
            Ok(None) => {}
            Err(e) => return Ok(failure(&e)),
        }
        // 2. 上限。
        match self
            .deps
            .data
            .own_tables_created_since(ctx, self.run.since, None, trace_id)
            .await
        {
            Ok(made) if made.len() >= MAX_TABLES_PER_RUN => {
                return Ok(ToolOutcome::error(format!(
                    "この応答ではもう {MAX_TABLES_PER_RUN} つのテーブルを作りました。これ以上は作りません。\
                     作ったテーブルを直すなら data_table_update を使ってください。"
                )));
            }
            Ok(_) => {}
            Err(e) => return Ok(failure(&e)),
        }
        // 3. 作る（スキーマの検証・持ち主・監査は data）。
        let schema = new_schema(fields, input.row_policy, input.field_policy);
        let table = match self
            .deps
            .data
            .create_table(ctx, NewDataTable { name, schema }, trace_id)
            .await
        {
            Ok(t) => t,
            Err(data::DataError::NameTaken) => {
                return Ok(ToolOutcome::error(
                    "この名前のテーブルは作れません（同じ名前のテーブルがあります）。別の名前にしてください。",
                ));
            }
            Err(e) => return Ok(failure(&e)),
        };
        // 4. 状態の進め方。
        let Some(spec) = fsm else {
            return Ok(created(&table, None));
        };
        let save = SaveTableFsm {
            status_field: spec.status_field.clone(),
            body: spec.body(),
            applicant_write_lock: spec.applicant_write_lock.clone(),
            expected_schema_version: table.schema_version,
        };
        match self
            .deps
            .fsms
            .save_for_table(ctx, table.id, &save, trace_id)
            .await
        {
            Ok(saved) => Ok(created(&saved, Some(&spec))),
            Err(e) => Ok(fsm_failed(&table, &e)),
        }
    }
}

/// 本人がこの run の間に同じ名前で作っていたとき（同じ列なら作成済み・違えば直し方）。
fn on_own_same_name(
    table: &DataTable,
    fields: &[data::FieldDef],
    fsm: Option<&FsmSpec>,
) -> ToolOutcome {
    let r = BuilderRef::new(
        BuilderKind::DataTable,
        table.id,
        &table.name,
        table.schema_version,
    );
    let now: Vec<Value> = table.schema.fields.iter().map(field_shape).collect();
    let same_fsm = table.schema.status_field.as_deref() == fsm.map(|f| f.status_field.as_str())
        && table.schema.fsm_ref.is_some() == fsm.is_some();
    if now == expected_shapes(fields, fsm) && same_fsm {
        already_created(&r)
    } else {
        created_differently(
            &r,
            &format!(
                "直すなら、data_table_update に table_id {} と expected_schema_version {} を渡して、足す列・変える列だけを\
                 書いてください。別のテーブルとして作るなら、別の名前にしてください。",
                table.id, table.schema_version
            ),
        )
    }
}

/// テーブルは作れたが状態の進め方を付けられなかったときの観測（誤り・参照の行つき）。
fn fsm_failed(table: &DataTable, err: &data::DataError) -> ToolOutcome {
    let r = BuilderRef::new(
        BuilderKind::DataTable,
        table.id,
        &table.name,
        table.schema_version,
    );
    let reason = failure(err).content;
    let mut outcome = ToolOutcome::error(format!(
        "テーブル「{}」は作りました（id: {}, スキーマ v{}）が、状態の進め方は保存できませんでした: {reason}\n\
         直してから data_table_update に table_id {} と expected_schema_version {} と fsm を渡してください\
         （テーブルを作り直さない）。\n{}",
        shown(&table.name),
        table.id,
        table.schema_version,
        table.id,
        table.schema_version,
        r.line()
    ));
    outcome.builder_refs.push(r.to_value());
    outcome
}
