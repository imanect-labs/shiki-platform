//! 職員間メッセージの結合テスト（実 Postgres ＋ 実 OpenFGA・Task 14.16 の権限 negative を主軸に）。
//!
//! `STORAGE_TEST_DATABASE_URL` と `OPENFGA_TEST_URL` が無ければスキップする（`SHIKI_REQUIRE_IT=1` なら失敗）。
//! サービスは RLS を受ける `shiki_runtime` ロールの接続で動かす。

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::pedantic,
    clippy::too_many_lines
)]

use async_trait::async_trait;
use authz::{
    AuthContext, AuthzClient, OpenFgaClient, OpenFgaConfig, Principal, PrincipalKind, Relation,
};
use chat::model::{ContentBlock, Mention};
use messaging::files::{FileAccess, FileMeta};
use messaging::pubsub::{Hub, Signal};
use messaging::{
    AccessRequest, AgentAnswer, AgentRequest, AgentRuntime, ChannelKind, CreateChannel, Error,
    MessageState, MessagingService, NewMessage, PendingApprovalRef, ReplyMode,
};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

fn context(tenant: &str, user: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: PrincipalKind::User,
            id: user.into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "org".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}

/// 指定の利用者だけが読めるファイル（file_ref の権限評価の差し替え）。
#[derive(Default)]
struct Files(Mutex<HashMap<Uuid, Vec<String>>>);

#[async_trait]
impl FileAccess for Files {
    async fn readable(&self, ctx: &AuthContext, ids: &[Uuid]) -> HashMap<Uuid, FileMeta> {
        let map = self.0.lock().unwrap();
        ids.iter()
            .filter(|id| {
                map.get(id)
                    .is_some_and(|users| users.contains(&ctx.principal().id))
            })
            .map(|id| {
                (
                    *id,
                    FileMeta {
                        name: "給与一覧.xlsx".into(),
                        size_bytes: Some(1234),
                        content_type: None,
                    },
                )
            })
            .collect()
    }
}

/// 受け取った文脈を記録する LLM の代役。
#[derive(Default)]
struct FakeRuntime {
    seen: Mutex<Vec<AgentRequest>>,
    chats: Mutex<Vec<Uuid>>,
    pending: Mutex<Vec<PendingApprovalRef>>,
    /// 回答に付ける社内文書（公開返信に紛れ込んだ場合の扱いを確かめる）。
    sources: Mutex<Vec<Uuid>>,
}

#[async_trait]
impl AgentRuntime for FakeRuntime {
    async fn answer(&self, _ctx: &AuthContext, req: &AgentRequest) -> Result<AgentAnswer, Error> {
        self.seen.lock().unwrap().push(req.clone());
        Ok(AgentAnswer {
            text: format!("要約: {} 件", req.transcript().len()),
            sources: self.sources.lock().unwrap().clone(),
            create_chat: None,
        })
    }

    async fn start_chat(
        &self,
        _ctx: &AuthContext,
        _title: &str,
        req: &AgentRequest,
    ) -> Result<Uuid, Error> {
        self.seen.lock().unwrap().push(req.clone());
        let id = Uuid::new_v4();
        self.chats.lock().unwrap().push(id);
        Ok(id)
    }

    async fn chat_status(
        &self,
        _ctx: &AuthContext,
        _thread: Uuid,
    ) -> Result<(Vec<PendingApprovalRef>, bool), Error> {
        Ok((self.pending.lock().unwrap().clone(), true))
    }
}

struct Env {
    db: PgPool,
    fga: Arc<OpenFgaClient>,
    svc: Arc<MessagingService>,
    files: Arc<Files>,
    tenant: String,
}

impl Env {
    fn ctx(&self, user: &str) -> AuthContext {
        context(&self.tenant, user)
    }

    async fn org_member(&self, user: &str) {
        let ctx = self.ctx(user);
        self.fga
            .write_tuple(
                &ctx.subject(),
                Relation::Member,
                &ctx.ns().organization(ctx.org()),
            )
            .await
            .unwrap();
        sqlx::query(
            "INSERT INTO directory_user (tenant_id, org, user_id, email, display_name) \
             VALUES ($1, 'org', $2, $3, $4) ON CONFLICT DO NOTHING",
        )
        .bind(&self.tenant)
        .bind(user)
        .bind(format!("{user}@example.com"))
        .bind(format!("{user} さん"))
        .execute(&self.db)
        .await
        .unwrap();
    }
}

async fn setup() -> Option<Env> {
    let Ok(url) = shiki_testenv::var("STORAGE_TEST_DATABASE_URL") else {
        return None;
    };
    let Ok(base_url) = shiki_testenv::var("OPENFGA_TEST_URL") else {
        return None;
    };
    let db = PgPoolOptions::new()
        .max_connections(5)
        .connect(&url)
        .await
        .unwrap();
    sqlx::migrate!("../../migrations").run(&db).await.unwrap();
    let fga = Arc::new(
        OpenFgaClient::connect(
            reqwest::Client::new(),
            &OpenFgaConfig {
                base_url,
                store_name: format!("messaging-{}", Uuid::new_v4()),
            },
            &serde_json::from_str(include_str!("../../authz/model/authorization-model.json"))
                .unwrap(),
        )
        .await
        .unwrap(),
    );
    let runtime = PgPoolOptions::new()
        .max_connections(8)
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET ROLE shiki_runtime").execute(conn).await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let files = Arc::new(Files::default());
    let svc = Arc::new(MessagingService::new(
        runtime,
        fga.clone(),
        Hub::local(),
        files.clone(),
    ));
    let env = Env {
        db,
        fga,
        svc,
        files,
        tenant: format!("t{}", Uuid::new_v4().simple()),
    };
    for u in ["alice", "bob", "carol", "dave"] {
        env.org_member(u).await;
    }
    Some(env)
}

fn text(s: &str) -> Vec<ContentBlock> {
    vec![ContentBlock::Text { text: s.into() }]
}

fn msg(s: &str) -> NewMessage {
    NewMessage {
        parent_id: None,
        blocks: text(s),
        agent_private: false,
    }
}

fn create(name: &str, kind: ChannelKind) -> CreateChannel {
    CreateChannel {
        name: name.into(),
        topic: String::new(),
        kind,
        announcement: false,
        members: vec![],
    }
}

fn texts(views: &[messaging::MessageView]) -> Vec<String> {
    views
        .iter()
        .map(|v| messaging::plain_text(&v.blocks))
        .collect()
}

