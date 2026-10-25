//! outbox → イベントトリガ・wait(event) の relay（`mod.rs` から分割・行数規約）。
//!
//! storage.write の 1 件を、トリガの run 入力（`$from trigger`）と wait(event) の到来イベントに
//! 写して配る（engine.md §5.4・§5.5 の同一マッチャ）。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use workflow_engine::{
    EventWaitGate, EventWaitSubscriber, RunStore, RunStoreError, SchedulerStore,
};

/// 1 tick で捌く outbox の最大バッチ数と、1 バッチの件数（claim は配送済みの行を走査するので、1 回の件数を
/// 大きくして回数を減らす・一括取込の 10 万件で 64 件ずつだと追いつくまで 15〜20 分かかった・検証 D-14）。
const RELAY_BATCHES_PER_TICK: usize = 32;
const RELAY_BATCH: usize = 500;

/// relay が使う依存（イベントの照合・起動・閲覧の確認・データ変更の見え方）。
pub struct RelayDeps<'a> {
    pub db: &'a sqlx::PgPool,
    pub hints: &'a storage::event::ClaimHints,
    pub sched: &'a SchedulerStore,
    pub runs: &'a RunStore,
    pub launcher: &'a dyn workflow_engine::RunLauncher,
    pub storage: &'a storage::StorageService,
    /// データ変更トリガ・wait(event) の見え方（委譲元・実行主体の権限で読む・GAP-07）。
    pub data: &'a Arc<data::DataStore>,
    pub fsms: &'a Arc<data::FsmStore>,
}

/// outbox の workflow 未配送イベントを処理する（storage.write とデータ変更・at-least-once）。
///
/// 各イベントを ①トリガマッチ（run 起動）②wait(event) 起床、の両方へ配る（engine.md §5.5 同一マッチャ）。
/// storage.write は祖先束縛 scope（イベント発生フォルダ id）＋filter で照合する。wait(event) はさらに、購読した
/// run の実行主体が保存されたファイルを読めるときだけ起こす（`storage` の viewer 確認・[`SavedFileGate`]）。
/// データ変更（`data.record.changed` / `data.record.transitioned`）は [`super::relay_data`] が配る。
/// 続けて、起動に一時失敗したトリガの発火を再試行する（D-21）。
/// スケジューラの relay ループから呼ぶほか、結合テストが本番と同じ経路でイベントを配るために使う。
pub async fn relay_events(deps: &RelayDeps<'_>) -> Result<(), anyhow::Error> {
    // 溜まっていれば 1 tick の中で続けて捌く（構造化データのレコード変更も outbox に乗るので、一括取込の
    // 直後は件数が多い。1 tick 1 バッチだと他の consumer と GC を長く止める・Task 15.6）。
    for _ in 0..RELAY_BATCHES_PER_TICK {
        if relay_events_batch(deps).await? < RELAY_BATCH {
            break;
        }
    }
    retry_firings(deps).await?;
    Ok(())
}

/// 起動に一時失敗したトリガの発火を再試行し、打ち切ったものを監査に残す（D-21）。
async fn retry_firings(deps: &RelayDeps<'_>) -> Result<(), anyhow::Error> {
    let report = deps
        .sched
        .retry_pending_firings(chrono::Utc::now(), None, deps.launcher)
        .await?;
    for g in &report.gave_up {
        let org: Option<String> = sqlx::query_scalar(
            "SELECT org FROM workflow_registration WHERE tenant_id = $1 AND workflow_id = $2",
        )
        .bind(&g.tenant_id)
        .bind(g.workflow_id)
        .fetch_optional(deps.db)
        .await?;
        audit_suppressed(
            deps.db,
            &g.tenant_id,
            org.as_deref().unwrap_or_default(),
            &g.trigger_id,
            g.event_id,
            "gave_up",
            None,
        )
        .await;
    }
    Ok(())
}

/// トリガの発火を抑止・打ち切ったことを監査に残す（件数ではなく 1 件ずつ・不可視による不発火は残さない）。
///
/// 書込の失敗は警告に留める（relay を止めない）。
pub(super) async fn audit_suppressed(
    db: &sqlx::PgPool,
    tenant_id: &str,
    org: &str,
    trigger_id: &str,
    event_id: i64,
    reason: &str,
    trace_id: Option<&str>,
) {
    let ctx = authz::AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "system:workflow-relay".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant_id.to_string()),
        },
        org.to_string(),
        tenant_id.to_string(),
    );
    let entry = storage::audit::AuditEntry {
        action: storage::audit::AuditAction::WorkflowTriggerSuppressed,
        object_type: "workflow_trigger",
        object_id: trigger_id,
        decision: storage::audit::Decision::Deny,
        trace_id,
        metadata: json!({ "reason": reason, "event_id": event_id }),
    };
    if let Err(e) = storage::audit::AuditRecorder::new(db.clone())
        .record(&ctx, entry)
        .await
    {
        tracing::warn!(error = %e, trigger_id, reason, "トリガの抑止の監査に失敗");
    }
}

