//! テーブルの「状態の進め方」の読み書き（持ち主だけ・GAP-44・D-84）。
//!
//! 状態の進め方は `artifact(kind=fsm)` に版を積み、テーブルは使う版を `fsm_ref` で指す（[`crate::FsmStore`]）。
//! ここはそれをテーブルの側から 1 つの操作にまとめる（テーブルの設定の画面が使う）。
//!
//! - **読む**（[`FsmStore::table_fsm`]）: テーブルの持ち主に、指している版の定義を**テーブル側の参照として**
//!   読ませる（定義そのものの閲覧の権限は求めない・遷移と同じ `get_version_pinned`）。進められる人の条件は
//!   行の決まりと同じく持ち主だけが読む（テーブル定義の応答も、持ち主以外には行の決まりを返さない）。
//! - **保存する**（[`FsmStore::save_for_table`]）: 状態の列の選択肢を状態の一覧に合わせ、定義に版を積み
//!   （書けない定義・消えた定義なら新しく作る）、テーブルが指す版と書き込みの決まり（[`crate::fsm_write_guard`]）を
//!   スキーマの改訂 1 回で替える。改訂は通常の入口（[`DataStore::update_table_schema`]）を通るので、楽観ロック・
//!   使われている選択肢の削除の拒否・索引・監査・購読者への通知はスキーマの改訂と同じ。
//!
//! 版を積んだ後でスキーマの改訂が失敗すると、どこからも指されない版が残る（定義の履歴が 1 つ増えるだけで、
//! 動きには効かない）。

use authz::{AuthContext, Relation};
use serde::Serialize;
use storage::audit::AuditAction;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::fsm::{FsmBody, FsmRef};
use crate::fsm_store::{map_artifact, FsmStore};
use crate::model::{DataTable, FieldType};
use crate::{DataError, InvalidCode};

/// テーブルが指している状態の進め方の版。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct PinnedFsm {
    pub artifact_id: Uuid,
    pub version: i64,
    pub body: FsmBody,
}

/// テーブルの状態の進め方（持ち主が「状態の進め方」の画面で読む形）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TableFsm {
    /// 状態の列の参照名（無ければ null）。
    pub status_field: Option<String>,
    /// テーブルが指している状態の進め方（無ければ null）。
    pub fsm: Option<PinnedFsm>,
    /// 「最初の状態を過ぎたら、申請者はレコードを書き換えられない」決まりで使っている申請者の列（オフなら null）。
    pub applicant_write_lock: Option<String>,
    /// いまのスキーマの版（保存の `expected_schema_version` に渡す）。
    pub schema_version: i64,
}

/// テーブルの状態の進め方の保存（API の要求から組む）。
#[derive(Debug, Clone)]
pub struct SaveTableFsm {
    /// 状態の列（選択肢の列）。状態の進め方を作った後は変えられない。
    pub status_field: String,
    /// 状態の一覧（先頭が最初の状態）と進め方。状態の列の選択肢は、この一覧（この順）になる。
    pub body: FsmBody,
    /// 「最初の状態を過ぎたら、申請者はレコードを書き換えられない」決まりの申請者の列（メンバーの列）。
    /// `None` なら、この決まりを外す。
    pub applicant_write_lock: Option<String>,
    /// 読んだときのスキーマの版（違えば 409）。
    pub expected_schema_version: i64,
}

/// 書き込みの決まりを付けると、だれも書き換え・削除できなくなるレコードの数（[`FsmStore::records_locked_by`]）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LockedRecords {
    /// 数（上限 [`LOCK_COUNT_CAP`] で打ち切る）。
    pub count: i64,
    /// 上限を超えた（「{上限} 件以上」と言う）。
    pub more: bool,
}

impl LockedRecords {
    /// 数の言い方（「1,234 件」・上限を超えたら「10,000 件以上」）。
    #[must_use]
    pub fn amount(&self) -> String {
        let n = crate::schema_data_check::group_digits(self.count);
        if self.more {
            format!("{n} 件以上")
        } else {
            format!("{n} 件")
        }
    }
}

/// [`LockedRecords`] の数の上限（これを超えたら「以上」）。
pub const LOCK_COUNT_CAP: i64 = 10_000;

fn invalid(field: Option<&str>, m: &str) -> DataError {
    DataError::input_detail(InvalidCode::FsmInvalid, field, m)
}