#[tokio::test]
async fn public_channel_lifecycle_and_posting_rules() {
    let Some(env) = setup().await else { return };
    let (alice, bob, carol) = (env.ctx("alice"), env.ctx("bob"), env.ctx("carol"));
    let ch = env
        .svc
        .create_channel(&alice, create("全社", ChannelKind::Public), None)
        .await
        .unwrap();
    assert!(ch.is_admin && ch.can_post && ch.joined);
    // 同じ名前（大文字小文字・空白違い）は作れない（非公開でも一意）。
    let dup = env
        .svc
        .create_channel(&bob, create(" 全社 ", ChannelKind::Private), None)
        .await;
    assert!(matches!(dup, Err(Error::Conflict(_))), "{dup:?}");

    let first = env
        .svc
        .post(&alice, ch.id, msg("おはよう"), None)
        .await
        .unwrap();
    // 参加していない組織メンバーも公開チャンネルを読める（過去の発言も）が、投稿はできない。
    let listed = env.svc.list_messages(&bob, ch.id, None, 50).await.unwrap();
    assert_eq!(texts(&listed), vec!["おはよう"]);
    let denied = env.svc.post(&bob, ch.id, msg("参加前"), None).await;
    assert!(matches!(denied, Err(Error::Forbidden(_))), "{denied:?}");
    // 参加すれば投稿できる。スレッド返信・リアクション。
    env.svc.join(&bob, ch.id, None).await.unwrap();
    let reply = env
        .svc
        .post(
            &bob,
            ch.id,
            NewMessage {
                parent_id: Some(first.id),
                blocks: text("返信"),
                agent_private: false,
            },
            None,
        )
        .await
        .unwrap();
    let thread = env.svc.thread(&carol, ch.id, first.id).await.unwrap();
    assert_eq!(texts(&thread), vec!["おはよう", "返信"]);
    // 返信への返信（入れ子 2 段）はできない。
    let nested = env
        .svc
        .post(
            &bob,
            ch.id,
            NewMessage {
                parent_id: Some(reply.id),
                blocks: text("x"),
                agent_private: false,
            },
            None,
        )
        .await;
    assert!(matches!(nested, Err(Error::Invalid(_))));
    let reacted = env
        .svc
        .react(&bob, first.id, "👍", true, None)
        .await
        .unwrap();
    assert_eq!(reacted.reactions.len(), 1);
    assert!(env
        .svc
        .react(&bob, first.id, "いいね", true, None)
        .await
        .is_err());
    // 編集は本人だけ。他の人には「編集済み」だけが出る。
    assert!(matches!(
        env.svc.edit(&bob, first.id, text("改ざん"), None).await,
        Err(Error::Forbidden(_))
    ));
    let edited = env
        .svc
        .edit(&alice, first.id, text("こんにちは"), None)
        .await
        .unwrap();
    assert!(edited.edited);
    // 編集前の本文は、通常の読取・検索に出ない（管理者閲覧の手続でだけ読める）。
    assert_eq!(messaging::plain_text(&edited.blocks), "こんにちは");
    assert!(env
        .svc
        .search(&bob, "おはよう", 20, None)
        .await
        .unwrap()
        .is_empty());
    // 削除は本人とチャンネルの admin（alice は bob の返信を消せる・bob は alice の発言を消せない）。
    assert!(matches!(
        env.svc.delete(&bob, first.id, None).await,
        Err(Error::Forbidden(_))
    ));
    env.svc.delete(&alice, reply.id, None).await.unwrap();
    let thread = env.svc.thread(&bob, ch.id, first.id).await.unwrap();
    assert_eq!(thread[1].state, MessageState::Deleted);
    assert!(thread[1].blocks.is_empty(), "削除後の本文を返さない");
    assert!(
        env.svc
            .search(&bob, "返信", 20, None)
            .await
            .unwrap()
            .is_empty(),
        "削除した発言は検索に出ない"
    );
    // 最後の admin は退出できない。
    assert!(matches!(
        env.svc.remove_member(&alice, ch.id, "alice", None).await,
        Err(Error::Forbidden(_))
    ));
    // アーカイブ中は読み取り専用。
    env.svc
        .set_archived(&alice, ch.id, true, None)
        .await
        .unwrap();
    assert!(matches!(
        env.svc.post(&alice, ch.id, msg("x"), None).await,
        Err(Error::Forbidden(_))
    ));
    // org 管理者でもチャンネルの admin は継承しない（管理操作は拒否）。
    let dave = env.ctx("dave");
    env.fga
        .write_tuple(
            &dave.subject(),
            Relation::Admin,
            &dave.ns().organization(dave.org()),
        )
        .await
        .unwrap();
    assert!(matches!(
        env.svc.set_archived(&dave, ch.id, false, None).await,
        Err(Error::Forbidden(_))
    ));
}