async fn relay_events_batch(deps: &RelayDeps<'_>) -> Result<usize, anyhow::Error> {
    let mut tx = deps.db.begin().await?;
    let events = storage::event::claim_undelivered_hinted(
        &mut tx,
        deps.hints,
        "workflow",
        "workflow",
        RELAY_BATCH as i64,
    )
    .await?;
    if events.is_empty() {
        return Ok(0);
    }
    let mut ids = Vec::with_capacity(events.len());
    for ev in &events {
        // ファイルの保存でないイベント（data サービス由来・削除・移動・フォルダの作成など）は
        // 配送済みにしてスキップする（storage.write として誤発火させない）。
        let Some(write) = storage_write_event(ev) else {
            ids.push(ev.id);
            continue;
        };
        // match_event / wake_event_waits は自 tx（trigger_firing UNIQUE・wait_subscription fired）で冪等。
        deps.sched
            .match_event(
                &ev.tenant_id,
                "storage.write",
                ev.id,
                write.folder,
                &write.payload,
                deps.launcher,
            )
            .await?;
        // 確かめられなかった（認可基盤の一時障害など）ときは Err でバッチごと配送済みにせず、次の tick で
        // 配り直す（トリガの発火・起こした wait は自 tx で冪等）。
        let gate = SavedFileGate {
            storage: deps.storage,
            file_id: ev.node_id,
            trace_id: ev.trace_id.as_deref(),
        };
        deps.runs
            .wake_event_waits(
                &ev.tenant_id,
                "storage.write",
                write.folder,
                &write.payload,
                &gate,
            )
            .await?;
        ids.push(ev.id);
    }
    storage::event::mark_delivered(&mut tx, "workflow", &ids).await?;
    tx.commit().await?;
    tracing::debug!(
        count = ids.len(),
        "outbox → workflow event matcher / wait へ relay"
    );
    Ok(events.len())
}

/// wait(event) を起こす前に、購読した run の実行主体が保存されたファイルを読めるか（viewer）を確かめる。
///
/// wait(event) の購読は scope が空でもよく（テナント内のどの保存にも一致する）、購読時に実行主体の権限も
/// 確かめていない。読めないファイルの id・書いた人などを run の実行履歴に出さないよう、起こすたびに
/// StorageService（読取の認可チョークポイント・強整合の OpenFGA check と監査）で確かめる。トリガは
/// 有効化の時点で有効化者がフォルダを読めることを確かめている（`RegistrationService::enable`）ので
/// 対象にしない。
struct SavedFileGate<'a> {
    storage: &'a storage::StorageService,
    file_id: uuid::Uuid,
    trace_id: Option<&'a str>,
}

#[async_trait]
impl EventWaitGate for SavedFileGate<'_> {
    async fn admits(&self, sub: &EventWaitSubscriber) -> Result<bool, RunStoreError> {
        let ctx = super::auth::principal_ctx(
            &sub.tenant_id,
            &sub.org,
            &sub.principal,
            &sub.principal_kind,
        );
        // 監査に「wait(event) の起床の確認」であることを残す（利用者が開いた記録と見分ける）。
        let mut note = serde_json::Map::new();
        note.insert("via".into(), json!("workflow.wait_event"));
        note.insert("run_id".into(), json!(sub.run_id.to_string()));
        note.insert("step".into(), json!(sub.step_path));
        match self
            .storage
            .get_metadata_annotated(&ctx, self.file_id, self.trace_id, &note)
            .await
        {
            Ok(_) => Ok(true),
            // 読めない（存在を明かさない not_found を含む）・保存の後に消えたファイルでは起こさない。
            Err(storage::StorageError::Forbidden | storage::StorageError::NotFound) => Ok(false),
            Err(e) => Err(RunStoreError::Internal(format!(
                "wait(event) の閲覧確認に失敗: {e}"
            ))),
        }
    }
}

/// storage.write として配る 1 件（祖先束縛の照合に使うフォルダと、run 入力になるペイロード）。
#[derive(Debug, PartialEq)]
struct StorageWriteEvent {
    /// イベント発生フォルダ（ファイルの親・ルート直下なら None で、どの folder 束縛にも一致しない）。
    folder: Option<uuid::Uuid>,
    payload: Value,
}

