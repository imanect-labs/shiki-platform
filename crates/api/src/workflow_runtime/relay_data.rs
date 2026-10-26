//! データ変更（outbox の `data.record.changed` / `data.record.transitioned`）→ データ変更トリガ・
//! wait(event) の relay（GAP-07・engine.md §5.4・§5.5）。
//!
//! 1. バッチの中のテーブルの集合で、候補のトリガと wait をテナントごとに 1 回ずつ引く（候補の無いイベントは
//!    data を読まずに配送済みにする）。
//! 2. トリガは一括取込（CSV の取込・一括 upsert）の行では発火しない（`bulk_flags`・D-26）。wait は起こす。
//! 3. 変更の見え方を、トリガは**委譲元**（テーブルの viewer をワークフローへ委譲した人）、wait は run の
//!    **実行主体**で作る（[`DataPort::actor`] → data の `change_event_view`）。見えない行・読めない列だけの
//!    変更では発火も起床もせず、記録もしない。委譲の有無は先に DB で確かめ、無ければ OpenFGA を呼ばない。
//! 4. filter（fail-closed）→ トリガは因果の鎖・queued の上限を通して発火（[`SchedulerStore::fire_event_trigger`]）。
//!
//! 主体と見え方は**このバッチの中だけ**で覚える（バッチを跨いで持たない・PIT-18）。
//! 権限の判定（Forbidden / NotFound）は「発火しない」として扱い、一時的な障害だけを `Err` で返す
//! （呼び出し側はバッチを配送済みにせず次の tick で配り直す）。

use std::collections::{BTreeMap, BTreeSet, HashMap};

use authz::{AuthContext, Relation};
use data::{ChangeEventKind, ChangeEventRef, ChangeEventView, DataError};
use serde_json::Value;
use uuid::Uuid;
use workflow_engine::vocab::{DataChangePayload, EventSource};
use workflow_engine::{DataTriggerCandidate, DataWaitCandidate, ExecCtx, FireOutcome};

use super::data_port::{Actor, DataPort};
use super::relay::{audit_suppressed, RelayDeps};

/// outbox の 1 件を写したデータ変更（候補の照合の材料）。
struct DataEvent<'a> {
    event: &'a storage::event::OutboxEvent,
    source: EventSource,
    kind: ChangeEventKind,
    table_id: Uuid,
    record_id: Uuid,
    rev: i64,
}

impl DataEvent<'_> {
    fn change_ref(&self) -> ChangeEventRef {
        ChangeEventRef {
            table_id: self.table_id,
            record_id: self.record_id,
            rev: self.rev,
            kind: self.kind,
        }
    }
}

/// outbox の 1 件をデータ変更へ写す（作成・値の更新・FSM 遷移だけ。削除・派生値・スキーマ・取り直しは配らない）。
fn data_event(ev: &storage::event::OutboxEvent) -> Option<DataEvent<'_>> {
    let p = &ev.payload;
    let id = |k: &str| {
        p.get(k)
            .and_then(Value::as_str)
            .and_then(|s| Uuid::parse_str(s).ok())
    };
    let (source, kind, rev) = match p.get("event_type").and_then(Value::as_str)? {
        data::RECORD_CHANGED_EVENT => {
            let rev = p.get("rev").and_then(Value::as_i64)?;
            match p.get("change").and_then(Value::as_str)? {
                "create" => (EventSource::DataRecordCreated, ChangeEventKind::Create, rev),
                "update" => (EventSource::DataRecordUpdated, ChangeEventKind::Update, rev),
                _ => return None,
            }
        }
        // 遷移の rev は outbox の version（公開部に rev を載せていない）。
        data::TRANSITION_EVENT_TYPE => (
            EventSource::DataTransition,
            ChangeEventKind::Transition,
            ev.version,
        ),
        _ => return None,
    };
    Some(DataEvent {
        event: ev,
        source,
        kind,
        table_id: id("table_id")?,
        record_id: id("record_id")?,
        rev,
    })
}

/// 見え方を作る主体（トリガは workflow 主体＝委譲元で、wait は run の実行主体）。
#[derive(Clone, PartialEq, Eq, Hash)]
struct PrincipalKey {
    kind: String,
    id: String,
    org: String,
}

