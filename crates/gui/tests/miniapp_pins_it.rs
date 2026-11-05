//! 束ねた部品の新しいバージョンと、上げると変わるところの結合テスト（GAP-11・GAP-86）。
//!
//! 実 Postgres（`STORAGE_TEST_DATABASE_URL`）＋実 OpenFGA（`OPENFGA_TEST_URL`）で次を確かめる。
//!
//! - ワークフローを改訂すると、固定した版より新しい版と、入力項目の変化（増える・必須になる）、使ってよい操作の増減、
//!   使う人に要る権限の増減を返す。要る権限の対象は、呼んだ人に見えるものだけ名前を付ける（見えないテーブルは名前も
//!   id も返さない）。
//! - 保存ビューを改訂すると、変わる部分（絞り込みと並べ替え）だけを返す。
//! - スキルを改訂すると、変わる設定（AI への指示・参照するフォルダー）と、参照するフォルダーを見る権限が要るように
//!   なることを返す（GAP-86）。
//! - 最新の版を固定していれば、差は空。
//! - 呼んだ人が読めない部品・種類の違う部品は `readable: false` で、最新の版も差も返さない（存在を区別しない）。
//!
//! テストで作った行（テナントごと）と OpenFGA のストアは、落ちても最後に消す。

#![allow(
    clippy::pedantic,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stderr
)]

mod access_env;
mod common;

use std::sync::Arc;

use access_env::{ctx, pool, run_with_cleanup, Env, Fga};
use artifact::{ArtifactKind, NewArtifact};
use authz::Relation;
use gui::{AccessKind, AccessObject, ComponentPin, PinAccess, PinDiff, SkillPart};
use serde_json::{json, Value};
use uuid::Uuid;