#[tokio::test]
async fn private_channel_hides_history_before_join_on_every_path() {
    let Some(env) = setup().await else { return };
    let (alice, bob, carol) = (env.ctx("alice"), env.ctx("bob"), env.ctx("carol"));
    let ch = env
        .svc
        .create_channel(&alice, create("人事", ChannelKind::Private), None)
        .await
        .unwrap();
    let old = env
        .svc
        .post(&alice, ch.id, msg("極秘の人事案"), None)
        .await
        .unwrap();
    // 非メンバーには存在ごと見えない（一覧・検索・スレッド・投稿）。
    assert!(matches!(
        env.svc.list_messages(&carol, ch.id, None, 50).await,
        Err(Error::NotFound)
    ));
    assert!(env
        .svc
        .search(&carol, "極秘", 20, None)
        .await
        .unwrap()
        .is_empty());
    assert!(!env
        .svc
        .browse_public(&carol, "", false)
        .await
        .unwrap()
        .iter()
        .any(|c| c.id == ch.id));
    // 非公開チャンネルの招待は admin だけ。
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    env.svc
        .invite(&alice, ch.id, vec!["bob".into()], None)
        .await
        .unwrap();
    env.svc
        .post(&alice, ch.id, msg("参加後の発言"), None)
        .await
        .unwrap();
    assert!(matches!(
        env.svc
            .invite(&bob, ch.id, vec!["carol".into()], None)
            .await,
        Err(Error::Forbidden(_))
    ));
    // 一覧・スレッド・検索のいずれでも、参加前の発言は読めない。
    let listed = env.svc.list_messages(&bob, ch.id, None, 50).await.unwrap();
    assert_eq!(texts(&listed), vec!["参加後の発言"]);
    assert!(matches!(
        env.svc.thread(&bob, ch.id, old.id).await,
        Err(Error::NotFound)
    ));
    assert!(env
        .svc
        .search(&bob, "極秘", 20, None)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        env.svc
            .search(&bob, "参加後", 20, None)
            .await
            .unwrap()
            .len(),
        1
    );
    // AI の文脈にも入らない（bob が自分のエージェントを呼ぶ）。
    let rt = Arc::new(FakeRuntime::default());
    let runtime: Arc<dyn AgentRuntime> = rt.clone();
    let trigger = env
        .svc
        .post(
            &bob,
            ch.id,
            NewMessage {
                parent_id: None,
                blocks: vec![
                    ContentBlock::Mention {
                        mention: Mention::Agent,
                    },
                    ContentBlock::Text {
                        text: " これまでの流れを要約して".into(),
                    },
                ],
                agent_private: false,
            },
            None,
        )
        .await
        .unwrap();
    env.svc
        .invoke_agent(&bob, &runtime, trigger.id, ReplyMode::Public, None)
        .await
        .unwrap();
    let seen = rt.seen.lock().unwrap().clone();
    let transcript: Vec<String> = seen[0]
        .transcript()
        .iter()
        .map(|l| l.text.clone())
        .collect();
    assert!(
        transcript.iter().all(|t| !t.contains("極秘")),
        "{transcript:?}"
    );
    assert!(
        transcript.iter().any(|t| t.contains("参加後")),
        "{transcript:?}"
    );
    // 公開返信はスレッドに書かれ、書き手は「bob のエージェント」。
    let replies = env.svc.thread(&alice, ch.id, trigger.id).await.unwrap();
    let answer = replies.last().unwrap();
    assert_eq!(answer.author.kind, messaging::AuthorKind::Assistant);
    assert_eq!(answer.author.owner_name.as_deref(), Some("bob さん"));
    // 他人（alice）が bob の発言でエージェントを呼ぶことはできない（持ち主だけが呼べる）。
    let other = env
        .svc
        .invoke_agent(&alice, &runtime, trigger.id, ReplyMode::Public, None)
        .await;
    assert!(matches!(other, Err(Error::Forbidden(_))), "{other:?}");
    // 退出すると読めず、検索にも出ない（剥奪の伝播・PIT-64）。
    env.svc
        .remove_member(&alice, ch.id, "bob", None)
        .await
        .unwrap();
    assert!(matches!(
        env.svc.list_messages(&bob, ch.id, None, 50).await,
        Err(Error::NotFound)
    ));
    assert!(env
        .svc
        .search(&bob, "参加後", 20, None)
        .await
        .unwrap()
        .is_empty());
    // pre-filter を壊して全チャンネルを渡しても、post-filter だけで守られる（片系故障）。
    let broken = env
        .svc
        .search_in(&bob, "参加後", &[ch.id], 20, None)
        .await
        .unwrap();
    assert!(broken.is_empty(), "post-filter が権限を守ること");
    // 再参加すると境界は入り直しの時刻に引き直される。
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    env.svc
        .invite(&alice, ch.id, vec!["bob".into()], None)
        .await
        .unwrap();
    assert!(env
        .svc
        .list_messages(&bob, ch.id, None, 50)
        .await
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn dm_is_one_to_one_idempotent_and_cannot_be_left() {
    let Some(env) = setup().await else { return };
    let (alice, bob, carol) = (env.ctx("alice"), env.ctx("bob"), env.ctx("carol"));
    let a = env.svc.open_dm(&alice, "bob", None).await.unwrap();
    let b = env.svc.open_dm(&bob, "alice", None).await.unwrap();
    assert_eq!(a.id, b.id, "同じ 2 人の DM は 1 つ");
    assert_eq!(a.kind, ChannelKind::Dm);
    assert_eq!(a.name, "bob さん");
    env.svc
        .post(&alice, a.id, msg("内緒の話"), None)
        .await
        .unwrap();
    assert!(matches!(
        env.svc.list_messages(&carol, a.id, None, 10).await,
        Err(Error::NotFound)
    ));
    assert!(matches!(
        env.svc
            .invite(&alice, a.id, vec!["carol".into()], None)
            .await,
        Err(Error::Forbidden(_))
    ));
    assert!(matches!(
        env.svc.remove_member(&alice, a.id, "alice", None).await,
        Err(Error::Forbidden(_))
    ));
    // 非表示にしても、新しい発言が来たら一覧に戻る。
    env.svc.hide_dm(&bob, a.id).await.unwrap();
    assert!(!env
        .svc
        .list_channels(&bob)
        .await
        .unwrap()
        .iter()
        .any(|c| c.id == a.id));
    env.svc.post(&alice, a.id, msg("見て"), None).await.unwrap();
    let listed = env.svc.list_channels(&bob).await.unwrap();
    let dm = listed.iter().find(|c| c.id == a.id).unwrap();
    assert_eq!(dm.unread, 2);
    // 組織のメンバーでない相手とは DM を開けない。
    assert!(env.svc.open_dm(&alice, "mallory", None).await.is_err());
}

#[tokio::test]
async fn mentions_reach_only_channel_members() {
    let Some(env) = setup().await else { return };
    let (alice, bob, carol) = (env.ctx("alice"), env.ctx("bob"), env.ctx("carol"));
    let ch = env
        .svc
        .create_channel(
            &alice,
            CreateChannel {
                members: vec!["bob".into()],
                ..create("開発", ChannelKind::Private)
            },
            None,
        )
        .await
        .unwrap();
    // ロール「営業」に bob と carol が入っているが、carol は参加していない。
    for u in ["bob", "carol"] {
        env.fga
            .write_tuple(
                &alice.ns().user(u),
                Relation::Member,
                &alice.ns().role("sales"),
            )
            .await
            .unwrap();
    }
    let blocks = vec![
        ContentBlock::Mention {
            mention: Mention::User {
                user_id: "carol".into(),
            },
        },
        ContentBlock::Mention {
            mention: Mention::Role {
                role_id: "sales".into(),
            },
        },
        ContentBlock::Text {
            text: " 見てください".into(),
        },
    ];
    env.svc
        .post(
            &alice,
            ch.id,
            NewMessage {
                parent_id: None,
                blocks,
                agent_private: false,
            },
            None,
        )
        .await
        .unwrap();
    let bob_notices = env.svc.mentions(&bob, true).await.unwrap();
    assert_eq!(bob_notices.len(), 1);
    assert_eq!(bob_notices[0].via, "role");
    assert!(
        env.svc.mentions(&carol, false).await.unwrap().is_empty(),
        "参加していない人には通知しない（非公開チャンネルの存在を漏らさない）"
    );
    // ミュート（なし）にすると通知されない。
    env.svc
        .set_notify(&bob, ch.id, messaging::NotifyLevel::None)
        .await
        .unwrap();
    env.svc
        .post(
            &alice,
            ch.id,
            NewMessage {
                parent_id: None,
                blocks: vec![
                    ContentBlock::Mention {
                        mention: Mention::Channel,
                    },
                    ContentBlock::Text {
                        text: " 全員".into(),
                    },
                ],
                agent_private: false,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(env.svc.mentions(&bob, true).await.unwrap().len(), 1);
}

#[tokio::test]
async fn file_refs_are_resolved_per_viewer() {
    let Some(env) = setup().await else { return };
    let (alice, bob) = (env.ctx("alice"), env.ctx("bob"));
    let ch = env
        .svc
        .create_channel(
            &alice,
            CreateChannel {
                members: vec!["bob".into()],
                ..create("経理", ChannelKind::Private)
            },
            None,
        )
        .await
        .unwrap();
    let file = Uuid::new_v4();
    env.files
        .0
        .lock()
        .unwrap()
        .insert(file, vec!["alice".into(), "bob".into()]);
    let posted = env
        .svc
        .post(
            &alice,
            ch.id,
            NewMessage {
                parent_id: None,
                blocks: vec![ContentBlock::FileRef {
                    node_id: file.to_string(),
                    name: "偽の名前.xlsx".into(),
                }],
                agent_private: false,
            },
            None,
        )
        .await
        .unwrap();
    let seen = env.svc.message(&bob, posted.id).await.unwrap();
    assert!(seen.files[0].available);
    assert_eq!(seen.files[0].name.as_deref(), Some("給与一覧.xlsx"));
    // 共有後に権限を絞ると、過去の発言からも見えなくなる（名前・サイズ・node_id を出さない）。
    env.files
        .0
        .lock()
        .unwrap()
        .insert(file, vec!["alice".into()]);
    let seen = env.svc.message(&bob, posted.id).await.unwrap();
    assert!(!seen.files[0].available);
    assert!(seen.files[0].name.is_none() && seen.files[0].size_bytes.is_none());
    assert_eq!(
        seen.blocks[0],
        ContentBlock::FileRef {
            node_id: String::new(),
            name: String::new()
        }
    );
}

#[tokio::test]
async fn admin_access_requires_an_approved_grant_for_dms() {
    let Some(env) = setup().await else { return };
    let (alice, bob, carol, dave) = (
        env.ctx("alice"),
        env.ctx("bob"),
        env.ctx("carol"),
        env.ctx("dave"),
    );
    // carol = 監査担当者、dave = 組織の管理者（閲覧権は無い）、alice = 承認者を設定する管理者。
    env.fga
        .write_tuple(
            &carol.subject(),
            Relation::AuditViewer,
            &carol.ns().organization(carol.org()),
        )
        .await
        .unwrap();
    for u in [&dave, &alice] {
        env.fga
            .write_tuple(&u.subject(), Relation::Admin, &u.ns().organization(u.org()))
            .await
            .unwrap();
    }
    let dm = env.svc.open_dm(&alice, "bob", None).await.unwrap();
    let posted = env
        .svc
        .post(&alice, dm.id, msg("DM の本文"), None)
        .await
        .unwrap();
    env.svc
        .edit(&alice, posted.id, text("書き直した本文"), None)
        .await
        .unwrap();
    let range = (
        chrono::Utc::now() - chrono::Duration::days(1),
        chrono::Utc::now() + chrono::Duration::days(1),
    );
    // 管理者ロールでも監査担当者でも、承認レコード無しでは DM を読めない。
    for who in [&dave, &carol] {
        assert!(matches!(
            env.svc
                .admin_read(who, dm.id, range.0, range.1, false, None)
                .await,
            Err(Error::Forbidden(_))
        ));
        // 通常の読取経路にも出ない（PIT-65）。
        assert!(env
            .svc
            .search(who, "本文", 20, None)
            .await
            .unwrap()
            .is_empty());
    }
    let req = AccessRequest {
        channel_id: dm.id,
        reason: "情報漏えいの調査".into(),
        messages_from: range.0,
        messages_until: range.1,
        valid_days: None,
    };
    assert!(matches!(
        env.svc.request_access(&dave, req.clone(), None).await,
        Err(Error::Forbidden(_))
    ));
    let grant = env.svc.request_access(&carol, req, None).await.unwrap();
    // 承認者が未指名なら誰も承認できない（fail-closed）。
    assert!(matches!(
        env.svc.decide_access(&alice, grant.id, true, None).await,
        Err(Error::Forbidden(_))
    ));
    let mut settings = env.svc.settings(&alice).await.unwrap();
    settings.access_approvers = vec!["carol".into(), "dave".into()];
    env.svc
        .update_settings(&alice, settings, None)
        .await
        .unwrap();
    // 申請者本人は承認できない。
    assert!(matches!(
        env.svc.decide_access(&carol, grant.id, true, None).await,
        Err(Error::Forbidden(_))
    ));
    env.svc
        .decide_access(&dave, grant.id, true, None)
        .await
        .unwrap();
    // 承認された申請者だけが期間内に読める（編集前の本文も含む）。
    assert!(env
        .svc
        .admin_read(&dave, dm.id, range.0, range.1, false, None)
        .await
        .is_err());
    let read = env
        .svc
        .admin_read(&carol, dm.id, range.0, range.1, true, None)
        .await
        .unwrap();
    assert_eq!(read.len(), 1);
    assert_eq!(read[0].revisions.len(), 1, "編集前の本文が残る");
    // 期間を過ぎると失敗する。
    sqlx::query(
        "UPDATE messaging_access_grant SET valid_until = now() - interval '1 second' \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&env.tenant)
    .bind(grant.id)
    .execute(&env.db)
    .await
    .unwrap();
    assert!(matches!(
        env.svc
            .admin_read(&carol, dm.id, range.0, range.1, true, None)
            .await,
        Err(Error::Forbidden(_))
    ));
    // 承認者を未指名に戻すと、期間内の承認があっても閲覧できない（fail-closed）。
    let grant2 = env
        .svc
        .request_access(
            &carol,
            AccessRequest {
                channel_id: dm.id,
                reason: "再調査".into(),
                messages_from: range.0,
                messages_until: range.1,
                valid_days: None,
            },
            None,
        )
        .await
        .unwrap();
    env.svc
        .decide_access(&dave, grant2.id, true, None)
        .await
        .unwrap();
    assert!(env
        .svc
        .admin_read(&carol, dm.id, range.0, range.1, false, None)
        .await
        .is_ok());
    let mut s = env.svc.settings(&alice).await.unwrap();
    s.access_approvers = vec![];
    env.svc.update_settings(&alice, s, None).await.unwrap();
    assert!(matches!(
        env.svc
            .admin_read(&carol, dm.id, range.0, range.1, false, None)
            .await,
        Err(Error::Forbidden(_))
    ));
    // 承認の範囲を超える期間は黙って縮めずに失敗する。
    let mut s = env.svc.settings(&alice).await.unwrap();
    s.access_approvers = vec!["dave".into()];
    env.svc.update_settings(&alice, s, None).await.unwrap();
    assert!(matches!(
        env.svc
            .admin_read(
                &carol,
                dm.id,
                range.0 - chrono::Duration::days(3),
                range.1,
                false,
                None
            )
            .await,
        Err(Error::Forbidden(_))
    ));
    // 全操作が監査に残る。
    let actions: Vec<String> = sqlx::query_scalar(
        "SELECT action FROM audit_log WHERE tenant_id = $1 AND action LIKE 'messaging.%' ORDER BY id",
    )
    .bind(&env.tenant)
    .fetch_all(&env.db)
    .await
    .unwrap();
    for a in [
        "messaging.access.request",
        "messaging.access.approve",
        "messaging.admin.export",
        "messaging.deny",
    ] {
        assert!(actions.iter().any(|x| x == a), "{a} が無い: {actions:?}");
    }
    let _ = bob;
}

#[tokio::test]
async fn retention_cannot_undercut_audit_and_purges_search() {
    let Some(env) = setup().await else { return };
    let alice = env.ctx("alice");
    env.fga
        .write_tuple(
            &alice.subject(),
            Relation::Admin,
            &alice.ns().organization(alice.org()),
        )
        .await
        .unwrap();
    let mut s = env.svc.settings(&alice).await.unwrap();
    assert_eq!(s.retention_days, None, "既定は無期限");
    s.retention_days = Some(s.audit_retention_days - 1);
    assert!(matches!(
        env.svc.update_settings(&alice, s.clone(), None).await,
        Err(Error::Invalid(_))
    ));
    s.retention_days = Some(s.audit_retention_days);
    let s = env.svc.update_settings(&alice, s, None).await.unwrap();
    let ch = env
        .svc
        .create_channel(&alice, create("古い話", ChannelKind::Public), None)
        .await
        .unwrap();
    let old = env
        .svc
        .post(&alice, ch.id, msg("昔の決定事項"), None)
        .await
        .unwrap();
    // 編集して改訂を作っておく（期限切れで改訂も消えることを確かめるため）。
    env.svc
        .edit(&alice, old.id, text("昔の決定事項（修正）"), None)
        .await
        .unwrap();
    let before: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM channel_message_revision WHERE tenant_id = $1 AND message_id = $2",
    )
    .bind(&env.tenant)
    .bind(old.id)
    .fetch_one(&env.db)
    .await
    .unwrap();
    assert!(before >= 1, "編集で改訂が残る");
    sqlx::query(
        "UPDATE channel_message SET created_at = now() - make_interval(days => $3) \
         WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&env.tenant)
    .bind(old.id)
    .bind(s.retention_days.unwrap() + 1)
    .execute(&env.db)
    .await
    .unwrap();
    let n = messaging::retention::sweep_org(
        &env.db,
        alice.tenant(),
        alice.org(),
        s.retention_days.unwrap(),
    )
    .await
    .unwrap();
    assert_eq!(n, 1);
    let view = env.svc.message(&alice, old.id).await.unwrap();
    assert_eq!(view.state, MessageState::Expired);
    assert!(view.blocks.is_empty(), "期限切れの本文を返さない");
    assert!(env
        .svc
        .search(&alice, "決定事項", 20, None)
        .await
        .unwrap()
        .is_empty());
    // 本文・索引・改訂そのものが消える（検索・対話文脈・管理者閲覧のどの経路にも出せない）。
    let (blocks, search_text): (serde_json::Value, Option<String>) = sqlx::query_as(
        "SELECT blocks, search_text FROM channel_message WHERE tenant_id = $1 AND id = $2",
    )
    .bind(&env.tenant)
    .bind(old.id)
    .fetch_one(&env.db)
    .await
    .unwrap();
    assert_eq!(blocks, serde_json::json!([]));
    assert_eq!(search_text, None);
    let revisions: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM channel_message_revision WHERE tenant_id = $1 AND message_id = $2",
    )
    .bind(&env.tenant)
    .bind(old.id)
    .fetch_one(&env.db)
    .await
    .unwrap();
    assert_eq!(revisions, 0);
}

#[tokio::test]
async fn agent_creates_chat_and_notifies_owner_by_dm() {
    let Some(env) = setup().await else { return };
    let alice = env.ctx("alice");
    let ch = env
        .svc
        .create_channel(&alice, create("調査", ChannelKind::Public), None)
        .await
        .unwrap();
    let rt = Arc::new(FakeRuntime::default());
    rt.pending.lock().unwrap().push(PendingApprovalRef {
        approval_id: "a1".into(),
        tool_name: "drive.delete".into(),
    });
    let runtime: Arc<dyn AgentRuntime> = rt.clone();
    let trigger = env
        .svc
        .post(
            &alice,
            ch.id,
            NewMessage {
                parent_id: None,
                blocks: vec![
                    ContentBlock::Mention {
                        mention: Mention::Agent,
                    },
                    ContentBlock::Text {
                        text: " チャットで競合調査をまとめて".into(),
                    },
                ],
                agent_private: false,
            },
            None,
        )
        .await
        .unwrap();
    env.svc
        .invoke_agent(&alice, &runtime, trigger.id, ReplyMode::Public, None)
        .await
        .unwrap();
    assert_eq!(rt.chats.lock().unwrap().len(), 1);
    // チャットの案内は本人だけに見える（チャンネルには書かない）。
    let replies = env.svc.agent_replies(&alice, ch.id).await.unwrap();
    assert!(replies[0].chat_thread_id.is_some());
    assert!(env
        .svc
        .agent_replies(&env.ctx("bob"), ch.id)
        .await
        .unwrap()
        .is_empty());
    // 人の承認が要る操作は、本人とエージェントの DM に通知が届く（見張りを 1 周回す）。
    assert_eq!(env.svc.watch_approvals_once(&runtime).await.unwrap(), 1);
    // 同じ承認待ちを二度知らせない。
    assert_eq!(env.svc.watch_approvals_once(&runtime).await.unwrap(), 0);
    let chans = env.svc.list_channels(&alice).await.unwrap();
    let dm = chans
        .iter()
        .find(|c| c.agent_dm)
        .expect("エージェントとの DM");
    let msgs = env
        .svc
        .list_messages(&alice, dm.id, None, 10)
        .await
        .unwrap();
    let found = texts(&msgs).iter().any(|t| t.contains("drive.delete"));
    assert!(found, "承認の通知が DM に届くこと");
}

#[tokio::test]
async fn agent_profile_rejects_impersonation() {
    let Some(env) = setup().await else { return };
    let alice = env.ctx("alice");
    let p = env.svc.agent_profile(&alice).await.unwrap();
    assert!(p.is_default && p.name == "シキ");
    for bad in ["シキ", "管理者", "bob さん", "ＢＯＢ さん"] {
        let r = env
            .svc
            .set_agent_profile(
                &alice,
                messaging::AgentProfileUpdate {
                    name: Some(bad.into()),
                    icon: None,
                },
                None,
            )
            .await;
        assert!(matches!(r, Err(Error::Invalid(_))), "{bad}: {r:?}");
    }
    let p = env
        .svc
        .set_agent_profile(
            &alice,
            messaging::AgentProfileUpdate {
                name: Some("ミケ".into()),
                icon: None,
            },
            None,
        )
        .await
        .unwrap();
    assert_eq!(p.name, "ミケ");
    let p = env
        .svc
        .set_agent_profile(
            &alice,
            messaging::AgentProfileUpdate {
                name: Some(String::new()),
                icon: None,
            },
            None,
        )
        .await
        .unwrap();
    assert!(p.is_default);
}

#[tokio::test]
async fn same_channel_id_in_another_tenant_never_crosses() {
    let Some(env) = setup().await else { return };
    let alice = env.ctx("alice");
    let ch = env
        .svc
        .create_channel(&alice, create("越境", ChannelKind::Public), None)
        .await
        .unwrap();
    env.svc
        .post(&alice, ch.id, msg("テナント A の発言"), None)
        .await
        .unwrap();
    // 別テナントの利用者（同じ local id・同じ org 名）は、同じ channel_id でも読めない。
    let other = context(&format!("t{}", Uuid::new_v4().simple()), "alice");
    env.fga
        .write_tuple(
            &other.subject(),
            Relation::Member,
            &other.ns().organization(other.org()),
        )
        .await
        .unwrap();
    assert!(matches!(
        env.svc.list_messages(&other, ch.id, None, 10).await,
        Err(Error::NotFound)
    ));
    // 配信の合図もテナントを越えない（同じ channel_id の購読でも受けない）。
    let mut theirs = env.svc.hub().subscribe(other.tenant_id(), ch.id);
    let mut ours = env.svc.hub().subscribe(alice.tenant_id(), ch.id);
    env.svc
        .post(&alice, ch.id, msg("もう一度"), None)
        .await
        .unwrap();
    assert_eq!(ours.recv().await, Some(Signal::Changed));
    let crossed = tokio::time::timeout(std::time::Duration::from_millis(300), theirs.recv()).await;
    assert!(crossed.is_err(), "別テナントの購読へ合図が届かない");
}

#[tokio::test]
async fn review_regressions_stay_fixed() {
    let Some(env) = setup().await else { return };
    let (alice, bob, carol) = (env.ctx("alice"), env.ctx("bob"), env.ctx("carol"));
    // 公開チャンネルを退出した人の検索に、そのチャンネルの発言を出さない（org_reader で読めても）。
    let pubch = env
        .svc
        .create_channel(&alice, create("雑談", ChannelKind::Public), None)
        .await
        .unwrap();
    env.svc.join(&bob, pubch.id, None).await.unwrap();
    let mine = env
        .svc
        .post(&bob, pubch.id, msg("退出前の発言"), None)
        .await
        .unwrap();
    env.svc
        .remove_member(&bob, pubch.id, "bob", None)
        .await
        .unwrap();
    let hits = env
        .svc
        .search_in(&bob, "退出前", &[pubch.id], 20, None)
        .await
        .unwrap();
    assert!(hits.is_empty(), "退出した公開チャンネルは検索の対象外");
    // 投稿権を失った人は、自分の過去の発言も書き換えられない。
    assert!(matches!(
        env.svc.edit(&bob, mine.id, text("書き換え"), None).await,
        Err(Error::Forbidden(_))
    ));
    // 役割の変更で入力が不正なら、権限は何も変わらない（admin だけ付いて失敗、を作らない）。
    let r = env
        .svc
        .set_member_role(&alice, pubch.id, "alice", Some(true), Some(true), None)
        .await;
    assert!(matches!(r, Err(Error::Invalid(_))));
    env.svc.join(&carol, pubch.id, None).await.unwrap();
    let r = env
        .svc
        .set_member_role(&alice, pubch.id, "carol", Some(true), Some(true), None)
        .await;
    assert!(matches!(r, Err(Error::Invalid(_))));
    let members = env.svc.members(&alice, pubch.id).await.unwrap();
    assert!(!members.iter().any(|m| m.user_id == "carol" && m.is_admin));
    // アーカイブ中は削除もできない（読み取り専用）。
    let keep = env
        .svc
        .post(&alice, pubch.id, msg("残す"), None)
        .await
        .unwrap();
    env.svc
        .set_archived(&alice, pubch.id, true, None)
        .await
        .unwrap();
    assert!(matches!(
        env.svc.delete(&alice, keep.id, None).await,
        Err(Error::Forbidden(_))
    ));

    // 非公開チャンネルの公開返信は、後から参加した人の境界より前の発言を文脈に入れない。
    let ch = env
        .svc
        .create_channel(&alice, create("経営会議", ChannelKind::Private), None)
        .await
        .unwrap();
    env.svc
        .post(&alice, ch.id, msg("昔の機密の議論"), None)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    env.svc
        .invite(&alice, ch.id, vec!["bob".into()], None)
        .await
        .unwrap();
    let rt = Arc::new(FakeRuntime::default());
    let runtime: Arc<dyn AgentRuntime> = rt.clone();
    let trigger = env
        .svc
        .post(
            &alice,
            ch.id,
            NewMessage {
                parent_id: None,
                blocks: vec![
                    ContentBlock::Mention {
                        mention: Mention::Agent,
                    },
                    ContentBlock::Text {
                        text: " 今までを要約して".into(),
                    },
                ],
                agent_private: false,
            },
            None,
        )
        .await
        .unwrap();
    env.svc
        .invoke_agent(&alice, &runtime, trigger.id, ReplyMode::Public, None)
        .await
        .unwrap();
    let seen = rt.seen.lock().unwrap().clone();
    assert!(
        seen[0]
            .transcript()
            .iter()
            .all(|l| !l.text.contains("機密")),
        "公開返信の文脈に、参加者の一部が読めない発言を入れない"
    );
    // 呼び出しの発言そのものが後から参加した人の境界より前なら、公開返信しない。
    let early = env
        .svc
        .post(
            &alice,
            ch.id,
            NewMessage {
                parent_id: None,
                blocks: vec![
                    ContentBlock::Mention {
                        mention: Mention::Agent,
                    },
                    ContentBlock::Text {
                        text: " 極秘の件を要約".into(),
                    },
                ],
                agent_private: false,
            },
            None,
        )
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    env.svc
        .invite(&alice, ch.id, vec!["carol".into()], None)
        .await
        .unwrap();
    assert!(matches!(
        env.svc
            .invoke_agent(&alice, &runtime, early.id, ReplyMode::Public, None)
            .await,
        Err(Error::Forbidden(_))
    ));
    // 自分だけに表示なら、照会者が読める発言を使ってよい。
    let trigger2 = env
        .svc
        .post(
            &alice,
            ch.id,
            NewMessage {
                parent_id: None,
                blocks: vec![
                    ContentBlock::Mention {
                        mention: Mention::Agent,
                    },
                    ContentBlock::Text {
                        text: " 今までを要約して（自分用）".into(),
                    },
                ],
                agent_private: true,
            },
            None,
        )
        .await
        .unwrap();
    env.svc
        .invoke_agent(&alice, &runtime, trigger2.id, ReplyMode::Private, None)
        .await
        .unwrap();
    let seen = rt.seen.lock().unwrap().clone();
    assert!(seen[1].transcript().iter().any(|l| l.text.contains("機密")));
}

/// 参加の境界は「挿入した順」で決まる（PIT-63）。投稿のトランザクションが参加より先に始まっていても、
/// 参加の確定後に挿入した発言は見える。参加と同じ時刻の発言は参加前として扱う。
#[tokio::test]
async fn join_boundary_follows_insert_order_and_ties_stay_hidden() {
    let Some(env) = setup().await else { return };
    let (alice, bob) = (env.ctx("alice"), env.ctx("bob"));
    let ch = env
        .svc
        .create_channel(&alice, create("境界", ChannelKind::Private), None)
        .await
        .unwrap();

    // 投稿のトランザクションを先に始め（now() はここで固定される）、その後に bob の参加を確定させる。
    let mut posting = env.db.begin().await.unwrap();
    let started: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("SELECT now()")
        .fetch_one(&mut *posting)
        .await
        .unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    env.svc
        .invite(&alice, ch.id, vec!["bob".into()], None)
        .await
        .unwrap();
    let blocks = text("参加の確定後に挿入した発言");
    let at = messaging::store_message::insert_message(
        &mut posting,
        &alice,
        &messaging::store_message::InsertMessage {
            id: Uuid::new_v4(),
            channel_id: ch.id,
            parent_id: None,
            author_kind: "user",
            author_id: "alice",
            blocks: &blocks,
            search_text: "参加の確定後に挿入した発言",
        },
    )
    .await
    .unwrap();
    posting.commit().await.unwrap();
    assert!(
        at > started,
        "発言の時刻はトランザクション開始ではなく挿入の瞬間"
    );
    let listed = env.svc.list_messages(&bob, ch.id, None, 50).await.unwrap();
    assert_eq!(texts(&listed), vec!["参加の確定後に挿入した発言"]);

    // 参加と同じ時刻の発言は見えない（同値は参加前に倒す）。
    let joined: chrono::DateTime<chrono::Utc> = sqlx::query_scalar(
        "SELECT visible_from FROM channel_member WHERE tenant_id = $1 AND channel_id = $2 AND user_id = 'bob'",
    )
    .bind(&env.tenant)
    .bind(ch.id)
    .fetch_one(&env.db)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO channel_message (tenant_id, org, id, channel_id, author_kind, author_id, blocks, \
            search_text, created_at, updated_at) VALUES ($1, 'org', $2, $3, 'user', 'alice', $4, '同時刻', $5, $5)",
    )
    .bind(&env.tenant)
    .bind(Uuid::new_v4())
    .bind(ch.id)
    .bind(sqlx::types::Json(text("同時刻")))
    .bind(joined)
    .execute(&env.db)
    .await
    .unwrap();
    let listed = env.svc.list_messages(&bob, ch.id, None, 50).await.unwrap();
    assert_eq!(texts(&listed), vec!["参加の確定後に挿入した発言"]);
    assert!(env
        .svc
        .search(&bob, "同時刻", 20, None)
        .await
        .unwrap()
        .is_empty());
}

/// 役割変更はチャンネルのロックと同じ接続で行う。小さいプールで同じチャンネルへ同時に投げても、
/// ロック待ちが接続を使い切って互いに待つことがない（全件が期限内に終わる）。
#[tokio::test]
async fn concurrent_role_changes_do_not_exhaust_a_small_pool() {
    let Some(env) = setup().await else { return };
    let alice = env.ctx("alice");
    let ch = env
        .svc
        .create_channel(&alice, create("役割の同時変更", ChannelKind::Public), None)
        .await
        .unwrap();
    env.svc
        .invite(&alice, ch.id, vec!["bob".into(), "carol".into()], None)
        .await
        .unwrap();
    let url = shiki_testenv::var("STORAGE_TEST_DATABASE_URL").unwrap();
    let small = PgPoolOptions::new()
        .max_connections(2)
        .acquire_timeout(std::time::Duration::from_secs(5))
        .after_connect(|conn, _| {
            Box::pin(async move {
                sqlx::query("SET ROLE shiki_runtime").execute(conn).await?;
                Ok(())
            })
        })
        .connect(&url)
        .await
        .unwrap();
    let svc = Arc::new(MessagingService::new(
        small,
        env.fga.clone(),
        Hub::local(),
        env.files.clone(),
    ));
    // 管理者の役割変更と、管理権の無い dave（参加者）の役割変更（拒否・拒否の監査は別の接続）を混ぜる。
    env.svc
        .invite(&alice, ch.id, vec!["dave".into()], None)
        .await
        .unwrap();
    let dave = env.ctx("dave");
    let mut tasks = Vec::new();
    for i in 0..8 {
        let svc = svc.clone();
        let (ctx, denied) = if i % 4 == 3 {
            (dave.clone(), true)
        } else {
            (alice.clone(), false)
        };
        let user = if i % 2 == 0 { "bob" } else { "carol" };
        tasks.push(tokio::spawn(async move {
            let r = svc
                .set_member_role(&ctx, ch.id, user, Some(i % 4 < 2), None, None)
                .await;
            (denied, r)
        }));
    }
    let all = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        futures::future::join_all(tasks),
    )
    .await
    .expect("役割変更が詰まらずに終わる");
    for r in all {
        let (denied, r) = r.unwrap();
        if denied {
            assert!(matches!(r, Err(Error::Forbidden(_))), "{r:?}");
        } else {
            r.unwrap();
        }
    }
    let denies: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action = 'messaging.deny' AND actor = 'dave'",
    )
    .bind(&env.tenant)
    .fetch_one(&env.db)
    .await
    .unwrap();
    assert_eq!(denies, 2, "拒否はどちらも監査に残る");
    // ロックが外れていること（後続の操作が待たされない）。
    tokio::time::timeout(
        std::time::Duration::from_secs(5),
        svc.set_member_role(&alice, ch.id, "bob", Some(false), None, None),
    )
    .await
    .expect("ロックが残っていない")
    .unwrap();
}

/// 公開返信の根拠は、チャンネルの発言とチャンネルに共有した資料だけ。実行部が共有されていない社内文書を
/// 根拠に返してきても、参加者には公開しない（本人だけに表示する）。
#[tokio::test]
async fn public_reply_is_grounded_on_channel_shared_documents_only() {
    let Some(env) = setup().await else { return };
    let (alice, bob) = (env.ctx("alice"), env.ctx("bob"));
    let ch = env
        .svc
        .create_channel(&alice, create("根拠", ChannelKind::Public), None)
        .await
        .unwrap();
    env.svc.join(&bob, ch.id, None).await.unwrap();
    let rt = Arc::new(FakeRuntime::default());
    rt.sources.lock().unwrap().push(Uuid::new_v4());
    let runtime: Arc<dyn AgentRuntime> = rt.clone();
    let trigger = env
        .svc
        .post(
            &bob,
            ch.id,
            NewMessage {
                parent_id: None,
                blocks: vec![
                    ContentBlock::Mention {
                        mention: Mention::Agent,
                    },
                    ContentBlock::Text {
                        text: " 規程を教えて".into(),
                    },
                ],
                agent_private: false,
            },
            None,
        )
        .await
        .unwrap();
    env.svc
        .invoke_agent(&bob, &runtime, trigger.id, ReplyMode::Public, None)
        .await
        .unwrap();
    // チャンネル（スレッド）にエージェントの発言は書かれない。
    let thread = env.svc.thread(&alice, ch.id, trigger.id).await.unwrap();
    assert!(
        thread
            .iter()
            .all(|m| m.author.kind != messaging::AuthorKind::Assistant),
        "{thread:?}"
    );
    // 本人だけに表示の回答として残る（他の参加者には見えない）。
    assert_eq!(env.svc.agent_replies(&bob, ch.id).await.unwrap().len(), 1);
    assert!(env
        .svc
        .agent_replies(&alice, ch.id)
        .await
        .unwrap()
        .is_empty());

    // 公開チャンネルは、参加していない組織メンバーも公開返信を読める。チャンネルに共有しただけの資料
    // （組織全体は読めない）は根拠にできず、組織全体にも読める資料なら公開返信にする。
    let shared = Uuid::new_v4();
    let shared_file = bob.ns().file(&shared.to_string());
    env.fga
        .write_tuple(
            &bob.ns().channel_member(&ch.id.to_string()),
            Relation::Viewer,
            &shared_file,
        )
        .await
        .unwrap();
    assert!(!ask(&env, &bob, &runtime, &rt, ch.id, vec![shared]).await);
    env.fga
        .write_tuple(
            &bob.ns().organization_member(bob.org()),
            Relation::Viewer,
            &shared_file,
        )
        .await
        .unwrap();
    assert!(ask(&env, &bob, &runtime, &rt, ch.id, vec![shared]).await);
    // 共有されていない文書が 1 件でも混ざれば公開しない。
    assert!(
        !ask(
            &env,
            &bob,
            &runtime,
            &rt,
            ch.id,
            vec![shared, Uuid::new_v4()]
        )
        .await
    );
    assert_eq!(env.svc.agent_replies(&bob, ch.id).await.unwrap().len(), 3);

    // 非公開チャンネルでは、チャンネルに共有した資料（参加者全員が読める）だけで公開返信にできる。
    let private = env
        .svc
        .create_channel(&alice, create("根拠・非公開", ChannelKind::Private), None)
        .await
        .unwrap();
    env.svc
        .invite(&alice, private.id, vec!["bob".into()], None)
        .await
        .unwrap();
    let doc = Uuid::new_v4();
    env.fga
        .write_tuple(
            &bob.ns().channel_member(&private.id.to_string()),
            Relation::Viewer,
            &bob.ns().file(&doc.to_string()),
        )
        .await
        .unwrap();
    assert!(ask(&env, &bob, &runtime, &rt, private.id, vec![doc]).await);
    assert!(!ask(&env, &bob, &runtime, &rt, private.id, vec![Uuid::new_v4()]).await);
}

/// エージェントを公開返信で呼び、チャンネル（スレッド）に公開されたかを返す。
async fn ask(
    env: &Env,
    who: &AuthContext,
    runtime: &Arc<dyn AgentRuntime>,
    rt: &FakeRuntime,
    channel: Uuid,
    sources: Vec<Uuid>,
) -> bool {
    *rt.sources.lock().unwrap() = sources;
    let trigger = env
        .svc
        .post(
            who,
            channel,
            NewMessage {
                parent_id: None,
                blocks: vec![
                    ContentBlock::Mention {
                        mention: Mention::Agent,
                    },
                    ContentBlock::Text {
                        text: " 共有した資料を要約して".into(),
                    },
                ],
                agent_private: false,
            },
            None,
        )
        .await
        .unwrap();
    env.svc
        .invoke_agent(who, runtime, trigger.id, ReplyMode::Public, None)
        .await
        .unwrap();
    env.svc
        .thread(who, channel, trigger.id)
        .await
        .unwrap()
        .iter()
        .any(|m| m.author.kind == messaging::AuthorKind::Assistant)
}

#[tokio::test]
async fn failure_notice_is_shown_to_the_invoker_but_cannot_be_posted() {
    let Some(env) = setup().await else { return };
    let (alice, bob) = (env.ctx("alice"), env.ctx("bob"));
    let ch = env
        .svc
        .create_channel(&alice, create("失敗の知らせ", ChannelKind::Public), None)
        .await
        .unwrap();
    env.svc.join(&bob, ch.id, None).await.unwrap();
    let trigger = env
        .svc
        .post(
            &bob,
            ch.id,
            NewMessage {
                parent_id: None,
                blocks: vec![
                    ContentBlock::Mention {
                        mention: Mention::Agent,
                    },
                    ContentBlock::Text {
                        text: " 要約して".into(),
                    },
                ],
                agent_private: false,
            },
            None,
        )
        .await
        .unwrap();
    env.svc
        .agent_failed(
            &bob,
            trigger.id,
            &Error::Unavailable("AI を呼び出せませんでした"),
        )
        .await;

    // 呼んだ本人にだけ、失敗として見える。
    let replies = env.svc.agent_replies(&bob, ch.id).await.unwrap();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0].visibility, "failed");
    assert!(env
        .svc
        .agent_replies(&alice, ch.id)
        .await
        .unwrap()
        .is_empty());
    // 回答ではないので、確認の有無にかかわらずチャンネルには投稿できない。
    for confirmed in [false, true] {
        let err = env
            .svc
            .publish_agent_reply(&bob, replies[0].id, confirmed, None)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    }
    let thread = env.svc.thread(&alice, ch.id, trigger.id).await.unwrap();
    assert!(
        thread
            .iter()
            .all(|m| m.author.kind != messaging::AuthorKind::Assistant),
        "{thread:?}"
    );
    // 閉じられる（閉じたら一覧から消える）。
    env.svc
        .dismiss_agent_reply(&bob, replies[0].id)
        .await
        .unwrap();
    assert!(env.svc.agent_replies(&bob, ch.id).await.unwrap().is_empty());
}