/// 1 バッチの中だけで使う主体・見え方の覚え（PIT-18: バッチを跨いで持たない）。
struct BatchCtx<'d, 'a> {
    deps: &'d RelayDeps<'a>,
    tenant_id: String,
    /// (主体, テーブル) → 主体（委譲が無い・権限が無いなら `None`）。
    actors: HashMap<(PrincipalKey, Uuid), Option<Actor>>,
    /// (主体, outbox の id) → 見え方（見えなければ `None`）。
    views: HashMap<(PrincipalKey, i64), Option<ChangeEventView>>,
    /// outbox の id → 変更が run の書込によるものか。
    by_workflow: HashMap<i64, bool>,
}

impl<'d, 'a> BatchCtx<'d, 'a> {
    fn new(deps: &'d RelayDeps<'a>, tenant_id: &str) -> Self {
        BatchCtx {
            deps,
            tenant_id: tenant_id.to_string(),
            actors: HashMap::new(),
            views: HashMap::new(),
            by_workflow: HashMap::new(),
        }
    }

    /// 主体の見え方で作った変更（見えない・権限が無ければ `None`・一時障害は `Err`）。
    ///
    /// 先に [`Self::prefetch`] で同じテーブルの変更をまとめて作っておけば、ここは覚えから返す。
    async fn view(
        &mut self,
        who: &PrincipalKey,
        ev: &DataEvent<'_>,
    ) -> Result<Option<ChangeEventView>, anyhow::Error> {
        if let Some(v) = self.views.get(&(who.clone(), ev.event.id)) {
            return Ok(v.clone());
        }
        self.prefetch(who, ev.table_id, &[ev]).await?;
        Ok(self
            .views
            .get(&(who.clone(), ev.event.id))
            .cloned()
            .flatten())
    }