/// outbox の 1 件を storage.write のイベントへ写す（ファイルの保存でなければ None）。
///
/// 配るのはファイルの作成と新しい版の保存（op = create / update）だけで、削除・移動・名前の変更・
/// 復元・フォルダの作成は配らない（トリガは「ファイルが保存されたとき」・BUG-8）。
///
/// 祖先束縛の照合に使うフォルダは outbox の `parent_id`。新しい版の保存のうち、利用者のアップロード
/// （finalize）・内容の更新（content_update）・提案の採用は `parent_id` を載せないので、フォルダを束縛した
/// トリガ・wait には一致しない（載せるのはチャット・エージェントのワークスペースへの上書きだけ）。
/// 上書き保存で起動させるには、自分が書いたファイルで自分を起こし続ける循環の抑止が先に要るので、
/// ここでは広げていない（同じ CSV を上書きする run が、その保存で再び起動する）。
///
/// ペイロード（`$from trigger` と wait(event) の出力）の形は、ワークフローの語彙のカタログが単一定義
/// （[`workflow_engine::vocab::StorageWritePayload`]・`payload_fields(storage.write)`・D-47）。web の参照の
/// 候補と AI の説明文も同じ表から作る。
///
/// ファイル名と content_type は載せない。outbox に無く、載せるにはメタデータの取得と、トリガごとの
/// 実行主体（委譲）でファイルを読めるかの確認が要る。名前などは storage.read（実行主体の ReBAC を通る）
/// の出力から取る。
fn storage_write_event(ev: &storage::event::OutboxEvent) -> Option<StorageWriteEvent> {
    // data サービス由来イベント（FSM 遷移等・payload.event_type 付き）は storage.write ではない。
    // data トリガ（GAP-07）が消費するまではここで捨てる（Task 9.10）。
    if ev
        .payload
        .get("event_type")
        .and_then(Value::as_str)
        .is_some()
    {
        return None;
    }
    if !matches!(ev.op.as_str(), "create" | "update") {
        return None;
    }
    if ev.payload.get("kind").and_then(Value::as_str) != Some("file") {
        return None;
    }
    let parent_id = ev.payload.get("parent_id").cloned().unwrap_or(Value::Null);
    let folder = parent_id
        .as_str()
        .and_then(|s| uuid::Uuid::parse_str(s).ok());
    let field = |k: &str| ev.payload.get(k).cloned().unwrap_or(Value::Null);
    let payload = workflow_engine::vocab::StorageWritePayload {
        file_id: ev.node_id.to_string(),
        version: ev.version,
        op: ev.op.clone(),
        kind: "file".into(),
        parent_id,
        size: field("size"),
        actor: ev.actor.clone(),
        blob_sha256: field("blob_sha256"),
    };
    Some(StorageWriteEvent {
        folder,
        payload: serde_json::to_value(payload).unwrap_or(Value::Null),
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]
    use super::*;

    fn event(op: &str, payload: Value) -> storage::event::OutboxEvent {
        storage::event::OutboxEvent {
            id: 7,
            org: "acme".into(),
            tenant_id: "t1".into(),
            node_id: uuid::Uuid::parse_str("11111111-2222-3333-4444-555555555555").unwrap(),
            version: 3,
            op: op.into(),
            actor: "alice".into(),
            trace_id: None,
            payload,
            created_at: chrono::Utc::now(),
            system: false,
        }
    }

    const FOLDER: &str = "66666666-7777-8888-9999-aaaaaaaaaaaa";

    /// 保存されたファイルの id・版・操作・書込主体が run 入力に載る（`$from trigger /file_id`）。
    #[test]
    fn file_save_carries_file_id_version_op_and_actor() {
        let ev = event(
            "create",
            json!({ "kind": "file", "blob_sha256": "abc", "size": 12, "parent_id": FOLDER }),
        );
        let got = storage_write_event(&ev).expect("ファイルの作成は発火する");
        assert_eq!(got.folder, Some(uuid::Uuid::parse_str(FOLDER).unwrap()));
        assert_eq!(
            got.payload,
            json!({
                "file_id": "11111111-2222-3333-4444-555555555555",
                "version": 3, "op": "create", "kind": "file",
                "parent_id": FOLDER, "size": 12, "actor": "alice", "blob_sha256": "abc",
            })
        );
        // 新しい版の保存も配る（親が載っている経路＝ワークスペースへの上書き）。
        let up = event(
            "update",
            json!({ "kind": "file", "blob_sha256": "def", "size": 3, "parent_id": FOLDER }),
        );
        assert_eq!(storage_write_event(&up).unwrap().payload["op"], "update");
    }

    /// 削除・移動・名前の変更・復元・フォルダの作成・data のイベントは storage.write として配らない。
    #[test]
    fn non_save_events_are_not_relayed() {
        for (op, payload) in [
            ("delete", json!({ "kind": "file", "parent_id": FOLDER })),
            ("move", json!({ "kind": "file", "new_parent": FOLDER })),
            ("rename", json!({ "kind": "file", "renamed": true })),
            ("restore", json!({ "kind": "file", "blob_sha256": "abc" })),
            ("create", json!({ "kind": "folder", "parent_id": FOLDER })),
            ("create", json!({ "parent_id": FOLDER })),
            (
                "update",
                json!({ "event_type": "data.record.changed", "kind": "file", "parent_id": FOLDER }),
            ),
        ] {
            assert_eq!(
                storage_write_event(&event(op, payload.clone())),
                None,
                "{op} {payload}"
            );
        }
    }

    /// 親の無い（ルート直下・親を載せない経路の）保存はフォルダ束縛のトリガに一致しない。
    #[test]
    fn save_without_parent_has_no_folder() {
        let ev = event(
            "update",
            json!({ "kind": "file", "blob_sha256": "x", "size": 1 }),
        );
        let got = storage_write_event(&ev).unwrap();
        assert_eq!(got.folder, None);
        assert_eq!(got.payload["parent_id"], Value::Null);
    }
}
