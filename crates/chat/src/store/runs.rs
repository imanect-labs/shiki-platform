//! `ChatStore`: 接続非依存生成の generation_run / generation_event 操作（Task 3.11）。
//!
//! 整合性の不変条件（design §4.4.1）:
//! - **Transactional Outbox**: [`post_message`](ChatStore::post_message) は message 保存＋run 行＋
//!   jobq enqueue を**単一 Postgres TX**で行う（202 で run_id を即返す・同期実行しない）。
//! - **Idempotent Consumer ＋ Lease/Fencing**: [`claim_run`](ChatStore::claim_run) は queued か
//!   リース失効 running を claim し `fencing_token` を +1。以降の追記/確定は fencing 一致時のみ
//!   通す（クラッシュ takeover ＋ゾンビ書込拒否）。
//! - **Append-only Event Log**: [`append_stream_event`](ChatStore::append_stream_event) は
//!   `(run_id, seq)` 単調 seq を真実のソースへ追記（exactly-once）し、Redis へ best-effort publish。
//!
//! claim/リース/fencing/追記のプリミティブは `crates/durable`（Task 10.0 で共通化）に
//! 委譲する。SQL 意味は #82 の先行実装と同値であり、キュー・レーン・状態機械（queued/
//! running/done/…の語彙）はチャット所有のまま（engine.md §1.2 の分担表）。

#[allow(clippy::wildcard_imports)]
use super::*;

use authz::{AuthContext, Relation};
use durable::{EventTableSpec, Key, KeyValue, RunTableSpec};
use sqlx::types::Json;
use uuid::Uuid;

use crate::model::{ContentBlock, RunStatus, SkillPin, StreamEvent, StreamEventKind};

/// チャット生成ジョブのキュー名（jobq・専用レーン）。
pub const CHAT_GENERATION_QUEUE: &str = "chat_generation";

/// `generation_run` の durable テーブル記述子（migrations/0012_chat.sql の列に対応）。
pub(super) const RUN_SPEC: RunTableSpec = RunTableSpec {
    event_seq_column: Some("event_seq"),
    table: "generation_run",
    status_column: "status",
    fencing_column: "fencing_token",
    lease_column: "lease_until",
    worker_column: "worker_id",
    attempt_column: Some("attempt"),
    updated_at_column: Some("updated_at"),
    queued_status: "queued",
    running_status: "running",
    // 承認待ち中にワーカーが死んでも、リース失効後に別ワーカーが checkpoint から resume できる
    // ようにする（#351）。これが無いと承認待ちで孤児化した run が恒久的に停止する。
    resumable_statuses: &["waiting_approval"],
};

/// `generation_event` の durable テーブル記述子。
pub(super) const EVENT_SPEC: EventTableSpec = EventTableSpec {
    table: "generation_event",
    seq_column: "seq",
    kind_column: "type",
    payload_column: "payload",
    // イベント行の tenant_id は run 行から写す（複合 FK (tenant_id, run_id) の子・#815）。
    copied_run_columns: &["tenant_id"],
};

/// run 行のキーカラム（chat は run_id 単独キー。workflow は tenant 複合キーで同じ形に乗る）。
pub(super) const RUN_KEY_COLUMNS: &[&str] = &["run_id"];

/// イベントを**読む**ときのキー（tenant 込み・#815）。`generation_event` は `(tenant_id, run_id)` の
/// 複合 FK の子で、読み手は run_id だけでなく自テナントでも絞る（別経路から呼ばれても越境しない）。
pub(super) const EVENT_READ_KEY_COLUMNS: &[&str] = &["tenant_id", "run_id"];