/// 回答に ```ui の app（会話に送るボタンとワークフローのボタン）を書くエージェント。
struct UiRuntime;

const UI_ANSWER: &str = "見積もりの条件を選んでください。\n\n```ui\napp 見積もり\n## 条件\n人数 = number 必須 名 as seats\n契約 = choice 月額 | 年間 as term\nbutton 申請する -> workflow quote\nbutton 試算する -> chat\n```";

#[async_trait]
impl AgentRuntime for UiRuntime {
    async fn answer(&self, _ctx: &AuthContext, _req: &AgentRequest) -> Result<AgentAnswer, Error> {
        Ok(AgentAnswer {
            text: UI_ANSWER.into(),
            sources: Vec::new(),
            create_chat: None,
        })
    }

    async fn start_chat(
        &self,
        _ctx: &AuthContext,
        _title: &str,
        _req: &AgentRequest,
    ) -> Result<Uuid, Error> {
        Err(Error::Unavailable("使わない"))
    }

    async fn chat_status(
        &self,
        _ctx: &AuthContext,
        _thread: Uuid,
    ) -> Result<(Vec<PendingApprovalRef>, bool), Error> {
        Ok((Vec::new(), false))
    }
}

#[tokio::test]
async fn ui_buttons_continue_the_thread_as_the_owner_only() {
    let Some(env) = setup().await else { return };
    let (alice, bob) = (env.ctx("alice"), env.ctx("bob"));
    let ch = env
        .svc
        .create_channel(&alice, create("図のボタン", ChannelKind::Public), None)
        .await
        .unwrap();
    env.svc.join(&bob, ch.id, None).await.unwrap();
    let runtime: Arc<dyn AgentRuntime> = Arc::new(UiRuntime);
    let trigger = env
        .svc
        .post(
            &bob,
            ch.id,
            NewMessage {
                parent_id: None,
                blocks: vec![
                    ContentBlock::Mention {
                        mention: Mention::Agent,
                    },
                    ContentBlock::Text {
                        text: " 見積もりを作って".into(),
                    },
                ],
                agent_private: false,
            },
            None,
        )
        .await
        .unwrap();
    env.svc
        .invoke_agent(&bob, &runtime, trigger.id, ReplyMode::Public, None)
        .await
        .unwrap();
    let answer = env
        .svc
        .thread(&bob, ch.id, trigger.id)
        .await
        .unwrap()
        .into_iter()
        .find(|m| m.author.kind == messaging::AuthorKind::Assistant)
        .expect("公開返信がスレッドに入る");
    let block = yui::extract_action_blocks(UI_ANSWER)
        .into_iter()
        .next()
        .unwrap();
    let workflow_key = yui::action_key(&block.hash, "a1");
    let chat_key = yui::action_key(&block.hash, "a2");
    let values: serde_json::Map<String, serde_json::Value> =
        serde_json::from_value(serde_json::json!({ "seats": 300, "term": "年間" })).unwrap();

    // 持ち主が会話に送るボタンを押すと、同じスレッドへの自分のエージェントあての発言になる。
    let (channel, reply) = env
        .svc
        .ui_action_reply(&bob, answer.id, &chat_key, &values)
        .await
        .unwrap();
    assert_eq!(channel, ch.id);
    assert_eq!(reply.parent_id, Some(trigger.id));
    assert!(messaging::mentions_agent(&reply.blocks));
    let text: String = reply
        .blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("試算する") && text.contains("300") && text.contains("年間"),
        "{text}"
    );
    let posted = env.svc.post(&bob, channel, reply, None).await.unwrap();
    assert_eq!(posted.parent_id, Some(trigger.id));

    // 他人のエージェントの回答のボタンは押せない。
    let err = env
        .svc
        .ui_action_reply(&alice, answer.id, &chat_key, &values)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Forbidden(_)), "{err:?}");
    // ワークフローのボタンはチャンネルでは押せない（回答に紛れ込んだボタンで起動させない）。
    let err = env
        .svc
        .ui_action_reply(&bob, answer.id, &workflow_key, &values)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    // 必須の値が無い・回答に無いボタン・人の発言は受け付けない。
    let err = env
        .svc
        .ui_action_reply(&bob, answer.id, &chat_key, &serde_json::Map::new())
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Invalid(_)), "{err:?}");
    let err = env
        .svc
        .ui_action_reply(&bob, answer.id, &format!("{}:a9", block.hash), &values)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::NotFound), "{err:?}");
    let err = env
        .svc
        .ui_action_reply(&bob, trigger.id, &chat_key, &values)
        .await
        .unwrap_err();
    assert!(matches!(err, Error::Forbidden(_)), "{err:?}");
}

