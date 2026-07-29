//! 会話の添付ファイルをサンドボックスへ配置する（seed・issue #379）。
//!
//! 添付は履歴に `[添付ファイル: {name}（node_id: {id}）]` として現れるだけで実体が
//! guest に無く、モデルは `/workspace/<name>` を読もうとして毎回 `FileNotFoundError`
//! で 1 ステップを空費していた（#352 実機デモ）。ここで**実行前に実体を置く**ことで
//! 「ファイル名が見えているのに読めない」状態を解消する。
//!
//! 上限つき（件数・1 ファイルサイズ・合計）で、超過や失敗は**黙らせず**モデルへ
//! 行動可能な注記として返す（「/workspace には無い。node_id で csv.query を使え」）。

use std::collections::HashMap;

use authz::AuthContext;
use sandbox_client::{Sandbox, SandboxHandle};

use super::artifacts::{hash_bytes, ENTRYPOINT_NAME};
use crate::tool::{AttachmentRef, AttachmentStore};

/// guest 上のワークスペース（`shell` の `WORKSPACE_DIR` と同じ位置）。
const WORKSPACE_DIR: &str = "/workspace";
/// seed する添付の最大件数（古い会話の全添付を毎回運ばない）。
const MAX_ATTACHMENTS: usize = 20;
/// 1 添付のサイズ上限（shell の seed 上限と同値）。
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// 1 回の実行で運ぶ合計サイズ上限。
const MAX_TOTAL_BYTES: u64 = 32 * 1024 * 1024;

/// seed の結果。`notes` はモデルへの観測、`seeded` は成果物回収の除外に使う。
#[derive(Debug, Default)]
pub(super) struct SeedResult {
    /// 「置けたもの」と「置けなかったもの＋代替手段」の両方。空なら添付が無い。
    pub notes: Vec<String>,
    /// 置いた guest ファイル名 → 内容ハッシュ。**未変更のまま成果物として再保存しない**
    /// ために `collect_artifacts` へ渡す（添付がドライブへ複製され続けるのを防ぐ）。
    pub seeded: HashMap<String, u64>,
}

/// 添付を guest `/workspace/<name>` へ配置し、観測用の注記と seed 済みの一覧を返す。
pub(super) async fn seed_attachments(
    sandbox: &dyn Sandbox,
    ctx: &AuthContext,
    handle: &SandboxHandle,
    store: &dyn AttachmentStore,
    attachments: &[AttachmentRef],
    trace_id: Option<&str>,
) -> SeedResult {
    if attachments.is_empty() {
        return SeedResult::default();
    }
    let mut notes = Vec::new();
    let mut placed = Vec::new();
    let mut seeded: HashMap<String, u64> = HashMap::new();
    let mut total: u64 = 0;
    // 新しい添付から順に運ぶ（打ち切り時に直近の話題が残るようにする）。
    for a in attachments.iter().rev().take(MAX_ATTACHMENTS) {
        let Some(file_name) = safe_guest_name(&a.name) else {
            notes.push(format!(
                "添付「{}」はファイル名が不正なため /workspace に置けませんでした。",
                a.name
            ));
            continue;
        };
        // 新しい方を勝たせる（同じ guest 名へ 2 回 put すると後勝ち＝古い方が残ってしまう）。
        if seeded.contains_key(&file_name) {
            continue;
        }
        let remaining = MAX_TOTAL_BYTES.saturating_sub(total).min(MAX_FILE_BYTES);
        if remaining == 0 {
            notes.push(format!(
                "添付「{}」は合計サイズ上限のため /workspace に置いていません。node_id \
                 「{}」を csv.query / doc_search へ渡して読んでください。",
                a.name, a.node_id
            ));
            continue;
        }
        let bytes = match store.read(ctx, &a.node_id, remaining, trace_id).await {
            Ok(b) => b,
            Err(e) => {
                notes.push(format!(
                    "添付「{}」は /workspace に置けませんでした（{e}）。node_id 「{}」を \
                     csv.query / doc_search へ渡して読んでください。",
                    a.name, a.node_id
                ));
                continue;
            }
        };
        total = total.saturating_add(bytes.len() as u64);
        let digest = hash_bytes(&bytes);
        let path = format!("{WORKSPACE_DIR}/{file_name}");
        match sandbox.put_file(handle, &path, bytes).await {
            Ok(()) => {
                seeded.insert(file_name, digest);
                placed.push(path);
            }
            Err(e) => notes.push(format!(
                "添付「{}」を /workspace へ書き込めませんでした（{e}）。node_id 「{}」を \
                 csv.query / doc_search へ渡して読んでください。",
                a.name, a.node_id
            )),
        }
    }
    if attachments.len() > MAX_ATTACHMENTS {
        notes.push(format!(
            "添付が {} 件あるため新しい {MAX_ATTACHMENTS} 件のみ /workspace に置きました。\
             他は node_id で csv.query / doc_search を使ってください。",
            attachments.len()
        ));
    }
    if !placed.is_empty() {
        // 置いた順（新しい順）ではなくパス順で見せる（会話ごとのブレを減らす）。
        placed.sort();
        notes.insert(
            0,
            format!("会話の添付を配置しました: {}", placed.join(" / ")),
        );
    }
    SeedResult { notes, seeded }
}