impl FsmStore {
    /// テーブルの状態の進め方（持ち主だけ・見えないテーブルは 404・持ち主でなければ 403）。
    pub async fn table_fsm(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<TableFsm, DataError> {
        let table = self
            .owned_table(ctx, table_id, AuditAction::DataTableGet, trace_id)
            .await?;
        let fsm = self.pinned_of(ctx, &table, trace_id).await?;
        let schema = &table.schema;
        let applicant_write_lock = schema.status_field.as_deref().and_then(|s| {
            crate::fsm_write_guard::applicant_write_lock(schema.row_policy.as_ref(), s)
        });
        Ok(TableFsm {
            status_field: schema.status_field.clone(),
            fsm,
            applicant_write_lock,
            schema_version: table.schema_version,
        })
    }

    /// テーブルの状態の進め方を保存する（持ち主だけ・改訂後のテーブルを返す）。
    pub async fn save_for_table(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        req: &SaveTableFsm,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        let table = self
            .owned_table(ctx, table_id, AuditAction::DataTableUpdateSchema, trace_id)
            .await?;
        if table.schema_version != req.expected_schema_version {
            return Err(DataError::Conflict(format!(
                "schema_version が一致しません（現在 {}）",
                table.schema_version
            )));
        }
        let status = req.status_field.as_str();
        match table.schema.field(status) {
            Some(f) if f.field_type == FieldType::Select => {}
            _ => {
                return Err(invalid(
                    Some(status),
                    "状態の列には、選択肢の列を選んでください。",
                ))
            }
        }
        if table.schema.fsm_ref.is_some() && table.schema.status_field.as_deref() != Some(status) {
            return Err(invalid(
                Some(status),
                "状態の進め方を作った後は、状態の列を変えられません。",
            ));
        }
        let initial = req
            .body
            .states
            .first()
            .ok_or_else(|| invalid(None, "状態を 1 つ以上決めてください。"))?;
        let mut next = table.schema.clone();
        next.status_field = Some(status.to_string());
        if let Some(f) = next.fields.iter_mut().find(|f| f.key == status) {
            f.options.clone_from(&req.body.states);
        }
        next.row_policy = match req.applicant_write_lock.as_deref() {
            Some(applicant) => {
                match next.field(applicant) {
                    Some(f) if f.field_type == FieldType::UserRef => {}
                    _ => {
                        return Err(invalid(
                            Some(applicant),
                            "申請者の列には、メンバーの列を選んでください。",
                        ))
                    }
                }
                Some(crate::fsm_write_guard::with_guard(
                    next.row_policy.as_ref(),
                    status,
                    initial,
                    applicant,
                ))
            }
            None => crate::fsm_write_guard::without_guard(next.row_policy.as_ref(), status),
        };
        // 行の決まりが使う列（状態の列・申請者の列）は索引が要る（data-platform §6.3）。
        crate::policy::validate::promote_policy_fields(&mut next);
        // 定義を書く前に確かめる（書いてから断ると、指されない版が増える）。
        crate::fsm::validate_fsm(&req.body, &next)?;
        let pinned = self
            .append_or_create(ctx, &table, &req.body, trace_id)
            .await?;
        next.fsm_ref = Some(pinned);
        self.data
            .update_table_schema(
                ctx,
                table_id,
                next,
                Some(req.expected_schema_version),
                trace_id,
            )
            .await
    }

    /// 保存で書き込みの決まり（[`crate::fsm_write_guard`]）を付ける・付け直すと、だれも書き換え・削除できなく
    /// なるレコードの数（認可は呼び出し側が持ち主で済ませる・数えるのはテーブルの全行）。
    ///
    /// 値なしの `ne` は偽なので、状態が最初の状態でなく申請者の列が空のレコードは、決まりを付けると持ち主・
    /// 編集者・保存済みのワークフローのどれからも書き換えられない（途中から申請者の列を足したテーブルで起きる）。
    /// 決まりを外す保存・同じ最初の状態と申請者の列のまま保存し直す保存は、効きが変わらないので数えない（`None`）。
    /// 列が無いなど保存が断る要求も数えない（保存が理由を言う）。
    pub async fn records_locked_by(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        req: &SaveTableFsm,
    ) -> Result<Option<LockedRecords>, DataError> {
        let (Some(applicant), Some(initial)) =
            (req.applicant_write_lock.as_deref(), req.body.states.first())
        else {
            return Ok(None);
        };
        let table = self.data.fetch_live(ctx, table_id).await?;
        let schema = &table.schema;
        let status = req.status_field.as_str();
        let current = crate::fsm_write_guard::current_guard(schema.row_policy.as_ref(), status);
        if current.is_some_and(|(i, a)| i == *initial && a == applicant) {
            return Ok(None);
        }
        let (Some(sf), Some(af)) = (schema.field(status), schema.field(applicant)) else {
            return Ok(None);
        };
        // 値はすべてバインド（SQL へ埋め込むのは固定の条件文だけ・PIT-21）。空は schema_data_check と同じ
        // （キーが無い・null・空の配列）。
        let n: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM (SELECT 1 FROM data_record \
             WHERE tenant_int = $1 AND table_int = $2 \
               AND NOT (jsonb_typeof(data -> $3) = 'string' AND data ->> $3 = $5) \
               AND (NOT (data ? $4) OR data -> $4 = 'null'::jsonb OR data -> $4 = '[]'::jsonb) \
             LIMIT $6) s",
        )
        .bind(table.keys.tenant_int)
        .bind(table.keys.table_int)
        .bind(sf.json_key())
        .bind(af.json_key())
        .bind(initial)
        .bind(LOCK_COUNT_CAP + 1)
        .fetch_one(self.data.pools.for_tenant(&ctx.tenant_id))
        .await
        .map_err(crate::map_db)?;
        Ok((n > 0).then_some(LockedRecords {
            count: n.min(LOCK_COUNT_CAP),
            more: n > LOCK_COUNT_CAP,
        }))
    }