/// ワーカーが claim した run（生成に必要な材料一式）。
// generation_run の行の射影（DB の boolean 列をそのまま写す）。状態機械に畳むと
// 列との対応が崩れるので bool のまま持つ。
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ClaimedRun {
    pub run_id: Uuid,
    pub thread_id: Uuid,
    pub message_id: Uuid,
    pub tenant_id: String,
    pub org: String,
    /// 発話ユーザーの subject local id（この本人の権限で生成する）。
    pub actor: String,
    pub agent_mode: bool,
    pub fencing_token: i64,
    pub cancel_requested: bool,
    /// 生成の trace_id（Langfuse/OTel/監査の相関・Task 5.9）。
    pub trace_id: Option<String>,
    /// 自律プロファイル（長ホライズン・フルツール・予算・計画・承認・Task 5.1）。
    pub autonomous: bool,
    /// メッセージ投入時点の承認モードのスナップショット（#350・実効判定は autonomous.rs）。
    pub autonomous_mode: String,
    /// 再開用チェックポイント（ステップ境界で保存・takeover/リトライ時に resume へ渡す・#351）。
    pub checkpoint: Option<Json<serde_json::Value>>,
    /// 適用する skill のバージョンピン（複数可・順序付き・post 時点の thread ピンの
    /// jsonb スナップショット・#344。run 行が生成材料の単一ソース）。
    pub skill_pins: Json<Vec<SkillPin>>,
    /// ミニアプリ経由のセッション（Task 6.10・skill はバンドル権限で読む）。
    pub mini_app_id: Option<Uuid>,
    pub mini_app_version: Option<i64>,
    /// 投入時に解決したモデル（#553）。`None` はカラム追加前の run（従来の既定で生成）。
    pub model: Option<String>,
    /// 投入時に解決した思考強度（`low` / `medium` / `high`）。
    pub effort: Option<String>,
    /// ユーザーがモデルを選ばなかった（Auto）か。Auto のときだけ skill のピンが上書きできる。
    pub model_auto: bool,
    /// ユーザーが**この発話にだけ**使ってほしいと指定したツール名（#734・0071）。
    ///
    /// 語彙（[`agent_core::ToolName`]）の型ではなく文字列で持つ: 後のリリースで名前が消えても
    /// claim の行デコードを失敗させない（照合は [`Self::requested_tool_names`] で行う）。
    pub requested_tools: Json<Vec<String>>,
    /// run の作成時刻（system プロンプトの「今日の日付」の基準・再開しても変わらない・#546）。
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl ClaimedRun {
    /// 指定ツールを語彙で照合して返す（投入順・重複なし）。語彙に無い名前は落とす（fail-closed）。
    #[must_use]
    pub fn requested_tool_names(&self) -> Vec<agent_core::ToolName> {
        known_tool_names(self.run_id, &self.requested_tools.0)
    }
}

/// run 行の `requested_tools`（文字列）を語彙で照合する（投入順・重複なし・未知名は落とす）。
fn known_tool_names(run_id: Uuid, raw: &[String]) -> Vec<agent_core::ToolName> {
    let mut out: Vec<agent_core::ToolName> = Vec::new();
    for name in raw {
        match agent_core::ToolName::parse(name) {
            Some(t) if !out.contains(&t) => out.push(t),
            Some(_) => {}
            None => tracing::warn!(%run_id, tool = %name,
                "run の requested_tools に語彙に無いツール名（無視する）"),
        }
    }
    out
}

/// カードを出した run の生成材料（`chat.submit` の続きの run へ継ぐ）。
#[derive(Debug, Clone, Default)]
pub struct RunContext {
    pub autonomous: bool,
    pub skill_pins: Vec<SkillPin>,
    /// その run のモデル指定（ユーザーが選んだモデルを次のターンでも使う）。
    pub model: Option<crate::model::RunModel>,
    /// その run の発話ごとのツール指定（#734）。カード回答は「カードを出した run の続き」なので
    /// 継ぐ（実行前フェーズの門で承認まで保留したツールを、実行段階で効かせるため）。
    pub requested_tools: Vec<agent_core::ToolName>,
}

/// いま UI が映すべき run（[`ChatStore::current_run`]）。
#[derive(Debug, Clone, Copy)]
pub struct CurrentRun {
    pub run_id: Uuid,
    /// 生成先の assistant メッセージ id（この run が本文に書く図のボタンの照合先）。
    pub message_id: Uuid,
    pub status: RunStatus,
    /// 自律プロファイルか（再訪時の UI 復元・#350）。
    pub autonomous: bool,
}

// `post_message`（Transactional Outbox）は [`super::post`] に分離（行数規約）。

impl ChatStore {
    /// run を claim する（queued かリース失効 running を running へ・fencing_token +1）。
    ///
    /// 既に done/cancelled、または有効リースを他ワーカーが保持中なら `None`。
    pub async fn claim_run(
        &self,
        run_id: Uuid,
        worker_id: &str,
        lease_secs: i64,
    ) -> Result<Option<ClaimedRun>, ChatError> {
        let kv = [KeyValue::Uuid(run_id)];
        durable::claim(
            &self.db,
            &RUN_SPEC,
            &Key::new(RUN_KEY_COLUMNS, &kv),
            worker_id,
            lease_secs,
            "run_id, thread_id, message_id, tenant_id, org, actor, agent_mode, \
             fencing_token, cancel_requested, trace_id, autonomous, autonomous_mode, checkpoint, \
             skill_pins, mini_app_id, mini_app_version, model, effort, model_auto, \
             requested_tools, created_at",
        )
        .await
        .map_err(map_db)
    }

    /// リースを延長し、最新の cancel_requested を返す（ハートビート・ゾンビは fencing で弾く）。
    /// 戻り値 `None` = fencing 不一致 or 非 running（リースを失った＝停止すべき）。
    pub async fn heartbeat(
        &self,
        run_id: Uuid,
        fencing_token: i64,
        lease_secs: i64,
    ) -> Result<Option<bool>, ChatError> {
        // durable::heartbeat は status='running' 限定だが、**承認待ち（waiting_approval）中も
        // リースを延長し続ける**必要がある（承認ブロック中にリース失効→誤キャンセルを防ぐ・Task 5.6）。
        // よって chat 専用 SQL で running / waiting_approval の両方を受ける。fencing でゾンビは弾く。
        let cancel: Option<bool> = sqlx::query_scalar(
            "UPDATE generation_run \
                SET lease_until = now() + ($3 || ' seconds')::interval, updated_at = now() \
             WHERE run_id = $1 AND fencing_token = $2 \
                AND status IN ('running', 'waiting_approval') \
             RETURNING cancel_requested",
        )
        .bind(run_id)
        .bind(fencing_token)
        .bind(lease_secs)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(cancel)
    }

    /// 生成イベントを append-only で追記する（単調 seq・exactly-once）＋Redis publish。
    ///
    /// **fencing 一致（＝現リース保持ワーカー）時のみ**追記する（ゾンビ書込拒否）。fencing 不一致
    /// は seq を返さず `None`（呼び出し側はリース喪失として停止する）。
    pub async fn append_stream_event(
        &self,
        run_id: Uuid,
        fencing_token: i64,
        event: &StreamEventKind,
    ) -> Result<Option<i64>, ChatError> {
        let payload = serde_json::to_value(event)
            .map_err(|e| ChatError::Internal(format!("event serialize: {e}")))?;
        let kv = [KeyValue::Uuid(run_id)];
        let seq = durable::append_event(
            &self.db,
            &RUN_SPEC,
            &EVENT_SPEC,
            &Key::new(RUN_KEY_COLUMNS, &kv),
            event.tag(),
            &payload,
            fencing_token,
        )
        .await
        .map_err(map_db)?;

        if let Some(seq) = seq {
            // 真実のソースへ書けたときのみ publish（DB=truth・Redis=best-effort 起床）。
            let se = StreamEvent {
                seq,
                event: event.clone(),
            };
            if let Ok(s) = serde_json::to_string(&se) {
                self.publish_event(run_id, &s).await;
            }
        }
        Ok(seq)
    }

    /// run を確定する（message.content projection＋端末 status＋終端イベントを**単一 TX**で）。
    /// fencing 一致時のみ。戻り `false` = fencing 不一致（ゾンビ）で no-op。
    ///
    /// `terminal_event`（Done / cancelled Status）は status 更新と**同一 TX**でコミットする。
    /// 分割コミットだと (a) status が先: SSE 側が「端末 status＋残イベント無し」を観測して
    /// 終端イベント未配信のままストリームを閉じる、(b) イベントが先: Done 配信時点で
    /// projection 未確定、のどちらかの race が生じるため（worker_it の flake の根本原因）。
    pub async fn finalize_run(
        &self,
        run_id: Uuid,
        fencing_token: i64,
        status: RunStatus,
        content: &[ContentBlock],
        last_error: Option<&str>,
        terminal_event: Option<&StreamEventKind>,
    ) -> Result<bool, ChatError> {
        self.finalize_run_with_ui(
            run_id,
            fencing_token,
            status,
            content,
            last_error,
            terminal_event,
            &[],
        )
        .await
    }

    /// [`Self::finalize_run`] に、本文の図のワークフローのボタンの束縛（#604）を添えて確定する。
    ///
    /// 束縛は本文と**同じトランザクション**で書く。確定した直後に押されても束縛が見え、
    /// 本文が確定しなかった（fencing 不一致）ときは束縛も残らない。
    #[allow(clippy::too_many_arguments)]
    pub async fn finalize_run_with_ui(
        &self,
        run_id: Uuid,
        fencing_token: i64,
        status: RunStatus,
        content: &[ContentBlock],
        last_error: Option<&str>,
        terminal_event: Option<&StreamEventKind>,
        ui_bindings: &[crate::yui_actions::UiWorkflowBinding],
    ) -> Result<bool, ChatError> {
        let mut tx = self.db.begin().await.map_err(map_db)?;
        let kv = [KeyValue::Uuid(run_id)];
        let message_id: Option<Uuid> = durable::fenced_finalize(
            &mut *tx,
            &RUN_SPEC,
            &Key::new(RUN_KEY_COLUMNS, &kv),
            fencing_token,
            status.as_str(),
            &[("last_error", KeyValue::OptText(last_error))],
            "message_id",
        )
        .await
        .map_err(map_db)?;
        let Some(message_id) = message_id else {
            tx.rollback().await.map_err(map_db)?;
            return Ok(false);
        };
        sqlx::query("UPDATE message SET content = $2 WHERE id = $1")
            .bind(message_id)
            .bind(Json(content))
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        super::yui_actions::insert_bindings(&mut tx, message_id, ui_bindings).await?;
        // 端末確定でチェックポイントを落とす（再開対象でなくなった run の履歴 JSON を残さない・#351）。
        sqlx::query("UPDATE generation_run SET checkpoint = NULL WHERE run_id = $1")
            .bind(run_id)
            .execute(&mut *tx)
            .await
            .map_err(map_db)?;
        // 終端イベントも同一 TX で追記し、publish は commit 後（DB=truth・Redis=起床通知）。
        let mut appended: Option<StreamEvent> = None;
        if let Some(event) = terminal_event {
            let payload = serde_json::to_value(event)
                .map_err(|e| ChatError::Internal(format!("event serialize: {e}")))?;
            let seq = durable::append_event(
                &mut *tx,
                &RUN_SPEC,
                &EVENT_SPEC,
                &Key::new(RUN_KEY_COLUMNS, &kv),
                event.tag(),
                &payload,
                fencing_token,
            )
            .await
            .map_err(map_db)?;
            appended = seq.map(|seq| StreamEvent {
                seq,
                event: event.clone(),
            });
        }
        tx.commit().await.map_err(map_db)?;
        if let Some(se) = appended {
            if let Ok(s) = serde_json::to_string(&se) {
                self.publish_event(run_id, &s).await;
            }
        }
        Ok(true)
    }

    /// ユーザー明示停止（editor 権限）。cancel_requested を立てる（ページ離脱≠キャンセル）。
    pub async fn request_cancel(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        run_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<(), ChatError> {
        self.require_thread(
            ctx,
            thread_id,
            Relation::Editor,
            "thread.run.cancel",
            trace_id,
        )
        .await?;
        let updated = sqlx::query(
            "UPDATE generation_run SET cancel_requested = true, updated_at = now() \
             WHERE run_id = $1 AND thread_id = $2 AND tenant_id = $3",
        )
        .bind(run_id)
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .execute(&self.db)
        .await
        .map_err(map_db)?;
        if updated.rows_affected() == 0 {
            return Err(ChatError::NotFound);
        }
        Ok(())
    }

    /// このスレッドで**いま UI が映すべき run**（SSE 購読対象）を返す。
    ///
    /// 非端末の run があれば**その最も古いもの**。生成は 1 スレッド 1 本ずつ直列に走るので
    /// （[`Self::blocked_by_earlier_run`]）、これが「いま動いている run」であり、後ろに
    /// 積まれた run は順番待ち。**最新**を返すと、生成中に届いた発話へ購読が飛び移り、
    /// 進行中の応答が画面から消える。
    ///
    /// 非端末が無ければ最新（＝直前に終わった run）を返す。再訪時のリプレイ先。
    /// `autonomous` は再訪 UI の復元用（自律 run 進行中ならモードセレクタ等を出す・#350）。
    pub async fn current_run(
        &self,
        thread_id: Uuid,
        tenant_id: &str,
    ) -> Result<Option<CurrentRun>, ChatError> {
        let row: Option<(Uuid, Uuid, String, bool)> = sqlx::query_as(
            "SELECT run_id, message_id, status, autonomous FROM generation_run \
             WHERE thread_id = $1 AND tenant_id = $2 \
             ORDER BY (status IN ('queued', 'running')) DESC, \
                      CASE WHEN status IN ('queued', 'running') THEN created_at END ASC, \
                      created_at DESC, run_id DESC \
             LIMIT 1",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row.and_then(|(run_id, message_id, s, autonomous)| {
            RunStatus::parse(&s).map(|status| CurrentRun {
                run_id,
                message_id,
                status,
                autonomous,
            })
        }))
    }

    /// スレッドで最後に記録されたコンテキスト使用量（再訪時のコンテキスト表示）。
    ///
    /// 新しい run から順に、その run の最後の `context_usage` イベントを引く（取り消しや失敗で
    /// 1 度も生成しなかった run は飛ばす）。イベントは PK (run_id, seq) を逆順に辿る。
    pub async fn latest_context_usage(
        &self,
        thread_id: Uuid,
        tenant_id: &str,
    ) -> Result<Option<agent_core::ContextUsage>, ChatError> {
        let row: Option<(Json<StreamEventKind>,)> = sqlx::query_as(
            "SELECT e.payload FROM generation_run r \
             CROSS JOIN LATERAL ( \
                 SELECT payload FROM generation_event \
                 WHERE tenant_id = r.tenant_id AND run_id = r.run_id \
                   AND type = 'context_usage' \
                 ORDER BY seq DESC LIMIT 1 \
             ) e \
             WHERE r.thread_id = $1 AND r.tenant_id = $2 \
             ORDER BY r.created_at DESC, r.run_id DESC \
             LIMIT 1",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row.and_then(|(Json(kind),)| match kind {
            StreamEventKind::ContextUsage { usage } => Some(usage),
            _ => None,
        }))
    }

    /// この run より**先に投入された未完了 run**が同じスレッドにあるか（＝順番待ちか）。
    ///
    /// 1 スレッドの生成を直列化するための唯一の判定。並行に走らせると、後の run が前の run の
    /// 出力を含まない履歴で生成し（発話順と応答が食い違う）、同じワークスペースへ同時に書き、
    /// 承認カードが 2 本同時に出る。
    ///
    /// 待ちは有限時間で解ける: 先行 run は必ず done/failed/cancelled へ到達する
    /// （完走・明示キャンセル・リース失効の takeover・jobq の DLQ 移送時の `force_fail_run`）。
    pub async fn blocked_by_earlier_run(&self, run_id: Uuid) -> Result<bool, ChatError> {
        sqlx::query_scalar::<_, bool>(
            "SELECT EXISTS ( \
                 SELECT 1 FROM generation_run earlier, generation_run me \
                 WHERE me.run_id = $1 \
                   AND earlier.thread_id = me.thread_id \
                   AND earlier.tenant_id = me.tenant_id \
                   AND earlier.status IN ('queued', 'running') \
                   AND (earlier.created_at, earlier.run_id) < (me.created_at, me.run_id) \
             )",
        )
        .bind(run_id)
        .fetch_one(&self.db)
        .await
        .map_err(map_db)
    }

    /// 生成がまだ始まっていない発話（＝順番待ち）の `(user メッセージ id, run id)`。
    ///
    /// 再訪時に「送ったが順番待ち」を復元し、取り消せるようにするための材料。`queued` は
    /// claim 前の run＝1 件も生成イベントが出ていない run で、`running` は既に進行中なので
    /// 含めない。
    ///
    /// **いま映している run（`current_run_id`）は必ず除く。** ワーカーが claim する前は
    /// 「最も古い未完了 run」と「queued な run」が同じものを指すため、除かないと*これから
    /// 流れてくる発話*が順番待ちとして扱われる。実際、計画を承認した発話が調査の実況の下に
    /// 「順番待ち」で居座り、承認したのに送られていないように見えていた。
    pub async fn queued_user_messages(
        &self,
        thread_id: Uuid,
        tenant_id: &str,
        current_run_id: Uuid,
    ) -> Result<Vec<(Uuid, Uuid)>, ChatError> {
        sqlx::query_as::<_, (Uuid, Uuid)>(
            "SELECT m.parent_id, r.run_id FROM generation_run r JOIN message m ON m.id = r.message_id \
             WHERE r.thread_id = $1 AND r.tenant_id = $2 AND r.status = 'queued' \
               AND r.run_id <> $3 \
               AND m.parent_id IS NOT NULL \
             ORDER BY r.created_at, r.run_id",
        )
        .bind(thread_id)
        .bind(tenant_id)
        .bind(current_run_id)
        .fetch_all(&self.db)
        .await
        .map_err(map_db)
    }

    /// そのメッセージを生成した run の**生成材料**（自律か・適用中の skill ピン）を引く（#387）。
    ///
    /// 本文の図（```ui の ask・plan）は自律 run の途中で出る。回答を投稿する
    /// `chat.submit` が素の設定で run を起こすと、続きが別物になる:
    ///
    /// - 非自律だと `max_steps=6`・`plan` ツール無しの制約版になる
    /// - skill ピンが落ちると、スラッシュコマンド起動の skill（run 単位ピン）は**1 ターン目に
    ///   しか適用されない**。手順書もフェーズ宣言（#400）も 2 ターン目から消え、
    ///   「質問 → 計画 → 実行」は最初の 1 手だけ守られて残りが素通りになる（#402 の実害）。
    ///
    /// **カードを出した run の生成材料を継ぐ**ための問い合わせ。
    ///
    /// **認可はこのメソッド内で先に取る**（呼び出し側の後段 `post_message` に委ねると、
    /// 未認可の読み取りが先に走り、単独利用で confused deputy になる）。
    pub async fn message_run_context(
        &self,
        ctx: &AuthContext,
        thread_id: Uuid,
        message_id: Uuid,
        trace_id: Option<&str>,
    ) -> Result<Option<RunContext>, ChatError> {
        self.require_thread(ctx, thread_id, Relation::Editor, "thread.run.get", trace_id)
            .await?;
        type Row = (
            Uuid,
            bool,
            Json<Vec<SkillPin>>,
            Option<String>,
            Option<String>,
            bool,
            Json<Vec<String>>,
        );
        let row: Option<Row> = sqlx::query_as(
            "SELECT run_id, autonomous, skill_pins, model, effort, model_auto, requested_tools \
             FROM generation_run \
             WHERE message_id = $1 AND thread_id = $2 AND tenant_id = $3 \
             ORDER BY created_at DESC, run_id DESC LIMIT 1",
        )
        .bind(message_id)
        .bind(thread_id)
        .bind(&ctx.tenant_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(row.map(
            |(run_id, autonomous, pins, model, effort, model_auto, tools)| RunContext {
                autonomous,
                skill_pins: pins.0,
                model: model.map(|model| crate::model::RunModel {
                    model,
                    effort: effort.as_deref().and_then(llm_gateway::Effort::parse),
                    auto: model_auto,
                }),
                requested_tools: known_tool_names(run_id, &tools.0),
            },
        ))
    }

    /// run の現在状態を引く（SSE の端末判定・crash safety）。他テナントの run は `None`。
    pub async fn run_status(
        &self,
        tenant_id: &str,
        run_id: Uuid,
    ) -> Result<Option<RunStatus>, ChatError> {
        let s: Option<String> = sqlx::query_scalar(
            "SELECT status FROM generation_run WHERE tenant_id = $1 AND run_id = $2",
        )
        .bind(tenant_id)
        .bind(run_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db)?;
        Ok(s.and_then(|s| RunStatus::parse(&s)))
    }

    /// `from_seq` より後のイベントを replay する（真実のソース・SSE の補填/復元）。
    ///
    /// 他テナントの run_id を渡しても 0 件（tenant_id でも絞る・#815）。
    pub async fn replay_events(
        &self,
        tenant_id: &str,
        run_id: Uuid,
        from_seq: i64,
    ) -> Result<Vec<StreamEvent>, ChatError> {
        let kv = [KeyValue::Text(tenant_id), KeyValue::Uuid(run_id)];
        let rows: Vec<(i64, StreamEventKind)> = durable::replay_events(
            &self.db,
            &EVENT_SPEC,
            &Key::new(EVENT_READ_KEY_COLUMNS, &kv),
            from_seq,
        )
        .await
        .map_err(map_db)?;
        Ok(rows
            .into_iter()
            .map(|(seq, event)| StreamEvent { seq, event })
            .collect())
    }
}

// 強制失敗・孤児回収（fencing 無視の backstop）は [`super::reaper`] に分離。

#[allow(clippy::needless_pass_by_value)]
pub(super) fn map_db(e: sqlx::Error) -> ChatError {
    ChatError::Internal(format!("db: {e}"))
}

/// SSE ストリームの端末イベントか（Done / Error / cancelled Status）。
pub(crate) fn is_terminal_event(ev: &StreamEventKind) -> bool {
    matches!(
        ev,
        StreamEventKind::Done { .. }
            | StreamEventKind::Error { .. }
            | StreamEventKind::Status {
                status: RunStatus::Cancelled | RunStatus::Failed | RunStatus::Done
            }
    )
}
