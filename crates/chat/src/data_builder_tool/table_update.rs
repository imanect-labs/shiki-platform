//! `data_table_update`: 自分が持ち主のテーブルを直す（承認カードを経る・足すだけ・D-10 の T05）。
//!
//! - **id だけで直す。** `table_id` に名前が来たら直さず、解決した id とスキーマの版を示して呼び直してもらう
//!   （承認カードで見たテーブルと直すテーブルを一致させる・試運転の `workflow_id` と同じ・TOCTOU）。
//! - **列は足す列と変える列だけ**を受け、今の列に参照名で重ねる（[`super::spec::merge_fields`]）。消す・型を変える・
//!   使われている選択肢を外す・値の無い行がある列を必須にする、は data が理由つきで断る（足すだけ）。
//! - **行の決まりを入れ替えても、書き込みの決まり（最初の状態を過ぎたら申請者は書き換えられない・D-115）は残す**
//!   （[`data::carry_write_guard`]）。決まりを外すのは状態の進め方（`fsm.applicant_write_lock` を省く）で行う。
//! - **状態の進め方を変える前に、影響を確かめる**（テーブルの設定の画面と同じ・GAP-44）。承認を待っているレコードが
//!   判断できなくなる・書き換えられなくなるレコードが出るなら、保存せずに理由を返す（AI はその確認を代われない。
//!   人が画面で数を見て決める）。確かめはスキーマを直す前に行う（途中まで直したままにしない）。
//! - 楽観ロック: `expected_schema_version`（data_schema・前の観測の版）が今の版と違えば直さない。

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use async_trait::async_trait;
use authz::AuthContext;
use data::{DataTable, SaveTableFsm};
use serde_json::{json, Value};
use uuid::Uuid;

use super::spec::{merge_fields, read_input, FsmSpec, UpdateInput, PREDICATE_NOTE};
use super::{failure, resolve_table, shown, DataBuilderDeps};
use crate::builder_obs::{BuilderKind, BuilderRef};

/// テーブルを直す道具。
pub struct DataTableUpdateTool {
    deps: DataBuilderDeps,
    description: String,
}

impl DataTableUpdateTool {
    #[must_use]
    pub fn new(deps: DataBuilderDeps) -> Self {
        DataTableUpdateTool {
            deps,
            description: description(),
        }
    }

    /// 状態の進め方を変えて困ることが起きるなら、その理由（保存しない）。
    async fn fsm_blocker(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        save: &SaveTableFsm,
    ) -> Result<Option<String>, data::DataError> {
        let pending = self
            .deps
            .approvals
            .pending_moves(ctx.tenant_id(), table.id)
            .await
            .map_err(|e| data::DataError::Internal(format!("pending approvals: {e}")))?;
        let lost: i64 = pending
            .iter()
            .filter(|m| save.body.transition(&m.from, &m.to).is_none())
            .map(|m| m.count)
            .sum();
        if lost > 0 {
            return Ok(Some(format!(
                "承認を待っているレコードが {lost} 件あり、この進め方では判断できなくなります。進め方を残すか、\
                 承認が終わってからにしてください（どうしても変えるなら、ユーザーにテーブルの設定の画面で保存してもらう）。"
            )));
        }
        if let Some(locked) = self
            .deps
            .fsms
            .records_locked_by(ctx, table.id, save)
            .await?
        {
            return Ok(Some(format!(
                "申請者の列が空で、最初の状態を過ぎたレコードが {}あります。この決まりを付けると、それらは誰も書き換えたり\
                 削除したりできなくなります。applicant_write_lock を省くか、ユーザーにテーブルの設定の画面で確かめて\
                 保存してもらってください。",
                locked.amount()
            )));
        }
        Ok(None)
    }
}

