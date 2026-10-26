//! outbox → イベントトリガ・wait(event) の relay（`mod.rs` から分割・行数規約）。
//!
//! storage.write の 1 件を、トリガの run 入力（`$from trigger`）と wait(event) の到来イベントに
//! 写して配る（engine.md §5.4・§5.5 の同一マッチャ）。データ変更は [`super::relay_data`] が配る。
//!
//! ファイル保存トリガは、①フォルダの祖先束縛で候補を引き ②filter ③workflow プリンシパル（委譲）が
//! 保存されたファイルを読めるか（[`SavedFileGate`]・D-50）④因果の鎖（自分の保存で自分を起こさない・
//! workflow.start の子も同じ・D-26）を通して発火する。新しい版の保存（op = update）は同じファイルの続く
//! 保存を 1 回にまとめてから起動する（`SchedulerStore::defer_event_trigger`・D-38）。まとめて待たせた発火と
//! 起動に一時失敗した発火は、起動する時点にも③を確かめ直す（[`retry_event_firings`]・BUG-50）。

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{json, Value};
use uuid::Uuid;
use workflow_engine::{
    Coalesce, EventTriggerRef, EventWaitGate, EventWaitSubscriber, LaunchOutcome, RunLauncher,
    RunStore, RunStoreError, SchedulerStore, SkipReason,
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
/// storage.write は祖先束縛 scope（イベント発生フォルダ id）＋filter で照合する。トリガは workflow プリンシパル、
/// wait(event) は購読した run の実行主体が、保存されたファイルを読めるときだけ起こす（`storage` の viewer
/// 確認・[`SavedFileGate`]）。
/// データ変更（`data.record.changed` / `data.record.transitioned`）は [`super::relay_data`] が配る。
/// 続けて、起動に一時失敗したトリガの発火を再試行する（D-21）。
/// スケジューラの relay ループから呼ぶほか、結合テストが本番と同じ経路でイベントを配るために使う。
pub async fn relay_events(deps: &RelayDeps<'_>) -> Result<(), anyhow::Error> {
    // 溜まっていれば 1 tick の中で続けて捌く（構造化データのレコード変更も outbox に乗るので、一括取込の
    // 直後は件数が多い。1 tick 1 バッチだと他の consumer と GC を長く止める・Task 15.6）。
    let mut batch_err = None;
    for _ in 0..RELAY_BATCHES_PER_TICK {
        match relay_events_batch(deps).await {
            Ok(n) if n < RELAY_BATCH => break,
            Ok(_) => {}
            Err(e) => {
                batch_err = Some(e);
                break;
            }
        }
    }
    // 配送のバッチが一時障害で止まっても、発火の再試行と承認の突合は回す（先頭のイベントが詰まって、
    // 全テナントの再試行・突合まで止まらないように）。誤りは最初のものを返す。
    let retried = retry_event_firings(deps, chrono::Utc::now(), None).await;
    // 承認の受付の突合（停止の commit より前に動いた行・承認の依頼の監査・GAP-08）。
    let reconciled = super::relay_approval::reconcile_approvals(deps).await;
    // データ変更の wait の受付の突合（停止の時点から購読の commit までに配送された変更・BUG-42）。
    let waits = super::relay_data::reconcile_data_waits(deps).await;
    tick_result(
        batch_err,
        [
            ("トリガの発火の再試行", retried),
            ("承認の受付の突合", reconciled),
            ("データ変更の wait の受付の突合", waits),
        ],
    )
}

/// 1 tick の誤りをまとめる。配送のバッチの誤りを先に、無ければ再試行・突合の順に最初のものを返し、返さない
/// 誤りは警告に残す（バッチが失敗し続けている間に突合も失敗していることを、運用者が見落とさないように）。
fn tick_result<const N: usize>(
    batch: Option<anyhow::Error>,
    rest: [(&str, Result<(), anyhow::Error>); N],
) -> Result<(), anyhow::Error> {
    let mut first = batch;
    for (what, r) in rest {
        let Err(e) = r else { continue };
        if first.is_none() {
            first = Some(e);
        } else {
            tracing::warn!(error = %e, "{what}に失敗（先に起きた誤りを返すので、ここに残す）");
        }
    }
    first.map_or(Ok(()), Err)
}

/// 起動に一時失敗したトリガの発火と、まとめて待たせた発火（D-38）のうち時刻の来たものを起動し、打ち切った
/// ものを監査に残す（D-21）。relay の tick と、結合テストが本番と同じ経路で起こすのに使う。
///
/// ファイル保存トリガの発火は、起動する時点にも workflow プリンシパルが保存されたファイルを読めるかを確かめ
/// 直す（[`FileGatedLauncher`]・BUG-50）。待たせていた間（最長 10 分）・再試行を待つ間に委譲やフォルダの共有が
/// 外れていたら起動しない（読めないファイルの id・書いた人を run の入力に載せない）。
pub async fn retry_event_firings(
    deps: &RelayDeps<'_>,
    now: chrono::DateTime<chrono::Utc>,
    tenant_scope: Option<&str>,
) -> Result<(), anyhow::Error> {
    let launcher = FileGatedLauncher { deps };
    let report = deps
        .sched
        .retry_pending_firings(now, tenant_scope, &launcher)
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

/// 待たせた発火・再試行の起動の前に、ファイル保存トリガなら workflow プリンシパルの閲覧を確かめ直す起動口
/// （relay の起動口を包む・[`retry_event_firings`]・BUG-50）。
///
/// 確かめるのは relay の発火の前と同じ [`SavedFileGate::admits_trigger`]（D-50）。読めなければ起動を見送り
/// （再試行しない・`skipped`）、確かめられない一時障害は起動の一時失敗として再試行に回す。ファイル保存
/// トリガでない発火（データ変更）はそのまま起動する。
struct FileGatedLauncher<'d, 'a> {
    deps: &'d RelayDeps<'a>,
}

impl FileGatedLauncher<'_, '_> {
    /// 起動してよいか（ファイル保存トリガでなければ真）。
    async fn admits(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        trigger_id: &str,
        payload: &Value,
    ) -> Result<bool, anyhow::Error> {
        let trigger: Option<(Option<String>, String)> = sqlx::query_as(
            "SELECT t.source, r.org FROM workflow_trigger t \
             JOIN workflow_registration r \
               ON r.tenant_id = t.tenant_id AND r.workflow_id = t.workflow_id \
             WHERE t.tenant_id = $1 AND t.trigger_id = $2 AND t.workflow_id = $3",
        )
        .bind(tenant_id)
        .bind(trigger_id)
        .bind(workflow_id)
        .fetch_optional(self.deps.db)
        .await?;
        // 消えたトリガは再試行が先に見送る（ここに来ない）。来ても起動口の確認に任せる。
        let Some((source, org)) = trigger else {
            return Ok(true);
        };
        if source.as_deref() != Some("storage.write") {
            return Ok(true);
        }
        // ペイロードにファイルが無ければ確かめようがないので起動しない（fail-closed）。
        let Some(file_id) = payload
            .get("file_id")
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
        else {
            return Ok(false);
        };
        let gate = SavedFileGate {
            storage: self.deps.storage,
            file_id,
            trace_id: None,
        };
        let t = EventTriggerRef {
            tenant_id: tenant_id.to_string(),
            trigger_id: trigger_id.to_string(),
            workflow_id,
        };
        gate.admits_trigger(&t, &org).await
    }
}

#[async_trait]
impl RunLauncher for FileGatedLauncher<'_, '_> {
    async fn launch(
        &self,
        tenant_id: &str,
        workflow_id: Uuid,
        trigger_kind: &str,
        trigger_id: &str,
        payload: &Value,
        event_id: Option<i64>,
    ) -> LaunchOutcome {
        match self
            .admits(tenant_id, workflow_id, trigger_id, payload)
            .await
        {
            Ok(true) => {
                self.deps
                    .launcher
                    .launch(
                        tenant_id,
                        workflow_id,
                        trigger_kind,
                        trigger_id,
                        payload,
                        event_id,
                    )
                    .await
            }
            Ok(false) => LaunchOutcome::Skipped(SkipReason::DelegationInvalid(
                "ワークフローが保存されたファイルを読めなくなりました".into(),
            )),
            Err(e) => LaunchOutcome::Failed(format!("{e}")),
        }
    }
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
    // データ変更（data.record.changed / transitioned）はテーブル単位で候補を引くので、まとめて配る（GAP-07）。
    super::relay_data::relay_data_events(deps, &events).await?;
    // 行の削除で、その行に束縛した保留中の承認を取り下げる（GAP-08）。
    super::relay_approval::withdraw_on_deletes(deps, &events).await?;
    let mut ids = Vec::with_capacity(events.len());
    for ev in &events {
        // ファイルの保存でないイベント（data サービス由来・削除・移動・フォルダの作成など）は
        // 配送済みにしてスキップする（storage.write として誤発火させない）。
        let Some(write) = storage_write_event(ev) else {
            ids.push(ev.id);
            continue;
        };
        // トリガの発火 / wake_event_waits は自 tx（trigger_firing UNIQUE・wait_subscription fired）で冪等。
        // 確かめられなかった（認可基盤の一時障害など）ときは Err でバッチごと配送済みにせず、次の tick で
        // 配り直す（トリガの発火・起こした wait は自 tx で冪等）。
        let gate = SavedFileGate {
            storage: deps.storage,
            file_id: ev.node_id,
            trace_id: ev.trace_id.as_deref(),
        };
        relay_file_triggers(deps, ev, &write, &gate).await?;
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

/// 1 件のファイルの保存を、フォルダに束縛したファイル保存トリガへ配る。
///
/// filter（fail-closed）→ workflow プリンシパルの閲覧（委譲・D-50）→ 発火の順に通す。読めないファイルの
/// id・書いた人を run 入力に載せない（委譲を外した後に保存されたファイルでは起動しない）。
/// 因果の鎖は、イベントを起こした書込の由来の run（outbox の非公開部の印・D-26・BUG-44）から引く。
/// 新しい版の保存（op = update）は同じファイルの続く保存とまとめるために待たせる（D-38）。
async fn relay_file_triggers(
    deps: &RelayDeps<'_>,
    ev: &storage::event::OutboxEvent,
    write: &StorageWriteEvent,
    gate: &SavedFileGate<'_>,
) -> Result<(), anyhow::Error> {
    let candidates = deps
        .sched
        .file_trigger_candidates(&ev.tenant_id, write.folder)
        .await?;
    if candidates.is_empty() {
        return Ok(());
    }
    let cause = super::relay_origin::cause_runs(deps.db, &[ev])
        .await?
        .get(&ev.id)
        .copied()
        .flatten();
    for c in &candidates {
        if !c.admits(&write.payload) {
            continue;
        }
        if !gate.admits_trigger(&c.trigger, &c.org).await? {
            continue;
        }
        let out = if ev.op == "update" {
            let coalesce = Coalesce {
                key: ev.node_id.to_string(),
                occurred_at: ev.created_at,
            };
            deps.sched
                .defer_event_trigger(&c.trigger, ev.id, &write.payload, cause, &coalesce)
                .await?
        } else {
            deps.sched
                .fire_event_trigger(&c.trigger, ev.id, &write.payload, cause, deps.launcher)
                .await?
        };
        if let Some(reason) = out.suppressed_reason() {
            audit_suppressed(
                deps.db,
                &ev.tenant_id,
                &c.org,
                &c.trigger.trigger_id,
                ev.id,
                reason,
                ev.trace_id.as_deref(),
            )
            .await;
        }
    }
    Ok(())
}

/// 起こす前に、起こされる主体が保存されたファイルを読めるか（viewer）を確かめる。
///
/// - wait(event): 購読した run の実行主体（[`EventWaitGate`]）。購読は scope が空でもよく（テナント内の
///   どの保存にも一致する）、購読時に実行主体の権限も確かめていない。
/// - ファイル保存トリガ: workflow プリンシパル（委譲・D-50）。有効化の時点では有効化者がフォルダを読める
///   ことしか確かめておらず（`RegistrationService::enable`）、フォルダを委譲しないまま有効化できる。委譲は
///   再同意で外れうる（委譲元の失権は、起動の時点の委譲の確認 `check_run_start` が止める）。
///
/// 読めないファイルの id・書いた人などを run の入力・実行履歴に出さないよう、起こすたびに StorageService
/// （読取の認可チョークポイント・強整合の OpenFGA check と監査）で確かめる。
struct SavedFileGate<'a> {
    storage: &'a storage::StorageService,
    file_id: uuid::Uuid,
    trace_id: Option<&'a str>,
}

impl SavedFileGate<'_> {
    /// 主体がファイルを読めるか。監査には確認の経路（`note`）を残す（利用者が開いた記録と見分ける）。
    async fn readable(
        &self,
        ctx: &authz::AuthContext,
        note: &serde_json::Map<String, Value>,
    ) -> Result<bool, storage::StorageError> {
        match self
            .storage
            .get_metadata_annotated(ctx, self.file_id, self.trace_id, note)
            .await
        {
            Ok(_) => Ok(true),
            // 読めない（存在を明かさない not_found を含む）・保存の後に消えたファイルでは起こさない。
            Err(storage::StorageError::Forbidden | storage::StorageError::NotFound) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// ファイル保存トリガを発火してよいか（workflow プリンシパルが委譲でファイルを読めるか・D-50）。
    ///
    /// relay の発火の前（[`relay_file_triggers`]）と、待たせた発火・再試行の起動の前（[`FileGatedLauncher`]・
    /// BUG-50）の両方がこれで確かめる。
    async fn admits_trigger(&self, t: &EventTriggerRef, org: &str) -> Result<bool, anyhow::Error> {
        let ctx = authz::AuthContext::for_workflow(
            t.tenant_id.clone(),
            org.to_string(),
            &t.workflow_id.to_string(),
        );
        let mut note = serde_json::Map::new();
        note.insert("via".into(), json!("workflow.trigger"));
        note.insert("trigger_id".into(), json!(t.trigger_id));
        self.readable(&ctx, &note)
            .await
            .map_err(|e| anyhow::anyhow!("ファイル保存トリガの閲覧確認に失敗: {e}"))
    }
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
        let mut note = serde_json::Map::new();
        note.insert("via".into(), json!("workflow.wait_event"));
        note.insert("run_id".into(), json!(sub.run_id.to_string()));
        note.insert("step".into(), json!(sub.step_path));
        self.readable(&ctx, &note)
            .await
            .map_err(|e| RunStoreError::Internal(format!("wait(event) の閲覧確認に失敗: {e}")))
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
/// 祖先束縛の照合に使うフォルダは outbox の `parent_id`（保存した時点の親フォルダ）。作成と、新しい版の
/// 保存（利用者のアップロード（finalize）・内容の更新（content_update・Office とノートの保存・csv.patch）・
/// 提案の採用・ワークスペースへの上書き）のどれも載せる（BUG-28）。
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

    /// 警告を集める書き出し先（`tracing` の出力を確かめる）。
    #[derive(Clone, Default)]
    struct Captured(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for Captured {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// 配送のバッチが失敗したときも、再試行・突合の誤りを捨てずに警告に残す（返すのはバッチの誤り）。
    #[test]
    fn tick_errors_that_are_not_returned_are_logged() {
        let out = Captured::default();
        let sink = out.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(move || sink.clone())
            .with_ansi(false)
            .finish();
        let r = tracing::subscriber::with_default(subscriber, || {
            tick_result(
                Some(anyhow::anyhow!("batch: openfga down")),
                [
                    ("retry", Err(anyhow::anyhow!("retry: db down"))),
                    ("reconcile", Err(anyhow::anyhow!("reconcile: db down"))),
                ],
            )
        });
        assert_eq!(r.unwrap_err().to_string(), "batch: openfga down");
        let logged = String::from_utf8(out.0.lock().unwrap().clone()).unwrap();
        assert!(logged.contains("retry: db down"), "{logged}");
        assert!(logged.contains("reconcile: db down"), "{logged}");
        // バッチが通ったら、再試行・突合の最初の誤りを返し、残りを警告に残す。
        let r = tick_result(
            None,
            [
                ("retry", Ok(())),
                ("reconcile", Err(anyhow::anyhow!("reconcile: db down"))),
            ],
        );
        assert_eq!(r.unwrap_err().to_string(), "reconcile: db down");
        assert!(tick_result(None, [("retry", Ok(())), ("reconcile", Ok(()))]).is_ok());
    }

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
        // 新しい版の保存も配る（アップロード・内容の更新・提案の採用・上書きのどれも親を載せる・BUG-28）。
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

    /// 親の無い（ルート直下の）保存はフォルダ束縛のトリガに一致しない。
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
