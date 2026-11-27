//! `drive.find` / `drive.move` のユニットテスト（#942）。
//!
//! StorageService は差さず [`DriveOps`] をフェイクで置き換え、「何を問い合わせ・何を更新しに
//! 行き」「モデルに何を返すか」を固定する。認可そのものは StorageService 側の責務
//! （move_rename / read の結合テスト）で、ここではエラーの写し方（存在秘匿）だけを見る。

#![allow(clippy::unwrap_used)]

use std::sync::Mutex;

use super::*;
use chrono::Utc;

fn ctx() -> AuthContext {
    AuthContext::new(
        authz::Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some("default".into()),
        },
        "acme".into(),
        authz::TenantId::parse("default").unwrap(),
    )
}

fn node(id: Uuid, kind: NodeKind, name: &str) -> Node {
    Node {
        id,
        org: "acme".into(),
        tenant_id: "default".into(),
        kind,
        name: name.into(),
        parent_id: None,
        blob_sha256: None,
        size_bytes: None,
        content_type: None,
        version: 3,
        deleted_at: None,
        created_by: "alice".into(),
        updated_by: "alice".into(),
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

fn crumb(id: Uuid, kind: NodeKind, name: &str) -> Crumb {
    Crumb {
        id,
        name: name.into(),
        kind,
    }
}

/// 1 回分の update 呼び出し（種別・対象・新しい名前・移動先）。
type UpdateCall = (NodeKind, Uuid, Option<String>, Option<Option<Uuid>>);

/// 1 回分の find 呼び出し（問い合わせ・種別・続きの位置・件数）。
type FindQuery = (String, Option<NodeKind>, Option<String>, usize);

#[derive(Default)]
struct FakeDrive {
    found: Vec<Node>,
    /// 決めたページを順に返す（空でなければ `found` を使わない）。`(このページの結果, 続きがあるか)`。
    /// 続きの位置はページの番号（storage の打ち切り＝`limit` より少ないページに続きが付く・BUG-187 を真似る）。
    pages: Vec<(Vec<Node>, bool)>,
    /// node_id → パンくず。無い id は NotFound。
    paths: Vec<(Uuid, Vec<Crumb>)>,
    update_err: Option<fn() -> StorageError>,
    updates: Mutex<Vec<UpdateCall>>,
    find_queries: Mutex<Vec<FindQuery>>,
}

impl FakeDrive {
    fn updates(&self) -> Vec<UpdateCall> {
        self.updates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

#[async_trait::async_trait]
impl DriveOps for FakeDrive {
    async fn find(
        &self,
        _ctx: &AuthContext,
        query: &str,
        kind: Option<NodeKind>,
        cursor: Option<&str>,
        limit: usize,
        _trace_id: Option<&str>,
    ) -> Result<ChildPage, StorageError> {
        self.find_queries
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((query.to_string(), kind, cursor.map(str::to_string), limit));
        if !self.pages.is_empty() {
            let at: usize = cursor.map_or(0, |c| c.parse().unwrap());
            let (items, more) = &self.pages[at];
            return Ok(ChildPage {
                items: items.iter().take(limit).cloned().collect(),
                next_cursor: more.then(|| (at + 1).to_string()),
            });
        }
        // 本体（SQL）と同じく、種別と件数は検索側で絞る。
        Ok(ChildPage {
            items: self
                .found
                .iter()
                .filter(|n| kind.is_none_or(|k| n.kind == k))
                .take(limit)
                .cloned()
                .collect(),
            next_cursor: None,
        })
    }

    async fn path(
        &self,
        _ctx: &AuthContext,
        node_id: Uuid,
        _trace_id: Option<&str>,
    ) -> Result<Vec<Crumb>, StorageError> {
        self.paths
            .iter()
            .find(|(id, _)| *id == node_id)
            .map(|(_, p)| p.clone())
            .ok_or(StorageError::NotFound)
    }

    async fn update(
        &self,
        _ctx: &AuthContext,
        kind: NodeKind,
        node_id: Uuid,
        new_name: Option<&str>,
        new_parent: Option<Option<Uuid>>,
        _trace_id: Option<&str>,
    ) -> Result<Node, StorageError> {
        self.updates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((kind, node_id, new_name.map(str::to_string), new_parent));
        if let Some(err) = self.update_err {
            return Err(err());
        }
        Ok(node(node_id, kind, new_name.unwrap_or("報告書.docx")))
    }
}

#[tokio::test]
async fn find_lists_matches_with_place_and_filters_by_kind() {
    let (folder, file) = (Uuid::new_v4(), Uuid::new_v4());
    let parent = Uuid::new_v4();
    let drive = Arc::new(FakeDrive {
        found: vec![
            node(file, NodeKind::File, "経理メモ.md"),
            node(folder, NodeKind::Folder, "経理"),
        ],
        paths: vec![(
            folder,
            vec![
                crumb(parent, NodeKind::Folder, "管理部"),
                crumb(folder, NodeKind::Folder, "経理"),
            ],
        )],
        ..FakeDrive::default()
    });
    let tool = DriveFindTool::new(drive.clone());
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({ "query": " 経理 ", "kind": "folder" }),
            None,
        )
        .await
        .unwrap();
    assert!(!out.is_error);
    assert!(out.content.contains(&format!(
        "[フォルダ] 経理 (id: {folder}) 場所: 管理部 / 経理"
    )));
    // 種別で絞った（ファイルは出さない）。種別と件数は検索側へ渡し、前後の空白は落とす。
    assert!(!out.content.contains("経理メモ.md"));
    assert_eq!(
        drive.find_queries.lock().unwrap().as_slice(),
        &[("経理".to_string(), Some(NodeKind::Folder), None, 10)]
    );
    assert!(tool.is_read_only());
    assert!(!tool.requires_confirmation());
}

#[tokio::test]
async fn find_reports_no_match_and_omits_unreadable_place() {
    let file = Uuid::new_v4();
    let drive = Arc::new(FakeDrive {
        found: vec![node(file, NodeKind::File, "議事録.md")],
        ..FakeDrive::default()
    });
    let tool = DriveFindTool::new(drive);
    // パンくずが引けない（NotFound）ものは場所を省くだけで結果には出す。
    let out = tool
        .call(&ctx(), serde_json::json!({ "query": "議事録" }), None)
        .await
        .unwrap();
    assert!(out
        .content
        .contains(&format!("- [ファイル] 議事録.md (id: {file})\n")));

    let none = DriveFindTool::new(Arc::new(FakeDrive::default()))
        .call(
            &ctx(),
            serde_json::json!({ "query": "無い", "kind": "folder" }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        none.content,
        "「無い」に一致するフォルダは見つかりませんでした。"
    );
}

/// 打ち切られて短い（空の）ページに続きが付いたら、続きをたどって集める（1 ページ目で「無い」と言わない）。
#[tokio::test]
async fn find_follows_cursor_past_short_pages() {
    let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
    let drive = Arc::new(FakeDrive {
        pages: vec![
            (vec![], true),
            (vec![node(a, NodeKind::File, "予算_2025.xlsx")], true),
            (vec![node(b, NodeKind::File, "予算_2026.xlsx")], false),
        ],
        ..FakeDrive::default()
    });
    let out = DriveFindTool::new(drive.clone())
        .call(&ctx(), serde_json::json!({ "query": "予算" }), None)
        .await
        .unwrap();
    assert!(out.content.starts_with("「予算」の検索結果（2 件）:\n"));
    assert!(out.content.contains(&format!("予算_2026.xlsx (id: {b})")));
    assert!(!out.content.contains("探しきれていません"));
    // 続きの位置を渡し、残りの件数だけを頼む。
    let queries = drive.find_queries.lock().unwrap().clone();
    let asked: Vec<_> = queries.iter().map(|q| (q.2.clone(), q.3)).collect();
    assert_eq!(
        asked,
        [(None, 10), (Some("1".into()), 10), (Some("2".into()), 9)]
    );
}

/// ページの上限までたどっても続きが残ったら、「見つかりませんでした」と言い切らず、探しきれなかったと伝える。
#[tokio::test]
async fn find_reports_not_exhausted_when_pages_run_out() {
    let drive = Arc::new(FakeDrive {
        pages: vec![(vec![], true); MAX_FIND_PAGES + 1],
        ..FakeDrive::default()
    });
    let out = DriveFindTool::new(drive.clone())
        .call(&ctx(), serde_json::json!({ "query": "予算" }), None)
        .await
        .unwrap();
    assert!(!out.is_error);
    assert!(out.content.starts_with(
        "「予算」に一致するファイル・フォルダは、探した範囲では見つかりませんでした。"
    ));
    assert!(out.content.contains("無いとは限りません"));
    assert!(out.content.contains("kind で種別を絞って"));
    // 続きの位置（見えないノードの名前を含みうる）はモデルへ出さない。
    assert!(!out.content.contains("cursor"));
    assert_eq!(drive.find_queries.lock().unwrap().len(), MAX_FIND_PAGES);

    // 見つかった分があっても、探しきれていなければそう添える（種別を絞っていれば種別は勧めない）。
    let file = Uuid::new_v4();
    let mut pages = vec![(vec![node(file, NodeKind::File, "予算.xlsx")], true)];
    pages.extend(vec![(vec![], true); MAX_FIND_PAGES]);
    let out = DriveFindTool::new(Arc::new(FakeDrive {
        pages,
        ..FakeDrive::default()
    }))
    .call(
        &ctx(),
        serde_json::json!({ "query": "予算", "kind": "file" }),
        None,
    )
    .await
    .unwrap();
    assert!(out.content.contains(&format!("予算.xlsx (id: {file})")));
    assert!(out.content.contains("目的のものが無ければ"));
    assert!(!out.content.contains("kind で種別を絞って"));
}

#[tokio::test]
async fn find_rejects_empty_query_and_unknown_kind() {
    let tool = DriveFindTool::new(Arc::new(FakeDrive::default()));
    for input in [
        serde_json::json!({ "query": "  " }),
        serde_json::json!({ "query": "a", "kind": "drive" }),
    ] {
        let err = tool.call(&ctx(), input, None).await.unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)));
    }
}

#[tokio::test]
async fn move_file_into_folder_returns_message_and_card() {
    let (file, dest, parent) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let drive = Arc::new(FakeDrive {
        paths: vec![
            (file, vec![crumb(file, NodeKind::File, "報告書.docx")]),
            (
                dest,
                vec![
                    crumb(parent, NodeKind::Folder, "管理部"),
                    crumb(dest, NodeKind::Folder, "経理"),
                ],
            ),
        ],
        ..FakeDrive::default()
    });
    let tool = DriveMoveTool::new(drive.clone());
    assert!(tool.requires_confirmation());
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({ "node_id": file, "destination_folder_id": dest.to_string() }),
            None,
        )
        .await
        .unwrap();
    assert!(!out.is_error);
    assert_eq!(
        out.content,
        "ファイル「報告書.docx」を「管理部 / 経理」へ移動しました。"
    );
    assert_eq!(
        drive.updates(),
        vec![(NodeKind::File, file, None, Some(Some(dest)))]
    );
    assert_eq!(out.document_refs.len(), 1);
    assert_eq!(out.document_refs[0]["id"], file.to_string());
    assert_eq!(out.document_refs[0]["moved_to"], "管理部 / 経理");
}