#[tokio::test]
async fn stars_dm_excerpts_and_pins_follow_membership() {
    let Some(env) = setup().await else { return };
    let (alice, bob, carol) = (env.ctx("alice"), env.ctx("bob"), env.ctx("carol"));
    env.org_member("carol").await;
    let ch = env
        .svc
        .create_channel(&alice, create("ピンとスター", ChannelKind::Private), None)
        .await
        .unwrap();
    env.svc
        .invite(&alice, ch.id, vec!["bob".into()], None)
        .await
        .unwrap();

    // スターは自分だけの設定（参加していないチャンネルには付けられない）。
    env.svc.set_starred(&bob, ch.id, true).await.unwrap();
    let bobs = env.svc.list_channels(&bob).await.unwrap();
    assert!(bobs.iter().any(|c| c.id == ch.id && c.starred));
    let alices = env.svc.list_channels(&alice).await.unwrap();
    assert!(alices.iter().any(|c| c.id == ch.id && !c.starred));
    assert!(matches!(
        env.svc.set_starred(&carol, ch.id, true).await,
        Err(Error::NotFound | Error::Forbidden(_))
    ));

    // DM は最新の発言の冒頭を一覧の 2 行目に持つ（削除した発言は出さない・チャンネルには付かない）。
    let dm = env.svc.open_dm(&alice, "bob", None).await.unwrap();
    env.svc
        .post(&bob, dm.id, msg("明日の打ち合わせ"), None)
        .await
        .unwrap();
    let last = env
        .svc
        .post(&bob, dm.id, msg("社外秘の話"), None)
        .await
        .unwrap();
    let excerpt = |list: &[messaging::ChannelView], id| {
        list.iter()
            .find(|c| c.id == id)
            .and_then(|c| c.last_excerpt.clone())
    };
    let list = env.svc.list_channels(&alice).await.unwrap();
    assert_eq!(excerpt(&list, dm.id).as_deref(), Some("社外秘の話"));
    assert_eq!(excerpt(&list, ch.id), None);
    env.svc.delete(&bob, last.id, None).await.unwrap();
    let list = env.svc.list_channels(&alice).await.unwrap();
    assert_eq!(excerpt(&list, dm.id).as_deref(), Some("明日の打ち合わせ"));

    // ピン留め: 投稿できる参加者が付け外しでき、一覧は参加者に共通。
    let m = env
        .svc
        .post(&alice, ch.id, msg("見積もりは 3 年で"), None)
        .await
        .unwrap();
    let pinned = env.svc.set_pinned(&bob, m.id, true, None).await.unwrap();
    assert!(pinned.pinned);
    let pins = env.svc.pinned_messages(&alice, ch.id).await.unwrap();
    assert_eq!(pins.iter().map(|p| p.id).collect::<Vec<_>>(), vec![m.id]);
    // 参加していない人は付けられず、一覧も読めない。
    assert!(env.svc.set_pinned(&carol, m.id, true, None).await.is_err());
    assert!(env.svc.pinned_messages(&carol, ch.id).await.is_err());
    // 後から参加した人には、参加前のピン留めは見えない（境界は通常の読取と同じ）。
    env.svc
        .invite(&alice, ch.id, vec!["carol".into()], None)
        .await
        .unwrap();
    assert!(env
        .svc
        .pinned_messages(&carol, ch.id)
        .await
        .unwrap()
        .is_empty());
    // 外すと一覧から消え、削除した発言はピン留めできない。
    let unpinned = env.svc.set_pinned(&alice, m.id, false, None).await.unwrap();
    assert!(!unpinned.pinned);
    assert!(env
        .svc
        .pinned_messages(&alice, ch.id)
        .await
        .unwrap()
        .is_empty());
    env.svc.delete(&alice, m.id, None).await.unwrap();
    assert!(env.svc.set_pinned(&alice, m.id, true, None).await.is_err());
    // 付け外しは監査に残る。
    let audits: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM audit_log WHERE tenant_id = $1 AND action IN ('messaging.message.pin', 'messaging.message.unpin')",
    )
    .bind(&env.tenant)
    .fetch_one(&env.db)
    .await
    .unwrap();
    assert_eq!(audits, 2);
}
