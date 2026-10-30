//! テーブルの閲覧者文脈（Task 15.5・グリッドとビュー編集の表示制御）。
//!
//! 画面が「どの列が自分に読めないか」「編集・スキーマ変更ができるか」を知るための入口。
//! 判定は読取・書込の各 API と同じ（マスクは [`DataStore::masked_field_keys`]・権限は OpenFGA の
//! relation）で、**表示の出し分けのためだけ**に使う（実際の読み書きは各 API が改めて認可する）。
//! 認可材料はこの呼び出しの中だけで使い捨てる（PIT-18）。

use authz::{AuthContext, Relation};
use serde::Serialize;
use storage::audit::AuditAction;
use utoipa::ToSchema;
use uuid::Uuid;

use crate::store::DataStore;
use crate::DataError;

/// テーブルに対する閲覧者本人の文脈。
#[allow(clippy::struct_excessive_bools)]
// 画面の出し分けの独立した可否（編集・追加・追加だけ・スキーマ）で、状態機械ではない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, ToSchema)]
pub struct TableViewerContext {
    /// 閲覧者本人に**読めない**列の参照名（フィールドマスク・ビューの列設定に出せない）。
    ///
    /// **スキーマ管理者（`can_manage`）にだけ**返す（定義の全体を既に知っている人）。それ以外の閲覧者には
    /// 常に空（読めない列の名前・有無・件数を返さない・検証 2-B-P2-1）。スキーマ管理者以外のテーブル定義は
    /// 読める列だけなので（[`DataStore::get_table_for_viewer`]）、画面の出し分けはそれで足りる。
    pub masked_fields: Vec<String>,
    /// レコードを編集できる（テーブルの editor 以上）。行述語の書込条件は別途各行で効く。
    pub can_edit: bool,
    /// レコードを追加できる（テーブルの editor 以上か、追加だけ・D-113）。
    pub can_add: bool,
    /// 追加だけで共有されている（自分が追加したレコードだけ見られる・編集・取込・状態を進める・保存ビューは
    /// 使えない・D-113）。
    pub add_only: bool,
    /// スキーマを変えられる（テーブルの owner）。
    pub can_manage: bool,
    /// 1 ビューで表示できるルックアップ・ロールアップ列の上限（テナント設定）。
    pub max_view_lookup_columns: u32,
    /// 状態の進め方のあるテーブルで、レコードを追加したときの状態（最初の状態）。状態の列が読めないか、
    /// 状態の進め方が無ければ null。定義はテーブル側の参照として読む（定義の閲覧の権限は求めない・BUG-154）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_state: Option<String>,
}

impl DataStore {
    /// テーブルの閲覧者文脈（viewer か追加だけ）。
    pub async fn table_viewer_context(
        &self,
        ctx: &AuthContext,
        table_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<TableViewerContext, DataError> {
        let az = self.authz_memo();
        // 追加だけの人（D-113）も開ける（画面は追加の操作と自分の行だけを出す）。
        let table = self
            .open_for_reader(ctx, &az, table_id, AuditAction::DataTableGet, trace_id)
            .await?;
        let add_only = table.narrowed.is_some();
        let obj = ctx.ns().data_table(&table_id.to_string());
        let rel = az
            .check_all(
                ctx,
                &[
                    (Relation::Editor, obj.clone()),
                    (Relation::Owner, obj.clone()),
                    (Relation::Submitter, obj),
                ],
            )
            .await?;
        let can_manage = rel.get(1).copied().unwrap_or(false);
        // 読めない列は 1 回だけ数える（スキーマ管理者に返すのと、最初の状態を出すかの判定に使う）。どちらも要らない
        // ときは数えない。
        let has_fsm = table.schema.status_field.is_some() && table.schema.fsm_ref.is_some();
        let unreadable = if can_manage || has_fsm {
            self.masked_fields(ctx, &az, &table).await?
        } else {
            std::collections::HashSet::new()
        };
        let initial_state = self
            .initial_state_for(ctx, &table, &unreadable, trace_id)
            .await;
        let mut masked: Vec<String> = if can_manage {
            unreadable.into_iter().collect()
        } else {
            Vec::new()
        };
        masked.sort();
        Ok(TableViewerContext {
            masked_fields: masked,
            can_edit: rel.first().copied().unwrap_or(false),
            can_add: rel.get(2).copied().unwrap_or(false),
            add_only,
            can_manage,
            max_view_lookup_columns: self.view_lookup_cap(ctx).await?,
            initial_state,
        })
    }

    /// 閲覧者に見せる最初の状態（状態の列が読めて、指している状態の進め方が読めるときだけ）。
    ///
    /// 状態の名前は状態の列の選択肢として閲覧者に見えているので、どれが最初かを足すだけ。定義が消えていれば
    /// 出さない（追加は失敗するので、画面は名前を出さずに追加させる）。最初の状態は補助の表示なので、定義を
    /// 読めない誤り（一時的な障害・監査の書き込みの失敗など）でも閲覧者の文脈は返す（出さずに tracing に残す）。
    /// 状態の列の選択肢の先頭は使わない（選択肢と状態は同じ集合だが、同じ順とは限らない・`fsm_pin`）。
    async fn initial_state_for(
        &self,
        ctx: &AuthContext,
        table: &crate::model::DataTable,
        unreadable: &std::collections::HashSet<String>,
        trace_id: Option<&str>,
    ) -> Option<String> {
        let status = table.schema.status_field.as_deref()?;
        if table.schema.fsm_ref.is_none() || unreadable.contains(status) {
            return None;
        }
        match self.pinned_fsm(ctx, table, trace_id).await {
            Ok(fsm) => fsm.and_then(|(body, _)| body.states.into_iter().next()),
            Err(DataError::NotFound) => None,
            Err(e) => {
                tracing::warn!(error = %e, table_id = %table.id, "閲覧者の文脈: 最初の状態を読めなかった（出さずに返す）");
                None
            }
        }
    }
}