#[tokio::test]
async fn move_folder_to_root_with_rename_has_no_card() {
    let folder = Uuid::new_v4();
    let drive = Arc::new(FakeDrive {
        paths: vec![(folder, vec![crumb(folder, NodeKind::Folder, "旧")])],
        ..FakeDrive::default()
    });
    let out = DriveMoveTool::new(drive.clone())
        .call(
            &ctx(),
            serde_json::json!({ "node_id": folder, "destination_folder_id": "root", "new_name": "2026年度" }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        out.content,
        "フォルダ「旧」を「マイドライブ（ルート）」へ移動し、「2026年度」に名前を変えました。"
    );
    assert_eq!(
        drive.updates(),
        vec![(
            NodeKind::Folder,
            folder,
            Some("2026年度".into()),
            Some(None)
        )]
    );
    assert!(out.document_refs.is_empty());
}

#[tokio::test]
async fn rename_only_keeps_parent() {
    let file = Uuid::new_v4();
    let drive = Arc::new(FakeDrive {
        paths: vec![(file, vec![crumb(file, NodeKind::File, "a.md")])],
        ..FakeDrive::default()
    });
    let out = DriveMoveTool::new(drive.clone())
        .call(
            &ctx(),
            serde_json::json!({ "node_id": file, "new_name": "b.md" }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        out.content,
        "ファイル「a.md」を「b.md」に名前を変えました。"
    );
    assert_eq!(
        drive.updates(),
        vec![(NodeKind::File, file, Some("b.md".into()), None)]
    );
}

#[tokio::test]
async fn rename_to_same_name_is_not_written() {
    let file = Uuid::new_v4();
    let drive = Arc::new(FakeDrive {
        paths: vec![(file, vec![crumb(file, NodeKind::File, "a.md")])],
        ..FakeDrive::default()
    });
    let out = DriveMoveTool::new(drive.clone())
        .call(
            &ctx(),
            serde_json::json!({ "node_id": file, "new_name": "a.md" }),
            None,
        )
        .await
        .unwrap();
    assert!(out.is_error);
    assert!(out.content.contains("変更がありません"), "{}", out.content);
    assert!(drive.updates().is_empty(), "書込を発生させない");
}

#[tokio::test]
async fn move_rejects_missing_change_and_bad_destination() {
    let tool = DriveMoveTool::new(Arc::new(FakeDrive::default()));
    let id = Uuid::new_v4();
    for input in [
        serde_json::json!({ "node_id": id }),
        serde_json::json!({ "node_id": id, "new_name": "  " }),
        serde_json::json!({ "node_id": id, "destination_folder_id": "経理" }),
    ] {
        let err = tool.call(&ctx(), input, None).await.unwrap_err();
        assert!(matches!(err, ToolError::Invalid(_)));
    }
}

#[tokio::test]
async fn move_hides_existence_and_rejects_file_destination() {
    let (file, other) = (Uuid::new_v4(), Uuid::new_v4());
    let drive = Arc::new(FakeDrive {
        paths: vec![
            (file, vec![crumb(file, NodeKind::File, "a.md")]),
            (other, vec![crumb(other, NodeKind::File, "b.md")]),
        ],
        ..FakeDrive::default()
    });
    let tool = DriveMoveTool::new(drive.clone());
    // 読めない対象・読めない移動先は同じ文言（存在秘匿）。update は呼ばない。
    for input in [
        serde_json::json!({ "node_id": Uuid::new_v4(), "destination_folder_id": "root" }),
        serde_json::json!({ "node_id": file, "destination_folder_id": Uuid::new_v4().to_string() }),
    ] {
        let out = tool.call(&ctx(), input, None).await.unwrap();
        assert!(out.is_error);
        assert!(out.content.contains("アクセスできません"));
    }
    // 移動先がファイル。
    let out = tool
        .call(
            &ctx(),
            serde_json::json!({ "node_id": file, "destination_folder_id": other.to_string() }),
            None,
        )
        .await
        .unwrap();
    assert_eq!(
        out.content,
        "destination_folder_id がフォルダではありません。"
    );
    assert!(drive.updates().is_empty());
}

/// update が返すエラーの作り方と、観測に含まれるべき文言。
type ErrorCase = (fn() -> StorageError, &'static str);

#[tokio::test]
async fn move_maps_storage_errors() {
    let file = Uuid::new_v4();
    let cases: [ErrorCase; 4] = [
        (|| StorageError::Forbidden, "アクセスできません"),
        (|| StorageError::Conflict, "同じ名前のものがあります"),
        (
            || StorageError::Invalid("自身の配下へは移動できません".into()),
            "移動できません: 自身の配下へは移動できません",
        ),
        (
            || StorageError::Integrity("x".into()),
            "移動に失敗しました。",
        ),
    ];
    for (err, expected) in cases {
        let drive = Arc::new(FakeDrive {
            paths: vec![(file, vec![crumb(file, NodeKind::File, "a.md")])],
            update_err: Some(err),
            ..FakeDrive::default()
        });
        let out = DriveMoveTool::new(drive)
            .call(
                &ctx(),
                serde_json::json!({ "node_id": file, "destination_folder_id": "root" }),
                None,
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.content.contains(expected), "{}", out.content);
    }
}

/// 作成系の保存先は省略・空・"root" をルート（None）として受け、UUID 以外は入力エラーにする。
#[test]
fn parent_id_accepts_root_and_empty() {
    #[derive(Deserialize)]
    struct In {
        #[serde(default, deserialize_with = "super::de_parent_id")]
        parent_id: Option<Uuid>,
    }
    #[derive(Deserialize)]
    struct Draft {
        #[serde(default, deserialize_with = "super::de_draft_parent")]
        parent_id: Option<String>,
    }
    let id = Uuid::new_v4();
    for (raw, want) in [
        (serde_json::json!({}), None),
        (serde_json::json!({ "parent_id": "" }), None),
        (serde_json::json!({ "parent_id": "root" }), None),
        (serde_json::json!({ "parent_id": id.to_string() }), Some(id)),
    ] {
        assert_eq!(serde_json::from_value::<In>(raw).unwrap().parent_id, want);
    }
    assert!(serde_json::from_value::<In>(serde_json::json!({ "parent_id": "経理" })).is_err());
    // 下書きは「指定なし」と「ルートへ戻す」を区別する。
    let d = |v| serde_json::from_value::<Draft>(v).unwrap().parent_id;
    assert_eq!(d(serde_json::json!({ "parent_id": " " })), None);
    assert_eq!(
        d(serde_json::json!({ "parent_id": "root" })).as_deref(),
        Some("root")
    );
    assert_eq!(
        d(serde_json::json!({ "parent_id": id.to_string() })),
        Some(id.to_string())
    );
}
