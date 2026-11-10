//! 起動した本人に返す run の要約（会話の AI の試運転の観測・D-10・設計 §4.3）。
//!
//! [`RunStore::run_digest`] は、**本人が画面・会話から起動した run だけ**を SQL で絞る（結果の読取口と同じ条件
//! `OWN_RUN`・二重の関門の二段目）。他人の run・別のテナントの run・schedule / event の run（実行主体は workflow
//! プリンシパル）は `None` になる。
//!
//! 返すのは次のものだけ。実行履歴の詳細（`run_detail`）は元にしない（入力・きっかけの内容・生の失敗の文を持つため）。
//!
//! - run の状態と、待機中なら待っているもの（`WAITING_FOR`）。
//! - 要素でない step ごとの状態・取った出口・誤りの code と文の先頭・出力の先頭（[`DIGEST_OUTPUT_CHARS`] 字まで・
//!   SQL の `left(output::text, n)` で取り、全体は読まない）。通らなかった（skipped）step には、上流がどの出口を
//!   取ったかから導いた理由（[`DigestSkip`]・列は足さない）。
//! - map の要素の step は (繰り返しのブロック・中のブロック・状態) ごとの件数に畳む（要素の出力は読まない）。
//! - 作った・書き換えたレコード（`data.record.create`・`upsert`・`update`・`data.transition`）の table_id と id。
//!   出力の本文は読まず、SQL で 2 つの値だけを取る（繰り返しの中も同じ・[`MAX_DIGEST_RECORDS`] 件まで）。UUID として
//!   読めるものだけを返す。
//! - 失敗は分類と参照番号（[`run_failure`]）。run の `fail_reason` の生の文は返さない。
//!
//! interactive の run の値はすべて本人の権限で読んだものなので、本人に返しても新たに読めるようになるものは無い。
//! ただし出力・誤りの文・ノード id は作成者や外の相手が書いた文字を含むので、モデルに渡すときは封筒で包むこと
//! （PIT-56）。

use std::collections::HashMap;

use serde::Serialize;
use uuid::Uuid;

use super::outcome::{FAILURE_CODE_COLUMNS, OWN_RUN, WAITING_FOR};
use super::{map_db, RunStore, RunStoreError};
use crate::ir::WorkflowIr;
use crate::run::failure::{failure_code, run_failure};
use crate::run::graph::RunGraph;
use crate::run::outcome::{RunFailure, WaitingFor};
use crate::run::{RunStatus, StepStatus};
use crate::vocab::NodeType;

/// 出力・誤りの文の先頭として返す文字数（Postgres の `left` の文字数）。
pub const DIGEST_OUTPUT_CHARS: i32 = 600;
/// 返すレコードの上限（超えた分は [`RunDigest::records_omitted`] に数える）。
pub const MAX_DIGEST_RECORDS: i64 = 50;

/// run の要約（[`RunStore::run_digest`]）。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunDigest {
    pub run_id: Uuid,
    pub workflow_id: Uuid,
    /// run が固定した版（呼び出し側が渡した IR の版と突き合わせる）。
    pub version: i64,
    pub status: RunStatus,
    /// 待機中（running で、実行中・実行待ちの step が無く、待つブロックで止まっている）なら待っているもの。
    pub waiting_for: Option<WaitingFor>,
    /// 失敗した run の分類・利用者向けの文・参照番号（生の失敗の文は含めない）。
    pub failure: Option<RunFailure>,
    /// 要素でない step（実行の順）。
    pub steps: Vec<DigestStep>,
    /// map の要素の step の件数（繰り返しのブロック・中のブロック・状態ごと）。
    pub map_counts: Vec<DigestMapCount>,
    /// 作った・書き換えたレコード（実行の順・要素でない step が先）。
    pub records: Vec<DigestRecord>,
    /// [`MAX_DIGEST_RECORDS`] を超えて載せなかったレコードの件数（実行履歴で確かめる）。
    pub records_omitted: i64,
}

