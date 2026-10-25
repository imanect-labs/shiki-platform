//! outbox → イベントトリガ・wait(event) の relay（`mod.rs` から分割・行数規約）。
//!
//! storage.write の 1 件を、トリガの run 入力（`$from trigger`）と wait(event) の到来イベントに
//! 写して配る（engine.md §5.4・§5.5 の同一マッチャ）。

use serde_json::{json, Value};
use workflow_engine::{RunStore, SchedulerStore};

/// 1 tick で捌く outbox の最大バッチ数と、1 バッチの件数（claim は配送済みの行を走査するので、1 回の件数を
/// 大きくして回数を減らす・一括取込の 10 万件で 64 件ずつだと追いつくまで 15〜20 分かかった・検証 D-14）。
const RELAY_BATCHES_PER_TICK: usize = 32;
const RELAY_BATCH: usize = 500;

/// outbox の workflow 未配送イベントを処理する（storage.write のみ・at-least-once）。
///
/// 各イベントを ①トリガマッチ（run 起動）②wait(event) 起床、の両方へ配る（engine.md §5.5 同一マッチャ）。
/// いずれも祖先束縛 scope（イベント発生フォルダ id）＋filter で照合する。スケジューラの tick から呼ぶ
/// ほか、結合テストが本番と同じ経路でイベントを配るために使う。
pub async fn relay_events(
    db: &sqlx::PgPool,
    hints: &storage::event::ClaimHints,
    sched: &SchedulerStore,
    runs: &RunStore,
    launcher: &dyn workflow_engine::RunLauncher,
) -> Result<(), anyhow::Error> {
    // 溜まっていれば 1 tick の中で続けて捌く（構造化データのレコード変更も outbox に乗るので、一括取込の
    // 直後は件数が多い。1 tick 1 バッチだと他の consumer と GC を長く止める・Task 15.6）。
    for _ in 0..RELAY_BATCHES_PER_TICK {
        if relay_events_batch(db, hints, sched, runs, launcher).await? < RELAY_BATCH {
            break;
        }
    }
    Ok(())
}

async fn relay_events_batch(
    db: &sqlx::PgPool,
    hints: &storage::event::ClaimHints,
    sched: &SchedulerStore,
    runs: &RunStore,
    launcher: &dyn workflow_engine::RunLauncher,
) -> Result<usize, anyhow::Error> {
    let mut tx = db.begin().await?;
    let events = storage::event::claim_undelivered_hinted(
        &mut tx,
        hints,
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
        sched
            .match_event(
                &ev.tenant_id,
                "storage.write",
                ev.id,
                write.folder,
                &write.payload,
                launcher,
            )
            .await?;
        runs.wake_event_waits(&ev.tenant_id, "storage.write", write.folder, &write.payload)
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

/// storage.write として配る 1 件（祖先束縛の照合に使うフォルダと、run 入力になるペイロード）。
#[derive(Debug, PartialEq)]
struct StorageWriteEvent {
    /// イベント発生フォルダ（ファイルの親・ルート直下なら None で、どの folder 束縛にも一致しない）。
    folder: Option<uuid::Uuid>,
    payload: Value,
}

/// outbox の 1 件を storage.write のイベントへ写す（ファイルの保存でなければ None）。
///
/// 発火するのはファイルの作成と新しい版の保存（op = create / update）だけで、削除・移動・名前の変更・
/// 復元・フォルダの作成では発火しない（トリガは「ファイルが保存されたとき」・BUG-8）。
///
/// ペイロード（`$from trigger` と wait(event) の出力）の形:
///
/// ```jsonc
/// { "file_id": "<uuid>", "version": 3, "op": "create",   // create | update
///   "kind": "file", "parent_id": "<uuid>", "size": 1234,
///   "actor": "<書込主体の id>",                          // ワークフロー経由の書込はワークフロー id
///   "blob_sha256": "…" }                                 // 従来から載っていたキー（互換のため残す）
/// ```
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
    Some(StorageWriteEvent {
        folder,
        payload: json!({
            "file_id": ev.node_id.to_string(),
            "version": ev.version,
            "op": ev.op,
            "kind": "file",
            "parent_id": parent_id,
            "size": field("size"),
            "actor": ev.actor,
            "blob_sha256": field("blob_sha256"),
        }),
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
        // 新しい版の保存も発火する（親が載っている経路）。
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