/// 表示名を guest のファイル名へ落とす（ディレクトリ経路を持たせない）。
///
/// サンドボックス側にも正規化はあるが、`..` や絶対パスを**渡さない**のはホスト側の責務
/// （PIT-23: サンドボックス境界をまたぐ入力は敵対的として扱う）。
///
/// 実行コード本体（`main.py`）と同名の添付は**改名する**。wasm ティアはコードを
/// `/workspace/main.py` に書く（`backend/wasm/instance.rs`）ため、そのまま置くと実行直前に
/// 上書きされ、モデルは添付ではなく自分のコードを読むことになる（黙って壊れる）。
/// gVisor ティアは `/__exec` の RO bind へ書くので衝突しないが、**ティアは admin ポリシーで
/// 切り替わる**（design §4.6）ので、どちらでも壊れないよう常に改名する。
fn safe_guest_name(name: &str) -> Option<String> {
    let base = name.rsplit(['/', '\\']).next()?.trim();
    if base.is_empty() || base == "." || base == ".." || base.contains('\0') {
        return None;
    }
    if base == ENTRYPOINT_NAME {
        return Some(format!("attachment-{base}"));
    }
    Some(base.to_string())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use sandbox_client::{FakeSandbox, SandboxSpec};

    fn ctx() -> AuthContext {
        AuthContext::new(
            authz::Principal {
                kind: authz::PrincipalKind::User,
                id: "u1".into(),
                email: None,
                groups: vec![],
                roles: vec![],
                tenant_id: Some("t1".into()),
            },
            "org1".into(),
            "t1".into(),
        )
    }

    fn refs(names: &[&str]) -> Vec<AttachmentRef> {
        names
            .iter()
            .enumerate()
            .map(|(i, n)| AttachmentRef {
                node_id: format!("node-{i}"),
                name: (*n).to_string(),
            })
            .collect()
    }

    /// 決め打ちの内容を返す（または失敗する）フェイク。
    struct FakeAttachments {
        bytes: Option<Vec<u8>>,
    }

    #[async_trait::async_trait]
    impl AttachmentStore for FakeAttachments {
        async fn read(
            &self,
            _ctx: &AuthContext,
            node_id: &str,
            _max_bytes: u64,
            _trace_id: Option<&str>,
        ) -> Result<Vec<u8>, crate::tool::ToolError> {
            self.bytes.clone().ok_or_else(|| {
                crate::tool::ToolError::Invalid(format!("{node_id}: サイズ上限を超えています"))
            })
        }
    }

    async fn seed(
        sandbox: &FakeSandbox,
        store: &FakeAttachments,
        attachments: &[AttachmentRef],
    ) -> (SeedResult, sandbox_client::SandboxHandle) {
        let handle = sandbox
            .create(SandboxSpec::code_interpreter(
                sandbox_client::SandboxBackend::Wasm,
                "t1".into(),
                "org1".into(),
                "u1".into(),
            ))
            .await
            .unwrap();
        let result = seed_attachments(sandbox, &ctx(), &handle, store, attachments, None).await;
        (result, handle)
    }

    /// 添付は `/workspace/<表示名>` に置かれ、置いたことが観測に載る（#379 の本筋）。
    #[tokio::test]
    async fn places_attachments_under_workspace() {
        let sandbox = FakeSandbox::default();
        let store = FakeAttachments {
            bytes: Some(b"a,b\n1,2\n".to_vec()),
        };
        let (result, handle) = seed(&sandbox, &store, &refs(&["deals.csv"])).await;
        let notes = result.notes;
        assert_eq!(
            sandbox
                .get_file(&handle, "/workspace/deals.csv")
                .await
                .unwrap(),
            b"a,b\n1,2\n".to_vec()
        );
        assert!(
            notes.iter().any(|n| n.contains("/workspace/deals.csv")),
            "配置を観測へ載せる: {notes:?}"
        );
    }

    /// 添付が無ければ何も置かず、注記も出さない（無関係な会話にノイズを足さない）。
    #[tokio::test]
    async fn no_attachments_produces_no_notes() {
        let sandbox = FakeSandbox::default();
        let store = FakeAttachments { bytes: None };
        let (result, _) = seed(&sandbox, &store, &[]).await;
        assert!(result.notes.is_empty() && result.seeded.is_empty());
    }

    /// 読めなかった添付は**行動可能な**注記になる（node_id と代替ツールを示す・受け入れ条件）。
    #[tokio::test]
    async fn unreadable_attachment_yields_actionable_note() {
        let sandbox = FakeSandbox::default();
        let store = FakeAttachments { bytes: None };
        let (result, _) = seed(&sandbox, &store, &refs(&["huge.csv"])).await;
        assert!(
            result.seeded.is_empty(),
            "置けていないものを seeded に載せない"
        );
        let joined = result.notes.join("\n");
        assert!(joined.contains("huge.csv"), "{joined}");
        assert!(joined.contains("node-0"), "node_id を示す: {joined}");
        assert!(joined.contains("csv.query"), "代替手段を示す: {joined}");
    }

    /// 件数上限を超える添付は**新しい方**を置き、打ち切ったことを黙らせない。
    #[tokio::test]
    async fn caps_attachment_count_and_reports_truncation() {
        let sandbox = FakeSandbox::default();
        let store = FakeAttachments {
            bytes: Some(b"x".to_vec()),
        };
        let names: Vec<String> = (0..MAX_ATTACHMENTS + 3)
            .map(|i| format!("f{i}.csv"))
            .collect();
        let attachments = refs(&names.iter().map(String::as_str).collect::<Vec<_>>());
        let (result, handle) = seed(&sandbox, &store, &attachments).await;
        let notes = result.notes;
        // 最も新しい添付は置かれ、最も古い添付は置かれない。
        let newest = format!("/workspace/{}", names[names.len() - 1]);
        let oldest = format!("/workspace/{}", names[0]);
        assert!(sandbox.get_file(&handle, &newest).await.is_ok());
        assert!(sandbox.get_file(&handle, &oldest).await.is_err());
        assert!(
            notes.iter().any(|n| n.contains("件のみ")),
            "打ち切りを観測へ載せる: {notes:?}"
        );
    }

    /// 実行コード本体と同名の添付は改名して置く（コードに上書きされて黙って壊れない）。
    #[tokio::test]
    async fn attachment_named_like_entrypoint_is_renamed() {
        let sandbox = FakeSandbox::default();
        let store = FakeAttachments {
            bytes: Some(b"print(1)".to_vec()),
        };
        let (result, handle) = seed(&sandbox, &store, &refs(&[ENTRYPOINT_NAME])).await;
        assert!(
            sandbox
                .get_file(&handle, "/workspace/attachment-main.py")
                .await
                .is_ok(),
            "改名先に置かれる"
        );
        assert!(
            sandbox
                .get_file(&handle, "/workspace/main.py")
                .await
                .is_err(),
            "実行コードのパスは奪わない"
        );
        // モデルには**実際のパス**を伝える（存在しない /workspace/main.py を案内しない）。
        assert!(
            result.notes[0].contains("/workspace/attachment-main.py"),
            "{:?}",
            result.notes
        );
    }

    /// seed した内容のハッシュを返す（成果物回収の除外に使う・#379）。
    #[tokio::test]
    async fn reports_seeded_digests_for_artifact_exclusion() {
        let sandbox = FakeSandbox::default();
        let bytes = b"a,b\n1,2\n".to_vec();
        let store = FakeAttachments {
            bytes: Some(bytes.clone()),
        };
        let (result, _) = seed(&sandbox, &store, &refs(&["deals.csv"])).await;
        assert_eq!(result.seeded.get("deals.csv"), Some(&hash_bytes(&bytes)));
    }

    #[test]
    fn guest_name_strips_paths_and_rejects_traversal() {
        assert_eq!(safe_guest_name("deals.csv").unwrap(), "deals.csv");
        assert_eq!(safe_guest_name("a/b/deals.csv").unwrap(), "deals.csv");
        assert_eq!(safe_guest_name("..\\..\\etc\\passwd").unwrap(), "passwd");
        assert_eq!(safe_guest_name("/etc/passwd").unwrap(), "passwd");
        for bad in ["", "   ", "..", ".", "a/..", "x\0y"] {
            assert!(safe_guest_name(bad).is_none(), "{bad:?} は拒否されること");
        }
    }
}