/// 要素でない step 1 つ。
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DigestStep {
    pub node_id: String,
    /// ノードの種類（固定した版の IR から・語彙に無ければ `None`）。
    pub node_type: Option<&'static str>,
    pub status: StepStatus,
    pub taken_ports: Vec<String>,
    /// 誤り（失敗した step・error の出口を取った step）。
    pub error: Option<DigestError>,
    /// 出力の JSON の先頭（[`DIGEST_OUTPUT_CHARS`] 字まで・出力が無ければ `None`）。
    pub output_head: Option<String>,
    /// 出力が先頭より長いか。
    pub output_truncated: bool,
    /// 通らなかった理由（skipped のときだけ・上流ごと）。
    pub skipped_because: Vec<DigestSkip>,
}

/// step の誤り（code と文の先頭）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DigestError {
    pub code: Option<String>,
    /// 誤りの文の先頭（[`DIGEST_OUTPUT_CHARS`] 字まで）。
    pub message: Option<String>,
}

/// skipped の step の理由（上流 1 つ分）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DigestSkip {
    /// 上流のブロック。
    pub upstream: String,
    /// この step へ線が出ている上流の出口。
    pub port: String,
    /// 上流の状態。
    pub upstream_status: StepStatus,
    /// 上流が取った出口（上流も通らなかったなら空）。
    pub upstream_taken: Vec<String>,
}

impl DigestSkip {
    /// 理由の文（観測に載せる・ブロックと出口の名前は IR のもの）。
    #[must_use]
    pub fn describe(&self) -> String {
        if self.upstream_status == StepStatus::Skipped {
            format!("上流の {} も通らなかったため", self.upstream)
        } else if self.upstream_taken.is_empty() {
            format!(
                "上流の {} がどの出口も取らなかったため（{}）",
                self.upstream,
                self.upstream_status.as_str()
            )
        } else {
            format!(
                "上流の {} が出口 {} を取ったため（{} からの線を通らなかった）",
                self.upstream,
                self.upstream_taken.join("・"),
                self.port
            )
        }
    }
}

/// map の要素の step の件数。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DigestMapCount {
    /// 繰り返しのブロック（中のブロックの `parent`）。
    pub map_node: String,
    /// 繰り返しの中のブロック。
    pub node_id: String,
    pub status: StepStatus,
    pub count: i64,
}

/// レコードの変更の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecordChange {
    /// 追加した（data.record.create・upsert で追加したとき）。
    Created,
    /// 書き換えた（data.record.update・data.transition・upsert で書き換えたとき）。
    Updated,
}

/// 作った・書き換えたレコード 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DigestRecord {
    pub node_id: String,
    /// step の path（繰り返しの中なら `<map>[i].<node>`）。
    pub step_path: String,
    pub table_id: Uuid,
    pub record_id: Uuid,
    pub change: RecordChange,
}

/// run の行（状態・待ち・版・失敗の code）。
type RunRow = (
    Uuid,
    String,
    Option<String>,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
);

/// 要素でない step の行。
type StepRow = (
    String,
    String,
    Vec<String>,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
);

/// レコードの行（step_path・node_id・table_id・id・created・全体の件数）。
type RecordRow = (
    String,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    i64,
);

impl RunStore {
    /// 本人が起動した run の要約（上のモジュールの説明）。本人の run でなければ `None`。
    ///
    /// `ir` は run が固定した版の IR（呼び出し側が起動に使った版・[`RunDigest::version`] と突き合わせる）。通らなかった
    /// 理由と、ノードの種類・繰り返しのブロックをここから引く。
    pub async fn run_digest(
        &self,
        ctx: &authz::AuthContext,
        run_id: Uuid,
        ir: &WorkflowIr,
    ) -> Result<Option<RunDigest>, RunStoreError> {
        let tenant_id = ctx.tenant_id();
        let row: Option<RunRow> = sqlx::query_as(&format!(
            "SELECT r.workflow_id, r.status, {WAITING_FOR}, r.version, r.fail_reason, \
                    {FAILURE_CODE_COLUMNS} \
             FROM workflow_run r WHERE r.run_id = $2 AND {OWN_RUN}"
        ))
        .bind(tenant_id)
        .bind(run_id)
        .bind(&ctx.principal().id)
        .bind(ctx.org())
        .fetch_optional(self.db())
        .await
        .map_err(map_db)?;
        let Some((workflow_id, status, waiting, version, fail_reason, step_code, item_code)) = row
        else {
            return Ok(None);
        };
        let status = RunStatus::parse(&status)
            .ok_or_else(|| RunStoreError::Internal(format!("run の状態が読めません: {status}")))?;
        let failure = (status == RunStatus::Failed).then(|| {
            let code = failure_code(step_code.as_deref(), item_code.as_deref());
            run_failure(run_id, code, fail_reason.as_deref())
        });

        let graph = RunGraph::build(ir);
        let steps = self.digest_steps(tenant_id, run_id, &graph).await?;
        let map_counts = self.digest_map_counts(tenant_id, run_id, &graph).await?;
        let (records, records_omitted) = self.digest_records(tenant_id, run_id, ir).await?;
        Ok(Some(RunDigest {
            run_id,
            workflow_id,
            version,
            status,
            waiting_for: waiting.as_deref().and_then(WaitingFor::parse),
            failure,
            steps,
            map_counts,
            records,
            records_omitted,
        }))
    }