    /// テーブルの持ち主を確かめて、生きているテーブルを返す。
    async fn owned_table(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        action: AuditAction,
        trace_id: Option<&str>,
    ) -> Result<DataTable, DataError> {
        self.data
            .require(ctx, table_id, Relation::Owner, action, trace_id)
            .await?;
        self.data.fetch_live(ctx, table_id).await
    }

    /// テーブルが指している版（テーブル側の参照として読む・無ければ `None`）。
    async fn pinned_of(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        trace_id: Option<&str>,
    ) -> Result<Option<PinnedFsm>, DataError> {
        let Some(r) = &table.schema.fsm_ref else {
            return Ok(None);
        };
        let v = match self
            .artifacts
            .get_version_pinned(
                ctx,
                r.artifact_id,
                r.version,
                artifact::ArtifactKind::Fsm,
                &format!("data_table:{}", table.id),
                trace_id,
            )
            .await
        {
            Ok(v) => v,
            // 指している定義が消えた（定義を直接消した）: 無いものとして返し、作り直せるようにする（保存は新しく作る）。
            Err(artifact::ArtifactError::NotFound) => return Ok(None),
            Err(e) => return Err(map_artifact(e)),
        };
        let body: FsmBody = serde_json::from_value(v.body)
            .map_err(|e| DataError::Internal(format!("fsm body 破損: {e}")))?;
        Ok(Some(PinnedFsm {
            artifact_id: r.artifact_id,
            version: r.version,
            body,
        }))
    }

    /// 指している定義に版を積む。書けない（定義の編集者でない）か消えた定義なら、新しく作る（作った人が持ち主）。
    async fn append_or_create(
        &self,
        ctx: &AuthContext,
        table: &DataTable,
        body: &FsmBody,
        trace_id: Option<&str>,
    ) -> Result<FsmRef, DataError> {
        let raw = serde_json::to_value(body)
            .map_err(|e| DataError::Internal(format!("fsm body: {e}")))?;
        if let Some(r) = &table.schema.fsm_ref {
            match self
                .artifacts
                .append_version(ctx, r.artifact_id, raw.clone(), None, trace_id)
                .await
            {
                Ok(v) => {
                    return Ok(FsmRef {
                        artifact_id: r.artifact_id,
                        version: v.version,
                    })
                }
                Err(artifact::ArtifactError::Forbidden | artifact::ArtifactError::NotFound) => {}
                Err(e) => return Err(map_artifact(e)),
            }
        }
        // 名前は種類ごとに一意（同じテーブルで作り直しても重ならないよう短い乱数を足す・名前は画面に出さない）。
        let suffix = Uuid::new_v4().simple().to_string();
        let a = self
            .artifacts
            .create(
                ctx,
                artifact::NewArtifact {
                    kind: artifact::ArtifactKind::Fsm,
                    name: format!("状態の進め方-{}-{}", table.id, &suffix[..8]),
                    body: raw,
                },
                trace_id,
            )
            .await
            .map_err(map_artifact)?;
        Ok(FsmRef {
            artifact_id: a.id,
            version: a.current_version,
        })
    }
}