    /// 同じテーブルの変更の見え方をまとめて作って覚える（テーブルの読込・認可の問い・マスクの判定を 1 回にする）。
    async fn prefetch(
        &mut self,
        who: &PrincipalKey,
        table_id: Uuid,
        evs: &[&DataEvent<'_>],
    ) -> Result<(), anyhow::Error> {
        let missing: Vec<&DataEvent<'_>> = evs
            .iter()
            .copied()
            .filter(|e| {
                e.table_id == table_id && !self.views.contains_key(&(who.clone(), e.event.id))
            })
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        let akey = (who.clone(), table_id);
        if !self.actors.contains_key(&akey) {
            let actor = self.actor(who, table_id).await?;
            self.actors.insert(akey.clone(), actor);
        }
        let views: Vec<Option<ChangeEventView>> = match self
            .actors
            .get(&akey)
            .and_then(Option::as_ref)
        {
            None => vec![None; missing.len()],
            Some(a) => {
                let refs: Vec<ChangeEventRef> = missing.iter().map(|e| e.change_ref()).collect();
                match a
                    .data
                    .change_event_views(&a.ctx, table_id, &refs, None)
                    .await
                {
                    Ok(v) => v,
                    Err(DataError::NotFound | DataError::Forbidden) => vec![None; missing.len()],
                    // 配り直せば直りうる誤りか（data の一時障害の判定・[`DataError::is_transient`]）。
                    Err(e) if e.is_transient() => {
                        return Err(anyhow::anyhow!("データ変更の見え方の作成に失敗: {e}"))
                    }
                    // 確定的な誤り（列・述語の評価など）は配り直しても直らない。1 件が全テナントの配送を
                    // 止めないよう、警告のうえ「見えない」（発火も起床もしない・fail-closed）として先へ進める。
                    Err(e) => {
                        tracing::warn!(error = %e, %table_id, "データ変更の見え方を作れないので配らない");
                        vec![None; missing.len()]
                    }
                }
            }
        };
        for (e, v) in missing.iter().zip(views) {
            self.views.insert((who.clone(), e.event.id), v);
        }
        Ok(())
    }

    /// 主体（[`DataPort::actor`]）。ワークフロー主体は、委譲台帳で委譲元を先に確かめ（無ければ OpenFGA を
    /// 呼ばない・剥奪の後に Deny の監査を溢れさせない）、委譲元の権限と天井で読む。
    async fn actor(
        &self,
        who: &PrincipalKey,
        table_id: Uuid,
    ) -> Result<Option<Actor>, anyhow::Error> {
        if who.kind == "workflow" {
            let Ok(workflow_id) = Uuid::parse_str(&who.id) else {
                return Ok(None);
            };
            let ctx = AuthContext::for_workflow(self.tenant_id.clone(), who.org.clone(), &who.id);
            let object = ctx.ns().data_table(&table_id.to_string());
            let delegator = workflow_engine::active_delegator(
                self.deps.db,
                &self.tenant_id,
                workflow_id,
                &object,
                Relation::Viewer,
            )
            .await?;
            if delegator.is_none() {
                return Ok(None);
            }
        }
        let ec = ExecCtx {
            tenant_id: self.tenant_id.clone(),
            org: who.org.clone(),
            principal: who.id.clone(),
            principal_kind: who.kind.clone(),
            trace_id: None,
            run_id: None,
            step_path: None,
        };
        let port = DataPort {
            data: self.deps.data.as_ref(),
            fsms: self.deps.fsms.as_ref(),
            db: self.deps.db,
        };
        match port.actor(&ec, table_id, Relation::Viewer).await {
            Ok(a) => Ok(Some(a)),
            // 権限が無い・委譲が無いのは「発火しない」。一時的な障害だけ配り直す。
            Err(e) if !e.retryable => Ok(None),
            Err(e) => Err(anyhow::anyhow!("{}: {}", e.code, e.message)),
        }
    }

    /// 主体がテーブルを読めない（委譲・権限が無い）と分かったか（[`Self::prefetch`] の後だけ真になりうる）。
    fn actor_unavailable(&self, who: &PrincipalKey, table_id: Uuid) -> bool {
        matches!(self.actors.get(&(who.clone(), table_id)), Some(None))
    }

    /// 変更が run の書込によるものか（書込の trace_id が run のもの・run id 自体は載せない）。
    async fn by_workflow(&mut self, ev: &DataEvent<'_>) -> Result<bool, anyhow::Error> {
        if let Some(b) = self.by_workflow.get(&ev.event.id) {
            return Ok(*b);
        }
        let b = match workflow_engine::scheduler::cause_run_of_trace(ev.event.trace_id.as_deref()) {
            None => false,
            Some(run_id) => sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM workflow_run WHERE tenant_id = $1 AND run_id = $2)",
            )
            .bind(&self.tenant_id)
            .bind(run_id)
            .fetch_one(self.deps.db)
            .await?,
        };
        self.by_workflow.insert(ev.event.id, b);
        Ok(b)
    }

    /// run 入力・wait の出力になるペイロード（形はワークフローの語彙のカタログが正・D-47）。
    async fn payload(
        &mut self,
        source: EventSource,
        ev: &DataEvent<'_>,
        view: ChangeEventView,
    ) -> Result<Value, anyhow::Error> {
        let by_workflow = self.by_workflow(ev).await?;
        Ok(DataChangePayload {
            source: source.as_str().to_string(),
            table_id: view.table_id.to_string(),
            record_id: view.record_id.to_string(),
            rev: view.rev,
            change: view.change,
            from: view.from,
            to: view.to,
            changed: view.changed,
            changes: view.changes,
            actor: view.actor,
            by_workflow,
            occurred_at: view.occurred_at.to_rfc3339(),
            record: serde_json::to_value(&view.record).ok(),
            truncated: false,
        }
        .into_value())
    }
}

/// バッチの中のデータ変更をトリガと wait(event) へ配る（`relay_events_batch` から呼ぶ）。
pub(super) async fn relay_data_events(
    deps: &RelayDeps<'_>,
    events: &[storage::event::OutboxEvent],
) -> Result<(), anyhow::Error> {
    let mut by_tenant: BTreeMap<&str, Vec<DataEvent<'_>>> = BTreeMap::new();
    for ev in events {
        if let Some(d) = data_event(ev) {
            by_tenant.entry(ev.tenant_id.as_str()).or_default().push(d);
        }
    }
    for (tenant_id, evs) in by_tenant {
        let tables: Vec<Uuid> = evs
            .iter()
            .map(|e| e.table_id)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let triggers = deps
            .sched
            .data_trigger_candidates(tenant_id, &tables)
            .await?;
        let waits = deps.runs.data_wait_candidates(tenant_id, &tables).await?;
        if triggers.is_empty() && waits.is_empty() {
            continue;
        }
        let mut cx = BatchCtx::new(deps, tenant_id);
        relay_tenant_triggers(&mut cx, &evs, &triggers).await?;
        relay_tenant_waits(&mut cx, &evs, &waits).await?;
    }
    Ok(())
}