fn description() -> String {
    format!(
        "自分が作った（持ち主の）テーブルを直す。実行の前にユーザーが承認カードで変更を確かめる。直せるのは足す変更だけ\
         （列を消す・型を変える・使われている選択肢を外すことはできない）。\n\
         \n\
         入力:\n\
         - table_id: テーブルの id（名前ではなく id。data_table_create の「参照:」の行や data_schema で分かる）\n\
         - expected_schema_version: 読んだときのスキーマの版（参照の行の version）。違えば直さない\n\
         - fields（省略可）: 足す列と変える列だけ。今の列に key で重なる（書かなかった列は今のまま・書いた項目だけ変わる）。\
         形は data_table_create と同じ\n\
         - row_policy（省略可）: 行の決まりを入れ替える {{read, write?}}（最初の状態を過ぎたら申請者は書き換えられない決まりは残る）\n\
         - field_policy（省略可）: 列の見せ方を入れ替える [{{field, readable_by}}]\n\
         - fsm（省略可）: 状態の進め方を入れ替える（形は data_table_create と同じ・申請者の決まりを外すなら applicant_write_lock を省く）\n\
         \n\
         {PREDICATE_NOTE}\n\
         - 承認を待っているレコードが判断できなくなる変更・書き換えられなくなるレコードが出る変更は保存しない（理由を返す）。\n\
         - 成功したら末尾の「参照:」の行に新しい版が出る。続けて直すときはその version を渡す。"
    )
}

/// 直したテーブルの観測（変えたこと・参照の行）。
fn updated(table: &DataTable, changes: &[String]) -> ToolOutcome {
    let r = BuilderRef::new(
        BuilderKind::DataTable,
        table.id,
        &table.name,
        table.schema_version,
    );
    let mut outcome = ToolOutcome::ok(format!(
        "テーブル「{}」を直しました（id: {}, スキーマ v{}）。\n変えたこと: {}\n{}",
        shown(&table.name),
        table.id,
        table.schema_version,
        changes.join(" / "),
        r.line()
    ));
    outcome.builder_refs.push(r.to_value());
    outcome
}