    /// 要素でない step（出力と誤りの文は先頭だけ・実行の順）。
    async fn digest_steps(
        &self,
        tenant_id: &str,
        run_id: Uuid,
        graph: &RunGraph,
    ) -> Result<Vec<DigestStep>, RunStoreError> {
        let rows: Vec<StepRow> = sqlx::query_as(
            "SELECT s.node_id, s.status, s.taken_ports, s.error->>'code', \
                    left(s.error->>'message', $3), left(s.output::text, $3), \
                    coalesce(length(s.output::text) > $3, false) \
             FROM step_execution s \
             WHERE s.tenant_id = $1 AND s.run_id = $2 AND strpos(s.step_path, '[') = 0",
        )
        .bind(tenant_id)
        .bind(run_id)
        .bind(DIGEST_OUTPUT_CHARS)
        .fetch_all(self.db())
        .await
        .map_err(map_db)?;
        let mut steps: Vec<DigestStep> = rows
            .into_iter()
            .filter_map(
                |(node_id, status, taken_ports, code, message, output_head, output_truncated)| {
                    let status = StepStatus::parse(&status)?;
                    let error = (code.is_some() || message.is_some())
                        .then_some(DigestError { code, message });
                    Some(DigestStep {
                        node_type: graph.node_type(&node_id).map(NodeType::as_str),
                        node_id,
                        status,
                        taken_ports,
                        error,
                        output_head,
                        output_truncated,
                        skipped_because: Vec::new(),
                    })
                },
            )
            .collect();
        let causes: Vec<Vec<DigestSkip>> = {
            let by_node: HashMap<&str, (StepStatus, &[String])> = steps
                .iter()
                .map(|s| (s.node_id.as_str(), (s.status, s.taken_ports.as_slice())))
                .collect();
            steps
                .iter()
                .map(|s| {
                    if s.status == StepStatus::Skipped {
                        skip_causes(&s.node_id, graph, &by_node)
                    } else {
                        Vec::new()
                    }
                })
                .collect()
        };
        for (step, because) in steps.iter_mut().zip(causes) {
            step.skipped_because = because;
        }
        let order: HashMap<&str, usize> = graph
            .step_order()
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();
        steps.sort_by(|a, b| {
            let pos = |s: &DigestStep| order.get(s.node_id.as_str()).copied();
            pos(a)
                .unwrap_or(usize::MAX)
                .cmp(&pos(b).unwrap_or(usize::MAX))
                .then_with(|| a.node_id.cmp(&b.node_id))
        });
        Ok(steps)
    }