/// 入力項目と、ブロック（種類・テーブル）の並びから、つながった IR を作る。
fn flow(scopes: Value, input_schema: Value, nodes: &[(&str, &str, Uuid)]) -> Value {
    let nodes: Vec<Value> = nodes
        .iter()
        .map(|(id, ty, table)| {
            let mut params = json!({ "table": table });
            if *ty == "data.record.create" {
                params["data"] = json!({});
            }
            json!({ "id": id, "type": ty, "params": params })
        })
        .collect();
    let edges: Vec<Value> = nodes
        .windows(2)
        .map(|w| json!({ "from": w[0]["id"], "to": w[1]["id"] }))
        .collect();
    json!({
        "ir_version": 1, "name": "expense", "declared_scopes": scopes,
        "input_schema": input_schema, "nodes": nodes, "edges": edges
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn pin_updates_show_newer_versions_and_what_changes() {
    let Some(pool) = pool().await else { return };
    let Some(fga) = Fga::connect().await else {
        return;
    };
    let client = Arc::clone(&fga.client);
    let tenant = format!("t-pins-{}", Uuid::new_v4().simple());
    let t = tenant.clone();
    let db = pool.clone();
    run_with_cleanup(pool, tenant, fga, async move {
        body(Env::new(&db, &client), &t).await;
    })
    .await;
}

async fn body(e: Env, tenant: &str) {
    workflow_changes(&e, tenant).await;
    view_changes(&e, tenant).await;
    skill_changes(&e, tenant).await;
}

fn pin(artifact_id: Uuid, version: i64) -> ComponentPin {
    ComponentPin {
        artifact_id,
        version,
    }
}

/// 確かめやすい形にした、要る権限の増減 1 つ（増え方・対象の種類・操作・見える対象の名前・名前を確かめていないか）。
type AccessRow<'a> = (&'a str, AccessObject, AccessKind, Option<&'a str>, bool);

/// ワークフロー: 入力項目・使ってよい操作・要る権限の増減。読めない人・種類の違う部品・壊れたピン。
async fn workflow_changes(e: &Env, tenant: &str) {
    let artifacts = &e.artifacts;
    let apps = &e.mini_apps;
    let names = e.checker();
    let alice = ctx(tenant, "alice");
    let bob = ctx(tenant, "bob");
    let erin = ctx(tenant, "erin");
    let ledger = e.table(&alice, "経費の台帳").await;
    // alice に見えないテーブル（erin の持ち物）。
    let hidden = e.table(&erin, "人事の記録").await;

    // ワークフロー: v1 は金額だけが必須で台帳を読む。v2 で件名が増え（必須）、金額は必須でなくなり、使ってよい操作に
    // テーブルへの書き込みが増え、台帳にレコードを追加し、alice に見えないテーブルのレコードを消すようになる。
    let wf = artifacts
        .create(
            &alice,
            NewArtifact {
                kind: ArtifactKind::Workflow,
                name: "expense".into(),
                body: flow(
                    json!(["data.read"]),
                    json!({ "type": "object", "required": ["amount"],
                        "properties": { "amount": { "type": "integer", "title": "金額" } } }),
                    &[("q", "data.query", ledger)],
                ),
            },
            None,
        )
        .await
        .expect("workflow")
        .id;
    artifacts
        .append_version(
            &alice,
            wf,
            flow(
                json!(["data.read", "data.write"]),
                json!({ "type": "object", "required": ["title"], "x-order": ["title", "amount"],
                    "properties": {
                        "title": { "type": "string", "title": "件名" },
                        "amount": { "type": "integer", "title": "金額" }
                    } }),
                &[
                    ("add", "data.record.create", ledger),
                    ("del", "data.record.delete", hidden),
                ],
            ),
            Some(1),
            None,
        )
        .await
        .expect("workflow v2");
    let wf_updates = apps
        .pin_updates(
            &alice,
            ArtifactKind::Workflow,
            &[pin(wf, 1), pin(wf, 2)],
            &names,
            None,
        )
        .await
        .expect("pin updates");
    assert!(wf_updates[0].readable);
    assert_eq!(wf_updates[0].latest_version, Some(2));
    let changes = &wf_updates[0].changes;
    assert_eq!(
        changes[..3],
        [
            PinDiff::InputAdded {
                key: "title".into(),
                label: Some("件名".into()),
                required: true
            },
            PinDiff::InputRequired {
                key: "amount".into(),
                label: Some("金額".into()),
                required: false
            },
            PinDiff::ScopeAdded {
                scope: "data.write".into()
            },
        ]
    );
    // 要る権限の増減。見える台帳は名前つき、見えないテーブルは種類だけ（名前も id も返さない）。
    let access: Vec<(&str, &PinAccess)> = changes[3..]
        .iter()
        .map(|d| match d {
            PinDiff::AccessAdded {
                access,
                conditional: false,
            } => ("added", access),
            PinDiff::AccessRemoved { access } => ("removed", access),
            other => panic!("要る権限の差でない: {other:?}"),
        })
        .collect();
    let summary: Vec<AccessRow<'_>> = access
        .iter()
        .map(|(c, a)| {
            (
                *c,
                a.object,
                a.need,
                a.target.as_ref().map(|t| t.name.as_str()),
                a.dynamic || a.over_limit,
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            (
                "added",
                AccessObject::Table,
                AccessKind::Add,
                Some("経費の台帳"),
                false
            ),
            ("added", AccessObject::Table, AccessKind::Edit, None, false),
            (
                "removed",
                AccessObject::Table,
                AccessKind::Read,
                Some("経費の台帳"),
                false
            ),
        ]
    );
    assert_eq!(access[0].1.target.as_ref().unwrap().id, ledger);
    let serialized = serde_json::to_string(&wf_updates[0]).unwrap();
    assert!(
        !serialized.contains(&hidden.to_string()) && !serialized.contains("人事の記録"),
        "見えないテーブルの id と名前を返さない: {serialized}"
    );
    assert!(!wf_updates[0].diff_unknown);
    // 最新の版を固定していれば差は無い。
    assert_eq!(wf_updates[1].latest_version, Some(2));
    assert!(wf_updates[1].changes.is_empty());
    assert!(!wf_updates[1].diff_unknown);
    // 見えるようになれば、次の確かめから名前を付ける（呼んだ人に見えるかだけで決める）。
    e.fga
        .write_tuple(
            &alice.subject(),
            Relation::Viewer,
            &alice.ns().data_table(&hidden.to_string()),
        )
        .await
        .expect("tuple");
    let named = apps
        .pin_updates(&alice, ArtifactKind::Workflow, &[pin(wf, 1)], &names, None)
        .await
        .expect("named");
    assert!(
        named[0].changes.iter().any(|d| matches!(d,
            PinDiff::AccessAdded { access, .. } if access.target.as_ref().is_some_and(|t| t.name == "人事の記録"))),
        "{:?}",
        named[0].changes
    );
    // 固定している版の本文が無い（壊れたピン）ときは、差が空でも「変わらない」とは言わない。
    let broken = apps
        .pin_updates(&alice, ArtifactKind::Workflow, &[pin(wf, 0)], &names, None)
        .await
        .expect("broken pin");
    assert!(broken[0].readable);
    assert_eq!(broken[0].latest_version, Some(2));
    assert!(broken[0].changes.is_empty());
    assert!(broken[0].diff_unknown);

    // 読めない人には、最新の版も差も返さない。種類の違う部品も読めないと同じ扱い。
    let hidden_wf = apps
        .pin_updates(&bob, ArtifactKind::Workflow, &[pin(wf, 1)], &names, None)
        .await
        .expect("bob");
    assert!(!hidden_wf[0].readable);
    assert_eq!(hidden_wf[0].latest_version, None);
    assert!(hidden_wf[0].changes.is_empty());
    let wrong_kind = apps
        .pin_updates(&alice, ArtifactKind::DataView, &[pin(wf, 1)], &names, None)
        .await
        .expect("wrong kind");
    assert!(!wrong_kind[0].readable);
    assert_eq!(wrong_kind[0].latest_version, None);
    let missing = apps
        .pin_updates(
            &alice,
            ArtifactKind::Workflow,
            &[pin(Uuid::new_v4(), 1)],
            &names,
            None,
        )
        .await
        .expect("missing");
    assert!(!missing[0].readable);
}

/// 保存ビュー: 変わる部分だけ（読むテーブルは同じなので、要る権限の増減は無い）。
async fn view_changes(e: &Env, tenant: &str) {
    let artifacts = &e.artifacts;
    let apps = &e.mini_apps;
    let names = e.checker();
    let alice = ctx(tenant, "alice");
    let ledger = e.table(&alice, "申請の一覧の台帳").await;
    // 保存ビュー: v2 で絞り込みだけを変える。
    let view = artifacts
        .create(
            &alice,
            NewArtifact {
                kind: ArtifactKind::DataView,
                name: "requests".into(),
                body: json!({ "table_id": ledger, "query": {}, "display": {} }),
            },
            None,
        )
        .await
        .expect("view")
        .id;
    artifacts
        .append_version(
            &alice,
            view,
            json!({ "table_id": ledger, "query": { "limit": 20 }, "display": {} }),
            Some(1),
            None,
        )
        .await
        .expect("view v2");

    let view_updates = apps
        .pin_updates(
            &alice,
            ArtifactKind::DataView,
            &[pin(view, 1)],
            &names,
            None,
        )
        .await
        .expect("view updates");
    assert_eq!(view_updates[0].latest_version, Some(2));
    assert_eq!(view_updates[0].changes, vec![PinDiff::ViewQuery]);
}

/// スキル: 変わる設定と、参照するフォルダーを見る権限（GAP-86）。
async fn skill_changes(e: &Env, tenant: &str) {
    let artifacts = &e.artifacts;
    let apps = &e.mini_apps;
    let names = e.checker();
    let alice = ctx(tenant, "alice");
    let bob = ctx(tenant, "bob");
    // スキル: v2 で AI への指示と参照するフォルダーを変える（中身は返さない・GAP-86）。参照するフォルダーは、使う人に
    // 見る権限が要るようになる（alice に見えるので名前つき）。
    e.fga
        .write_tuple(
            &alice.subject(),
            Relation::Member,
            &alice.ns().organization("acme"),
        )
        .await
        .unwrap();
    let folder = e
        .storage
        .create_folder(&alice, None, "経費の規程", None)
        .await
        .expect("folder")
        .id;
    let skill = artifacts
        .create(
            &alice,
            NewArtifact {
                kind: ArtifactKind::Skill,
                name: "expense-guide".into(),
                body: json!({ "description": "経費の手引き", "instructions": "経費の規程に沿って答えます。" }),
            },
            None,
        )
        .await
        .expect("skill")
        .id;
    artifacts
        .append_version(
            &alice,
            skill,
            json!({
                "description": "経費の手引き",
                "instructions": "経費の規程に沿って、根拠の条文を添えて答えます。",
                "knowledge_scope": { "folders": [folder] }
            }),
            Some(1),
            None,
        )
        .await
        .expect("skill v2");
    let skill_updates = apps
        .pin_updates(
            &alice,
            ArtifactKind::Skill,
            &[pin(skill, 1), pin(skill, 2)],
            &names,
            None,
        )
        .await
        .expect("skill updates");
    assert!(skill_updates[0].readable);
    assert_eq!(skill_updates[0].latest_version, Some(2));
    assert_eq!(
        skill_updates[0].changes[..2],
        [
            PinDiff::Skill {
                part: SkillPart::Instructions
            },
            PinDiff::Skill {
                part: SkillPart::KnowledgeScope
            },
        ]
    );
    match &skill_updates[0].changes[2..] {
        [PinDiff::AccessAdded {
            access,
            conditional: false,
        }] => {
            assert_eq!(
                (access.object, access.need),
                (AccessObject::Folder, AccessKind::Read)
            );
            assert_eq!(
                access.target.as_ref().map(|t| t.name.as_str()),
                Some("経費の規程")
            );
        }
        other => panic!("参照するフォルダーを見る権限が要るようになる: {other:?}"),
    }
    assert!(!skill_updates[0].diff_unknown);
    assert!(skill_updates[1].changes.is_empty());
    let hidden_skill = apps
        .pin_updates(&bob, ArtifactKind::Skill, &[pin(skill, 1)], &names, None)
        .await
        .expect("bob skill");
    assert!(!hidden_skill[0].readable);
    assert_eq!(hidden_skill[0].latest_version, None);
}