#[async_trait]
impl Tool for DataTableUpdateTool {
    fn name(&self) -> &str {
        ToolName::DataTableUpdate.as_str()
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "table_id": { "type": "string", "description": "テーブルの id（UUID）" },
                "expected_schema_version": { "type": "integer", "description": "読んだときのスキーマの版" },
                "fields": {
                    "type": "array",
                    "items": { "type": "object" },
                    "description": "足す列と変える列だけ（key で今の列に重なる）"
                },
                "fsm": { "type": "object", "description": "状態の進め方を入れ替える" },
                "row_policy": { "type": "object", "description": "行の決まりを入れ替える {read, write?}" },
                "field_policy": { "type": "array", "items": { "type": "object" }, "description": "列の見せ方を入れ替える" }
            },
            "required": ["table_id", "expected_schema_version"]
        })
    }

    /// テーブルの改訂は承認カードを経る（オートでも承認を残す・行の決まりは権限の意味を変える）。
    fn requires_confirmation(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input: UpdateInput = match read_input(input) {
            Ok(i) => i,
            Err(msg) => return Ok(ToolOutcome::error(msg)),
        };
        // 名前なら直さず、id を示して呼び直してもらう（承認したテーブルと直すテーブルを一致させる）。
        let Ok(table_id) = Uuid::parse_str(input.table_id.trim()) else {
            return Ok(
                match resolve_table(&self.deps.data, ctx, &input.table_id, trace_id).await {
                    Ok(t) => ToolOutcome::error(format!(
                        "table_id にはテーブルの id を入れてください。「{}」の id は {}（スキーマ v{}）です。\
                         直す内容はそのままで、table_id に id を入れて呼び直してください。",
                        shown(&t.name),
                        t.id,
                        t.schema_version
                    )),
                    Err(o) => o,
                },
            );
        };
        if input.fields.is_none()
            && input.fsm.is_none()
            && input.row_policy.is_none()
            && input.field_policy.is_none()
        {
            return Ok(ToolOutcome::error(
                "直す内容がありません。fields・row_policy・field_policy・fsm のどれかを書いてください。",
            ));
        }
        let table = match self.deps.data.get_table(ctx, table_id, trace_id).await {
            Ok(t) => t,
            Err(e) => return Ok(failure(&e)),
        };
        if table.schema_version != input.expected_schema_version {
            return Ok(ToolOutcome::error(format!(
                "テーブル「{}」のスキーマは v{} です（渡した版は v{}）。ほかの変更が入っています。data_schema で\
                 今の列を読み直してから、expected_schema_version に {} を渡して直してください。",
                shown(&table.name),
                table.schema_version,
                input.expected_schema_version,
                table.schema_version
            )));
        }
        let mut changes = Vec::new();
        let mut next = table.schema.clone();
        if let Some(raw) = input.fields {
            match merge_fields(&table.schema.fields, raw) {
                Ok(m) => {
                    if !m.added.is_empty() {
                        changes.push(format!("列を足した: {}", m.added.join(", ")));
                    }
                    if !m.changed.is_empty() {
                        changes.push(format!("列を変えた: {}", m.changed.join(", ")));
                    }
                    next.fields = m.fields;
                }
                Err(msg) => return Ok(ToolOutcome::error(msg)),
            }
        }
        if let Some(policy) = input.row_policy {
            next.row_policy = Some(policy);
            data::carry_write_guard(&table.schema, &mut next);
            changes.push("行の決まりを入れ替えた".into());
        }
        if let Some(fp) = input.field_policy {
            next.field_policy = fp;
            changes.push("列の見せ方を入れ替えた".into());
        }
        let save = input.fsm.as_ref().map(|spec| fsm_save(spec, &table));
        // 状態の進め方の影響は、スキーマを直す前に確かめる（途中まで直したままにしない）。
        if let Some(save) = &save {
            match self.fsm_blocker(ctx, &table, save).await {
                Ok(Some(reason)) => return Ok(ToolOutcome::error(reason)),
                Ok(None) => {}
                Err(e) => return Ok(failure(&e)),
            }
        }
        let mut current = table.clone();
        if next != table.schema {
            current = match self
                .deps
                .data
                .update_table_schema(ctx, table_id, next, Some(table.schema_version), trace_id)
                .await
            {
                Ok(t) => t,
                Err(e) => return Ok(failure(&e)),
            };
        }
        if let Some(mut save) = save {
            save.expected_schema_version = current.schema_version;
            match self
                .deps
                .fsms
                .save_for_table(ctx, table_id, &save, trace_id)
                .await
            {
                Ok(t) => {
                    current = t;
                    changes.push("状態の進め方を保存した".into());
                }
                Err(e) if current.schema_version != table.schema_version => {
                    // 列などは直した後で、状態の進め方だけ断られた（直した版を示して続きを頼む）。
                    let mut o = updated(&current, &changes);
                    o.is_error = true;
                    o.content = format!(
                        "{}\nただし状態の進め方は保存できませんでした: {}（expected_schema_version {} で fsm だけを渡し直す）",
                        o.content,
                        failure(&e).content,
                        current.schema_version
                    );
                    return Ok(o);
                }
                Err(e) => return Ok(failure(&e)),
            }
        }
        if changes.is_empty() {
            changes.push("変更なし（今の定義と同じ）".into());
        }
        Ok(updated(&current, &changes))
    }
}

/// 状態の進め方の保存（版は直した後の版で上書きする）。
fn fsm_save(spec: &FsmSpec, table: &DataTable) -> SaveTableFsm {
    SaveTableFsm {
        status_field: spec.status_field.clone(),
        body: spec.body(),
        applicant_write_lock: spec.applicant_write_lock.clone(),
        expected_schema_version: table.schema_version,
    }
}