    /// map の要素の step の件数（中のブロック・状態ごと・要素の出力は読まない）。
    async fn digest_map_counts(
        &self,
        tenant_id: &str,
        run_id: Uuid,
        graph: &RunGraph,
    ) -> Result<Vec<DigestMapCount>, RunStoreError> {
        let rows: Vec<(String, String, i64)> = sqlx::query_as(
            "SELECT s.node_id, s.status, count(*) FROM step_execution s \
             WHERE s.tenant_id = $1 AND s.run_id = $2 AND strpos(s.step_path, '[') > 0 \
             GROUP BY s.node_id, s.status",
        )
        .bind(tenant_id)
        .bind(run_id)
        .fetch_all(self.db())
        .await
        .map_err(map_db)?;
        let order: HashMap<&str, usize> = graph
            .step_order()
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();
        let mut out: Vec<DigestMapCount> = rows
            .into_iter()
            .filter_map(|(node_id, status, count)| {
                Some(DigestMapCount {
                    // 要素の step は必ず繰り返しの中のブロック（ノード id は IR で一意なので親は 1 つ）。
                    map_node: graph.parent(&node_id).unwrap_or_default().to_string(),
                    status: StepStatus::parse(&status)?,
                    node_id,
                    count,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            let pos = |c: &DigestMapCount| order.get(c.node_id.as_str()).copied();
            pos(a)
                .unwrap_or(usize::MAX)
                .cmp(&pos(b).unwrap_or(usize::MAX))
                .then_with(|| a.node_id.cmp(&b.node_id))
                .then_with(|| a.status.as_str().cmp(b.status.as_str()))
        });
        Ok(out)
    }

    /// 作った・書き換えたレコード（出力から table_id と id だけを SQL で取る・上限つき）。
    async fn digest_records(
        &self,
        tenant_id: &str,
        run_id: Uuid,
        ir: &WorkflowIr,
    ) -> Result<(Vec<DigestRecord>, i64), RunStoreError> {
        let kinds: HashMap<&str, NodeType> = ir
            .nodes
            .iter()
            .filter_map(|n| {
                let nt = NodeType::parse(&n.node_type)?;
                record_writer(nt).then_some((n.id.as_str(), nt))
            })
            .collect();
        if kinds.is_empty() {
            return Ok((Vec::new(), 0));
        }
        let ids: Vec<&str> = kinds.keys().copied().collect();
        let rows: Vec<RecordRow> = sqlx::query_as(
            "SELECT s.step_path, s.node_id, s.output->>'table_id', s.output->>'id', \
                    s.output->>'created', count(*) OVER () \
             FROM step_execution s \
             WHERE s.tenant_id = $1 AND s.run_id = $2 AND s.status = 'succeeded' \
               AND s.node_id = ANY($3) \
             ORDER BY strpos(s.step_path, '[') > 0, s.updated_at, s.step_path \
             LIMIT $4",
        )
        .bind(tenant_id)
        .bind(run_id)
        .bind(&ids)
        .bind(MAX_DIGEST_RECORDS)
        .fetch_all(self.db())
        .await
        .map_err(map_db)?;
        let total = rows.first().map_or(0, |r| r.5);
        let returned = i64::try_from(rows.len()).unwrap_or(i64::MAX);
        let records = rows
            .into_iter()
            .filter_map(|(step_path, node_id, table_id, id, created, _)| {
                let nt = *kinds.get(node_id.as_str())?;
                Some(DigestRecord {
                    table_id: Uuid::parse_str(table_id.as_deref()?).ok()?,
                    record_id: Uuid::parse_str(id.as_deref()?).ok()?,
                    change: record_change(nt, created.as_deref()),
                    node_id,
                    step_path,
                })
            })
            .collect();
        Ok((records, total.saturating_sub(returned).max(0)))
    }
}

/// 出力にレコード（`id`・`table_id`）を返す書込のブロックか。
fn record_writer(nt: NodeType) -> bool {
    matches!(
        nt,
        NodeType::DataRecordCreate
            | NodeType::DataRecordUpsert
            | NodeType::DataRecordUpdate
            | NodeType::DataTransition
    )
}

/// レコードの変更の種類（upsert は出力の `created` で分ける）。
fn record_change(nt: NodeType, created: Option<&str>) -> RecordChange {
    match nt {
        NodeType::DataRecordCreate => RecordChange::Created,
        NodeType::DataRecordUpsert if created == Some("true") => RecordChange::Created,
        _ => RecordChange::Updated,
    }
}

/// 通らなかった step の理由（入ってくる線のうち、上流が終わっていてその出口を取らなかったもの）。
///
/// 上流が終わっていない線（取り消し・失敗で止まった run）は理由にしない。readiness の skip 伝播（線が Dead）と
/// 同じ見方（`edge_state`）。
fn skip_causes(
    node_id: &str,
    graph: &RunGraph,
    by_node: &HashMap<&str, (StepStatus, &[String])>,
) -> Vec<DigestSkip> {
    graph
        .in_edges(node_id)
        .iter()
        .filter_map(|(from, port)| {
            let (status, taken) = by_node.get(from.as_str())?;
            let dead = super::edge_state(port, status.is_terminal(), taken)
                == crate::run::readiness::EdgeState::Dead;
            dead.then(|| DigestSkip {
                upstream: from.clone(),
                port: port.clone(),
                upstream_status: *status,
                upstream_taken: taken.to_vec(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn graph() -> RunGraph {
        let ir = WorkflowIr::from_json(&json!({
            "ir_version": 1, "name": "wf",
            "nodes": [
                { "id": "check", "type": "control.branch", "params": {} },
                { "id": "yes", "type": "storage.read", "params": {} },
                { "id": "after", "type": "storage.read", "params": {} },
                { "id": "wait", "type": "storage.read", "params": {} }
            ],
            "edges": [
                { "from": "check", "from_port": "true", "to": "yes" },
                { "from": "yes", "to": "after" },
                { "from": "check", "from_port": "false", "to": "wait" }
            ]
        }))
        .unwrap();
        RunGraph::build(&ir)
    }

    /// 上流が別の出口を取った・上流も通らなかった・上流がまだ終わっていない、を言い分ける。
    #[test]
    fn skip_causes_follow_the_upstream_ports() {
        let g = graph();
        let falsy = vec!["false".to_string()];
        let none: Vec<String> = Vec::new();
        let by_node: HashMap<&str, (StepStatus, &[String])> = [
            ("check", (StepStatus::Succeeded, falsy.as_slice())),
            ("yes", (StepStatus::Skipped, none.as_slice())),
            ("after", (StepStatus::Skipped, none.as_slice())),
            ("wait", (StepStatus::Running, none.as_slice())),
        ]
        .into_iter()
        .collect();
        let yes = skip_causes("yes", &g, &by_node);
        assert_eq!(yes.len(), 1);
        assert_eq!(yes[0].upstream, "check");
        assert_eq!(yes[0].port, "true");
        assert_eq!(yes[0].upstream_taken, falsy);
        assert_eq!(
            yes[0].describe(),
            "上流の check が出口 false を取ったため（true からの線を通らなかった）"
        );
        let after = skip_causes("after", &g, &by_node);
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].describe(), "上流の yes も通らなかったため");
        // 上流がまだ終わっていない線は理由にしない。
        let running: HashMap<&str, (StepStatus, &[String])> =
            [("check", (StepStatus::Running, none.as_slice()))]
                .into_iter()
                .collect();
        assert!(skip_causes("yes", &g, &running).is_empty());
        // 失敗して出口を取らなかった上流。
        let failed: HashMap<&str, (StepStatus, &[String])> =
            [("check", (StepStatus::Failed, none.as_slice()))]
                .into_iter()
                .collect();
        assert_eq!(
            skip_causes("yes", &g, &failed)[0].describe(),
            "上流の check がどの出口も取らなかったため（failed）"
        );
        // 記録の無い上流は理由にしない。
        assert!(skip_causes("yes", &g, &HashMap::new()).is_empty());
    }

    #[test]
    fn record_changes_by_block() {
        assert!(record_writer(NodeType::DataRecordCreate));
        assert!(record_writer(NodeType::DataTransition));
        assert!(!record_writer(NodeType::DataRecordDelete));
        assert!(!record_writer(NodeType::DataQuery));
        assert_eq!(
            record_change(NodeType::DataRecordCreate, None),
            RecordChange::Created
        );
        assert_eq!(
            record_change(NodeType::DataRecordUpsert, Some("true")),
            RecordChange::Created
        );
        assert_eq!(
            record_change(NodeType::DataRecordUpsert, Some("false")),
            RecordChange::Updated
        );
        assert_eq!(
            record_change(NodeType::DataRecordUpdate, None),
            RecordChange::Updated
        );
        assert_eq!(
            serde_json::to_value(RecordChange::Created).unwrap(),
            json!("created")
        );
    }
}