/// 1 テナントのトリガの発火（一括取込の行は発火しない）。
async fn relay_tenant_triggers(
    cx: &mut BatchCtx<'_, '_>,
    evs: &[DataEvent<'_>],
    triggers: &[DataTriggerCandidate],
) -> Result<(), anyhow::Error> {
    if triggers.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = evs
        .iter()
        .filter(|e| triggers.iter().any(|t| t.table_id == e.table_id))
        .map(|e| e.event.id)
        .collect();
    let bulk = cx.deps.data.bulk_flags(&cx.tenant_id, &ids).await?;
    let fires = |ev: &DataEvent<'_>| !bulk.get(&ev.event.id).copied().unwrap_or(false);
    // トリガごとに、そのテーブルの変更の見え方をまとめて作る（変更ごとに認可を引き直さない）。
    for t in triggers {
        let who = PrincipalKey {
            kind: "workflow".into(),
            id: t.trigger.workflow_id.to_string(),
            org: t.org.clone(),
        };
        let mine: Vec<&DataEvent<'_>> = evs
            .iter()
            .filter(|e| e.table_id == t.table_id && e.source == t.source && fires(e))
            .collect();
        cx.prefetch(&who, t.table_id, &mine).await?;
    }
    for ev in evs {
        if !fires(ev) {
            continue;
        }
        for t in triggers
            .iter()
            .filter(|t| t.table_id == ev.table_id && t.source == ev.source)
        {
            let who = PrincipalKey {
                kind: "workflow".into(),
                id: t.trigger.workflow_id.to_string(),
                org: t.org.clone(),
            };
            let Some(view) = cx.view(&who, ev).await? else {
                continue;
            };
            let payload = cx.payload(t.source, ev, view).await?;
            if !t.admits(&payload) {
                continue;
            }
            let cause =
                workflow_engine::scheduler::cause_run_of_trace(ev.event.trace_id.as_deref());
            let out = cx
                .deps
                .sched
                .fire_event_trigger(&t.trigger, ev.event.id, &payload, cause, cx.deps.launcher)
                .await?;
            if let Some(reason) = out.suppressed_reason() {
                audit_suppressed(
                    cx.deps.db,
                    &cx.tenant_id,
                    &t.org,
                    &t.trigger.trigger_id,
                    ev.event.id,
                    reason,
                    ev.event.trace_id.as_deref(),
                )
                .await;
            }
            if let FireOutcome::Fired(run_id) = out {
                tracing::debug!(%run_id, trigger = %t.trigger.trigger_id, "データ変更トリガで run を起動");
            }
        }
    }
    Ok(())
}

/// 1 テナントの wait(event) の起床（一括取込の行でも起こす・自分の書込でも起こす）。
async fn relay_tenant_waits(
    cx: &mut BatchCtx<'_, '_>,
    evs: &[DataEvent<'_>],
    waits: &[DataWaitCandidate],
) -> Result<(), anyhow::Error> {
    let who_of = |w: &DataWaitCandidate| PrincipalKey {
        kind: w.subscriber.principal_kind.clone(),
        id: w.subscriber.principal.clone(),
        org: w.subscriber.org.clone(),
    };
    for w in waits {
        let mine: Vec<&DataEvent<'_>> = evs
            .iter()
            .filter(|e| w.wants(e.source, e.table_id, e.record_id, e.rev, e.event.id))
            .collect();
        cx.prefetch(&who_of(w), w.table_id, &mine).await?;
    }
    // 起こした wait は、同じバッチの後のイベントでは見ない（一括取込の続く行で同じ wait を何度も評価しない）。
    let mut woken: BTreeSet<(Uuid, &str)> = BTreeSet::new();
    for ev in evs {
        for w in waits
            .iter()
            .filter(|w| w.wants(ev.source, ev.table_id, ev.record_id, ev.rev, ev.event.id))
        {
            let key = (w.subscriber.run_id, w.subscriber.step_path.as_str());
            if woken.contains(&key) {
                continue;
            }
            let Some(view) = cx.view(&who_of(w), ev).await? else {
                // 承認を待つ run の実行主体が、停止の後に委譲・権限を失った: 判断の遷移は二度と見えないので、
                // 取り下げて理由を残す（行は承認へ進んだのに run が期限まで待つ食い違いを作らない・GAP-08）。
                if let Some(a) = w.approval() {
                    if cx.actor_unavailable(&who_of(w), w.table_id) {
                        super::relay_approval::resolve(
                            cx.deps,
                            &w.subscriber,
                            a,
                            &workflow_engine::ApprovalResolution::withdrawn(),
                            Some(super::relay_approval::ACTOR_UNAVAILABLE),
                        )
                        .await?;
                        woken.insert(key);
                    }
                }
                continue;
            };
            let payload = cx.payload(w.source, ev, view).await?;
            if !w.admits(&payload) {
                continue;
            }
            let woke = cx.deps.runs.wake_data_wait(w, &payload).await?;
            if let (true, Some(a)) = (woke, w.approval()) {
                // 承認の決着（判断の遷移・判断の外の遷移による取り下げ）を監査に残す（GAP-08）。
                let status = workflow_engine::ApprovalResolution::transition(
                    &a.choices,
                    payload
                        .get("to")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                    None,
                    None,
                    None,
                )
                .status;
                super::relay_approval::audit_resolved(
                    cx.deps.db,
                    &workflow_engine::ResolvedApproval {
                        tenant_id: w.subscriber.tenant_id.clone(),
                        org: w.subscriber.org.clone(),
                        approval_id: a.approval_id,
                        run_id: w.subscriber.run_id,
                        status: status.to_string(),
                    },
                )
                .await;
            }
            woken.insert(key);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn outbox(op: &str, version: i64, payload: Value) -> storage::event::OutboxEvent {
        storage::event::OutboxEvent {
            id: 9,
            org: "acme".into(),
            tenant_id: "t1".into(),
            node_id: Uuid::nil(),
            version,
            op: op.into(),
            actor: "alice".into(),
            trace_id: None,
            payload,
            created_at: chrono::Utc::now(),
            system: false,
        }
    }

    const TABLE: &str = "66666666-7777-8888-9999-aaaaaaaaaaaa";
    const ROW: &str = "11111111-2222-3333-4444-555555555555";

    /// 作成・更新・遷移だけをデータ変更として配る（削除・派生値・スキーマ・ファイルの保存は配らない）。
    #[test]
    fn only_creates_updates_and_transitions_are_data_events() {
        let changed = |change: &str| {
            json!({ "event_type": "data.record.changed", "table_id": TABLE, "record_id": ROW,
                    "rev": 4, "change": change })
        };
        let created = outbox("create", 1, changed("create"));
        let d = data_event(&created).unwrap();
        assert_eq!(
            (d.source, d.kind, d.rev),
            (EventSource::DataRecordCreated, ChangeEventKind::Create, 4)
        );
        let updated = outbox("update", 4, changed("update"));
        assert_eq!(
            data_event(&updated).unwrap().source,
            EventSource::DataRecordUpdated
        );
        let moved = outbox(
            "update",
            5,
            json!({ "event_type": "data.record.transitioned", "table_id": TABLE, "record_id": ROW,
                    "from": "draft", "to": "submitted" }),
        );
        let d = data_event(&moved).unwrap();
        assert_eq!(
            (d.source, d.rev),
            (EventSource::DataTransition, 5),
            "遷移の rev は version"
        );
        for change in ["delete", "derived", "schema", "refresh"] {
            assert!(
                data_event(&outbox("update", 4, changed(change))).is_none(),
                "{change}"
            );
        }
        let file = outbox("create", 1, json!({ "kind": "file", "parent_id": TABLE }));
        assert!(data_event(&file).is_none());
    }
}
