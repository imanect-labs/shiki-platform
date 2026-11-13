#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
use super::corpus::hash;
use super::*;
use crate::{EmbedInput, EmbedResponse, EmbeddingProvider, RagError};
use authz::{AuthContext, Principal};
use serde_json::json;
use std::{fs, sync::Arc};
struct Embed {
    version: &'static str,
    fail: bool,
}
#[async_trait::async_trait]
impl EmbeddingProvider for Embed {
    fn model_version(&self) -> &str {
        self.version
    }
    async fn embed(
        &self,
        _: &AuthContext,
        _: EmbedInput,
        _: &[String],
    ) -> Result<EmbedResponse, RagError> {
        if self.fail {
            return Err(RagError::Worker("offline".into()));
        }
        Ok(EmbedResponse {
            vectors: vec![vec![1.0, 0.0]],
            dimension: 2,
            model_version: self.version.into(),
        })
    }
}
fn context(tenant: &str) -> AuthContext {
    AuthContext::new(
        Principal {
            kind: authz::PrincipalKind::User,
            id: "alice".into(),
            email: None,
            groups: vec![],
            roles: vec![],
            tenant_id: Some(tenant.into()),
        },
        "org".into(),
        authz::TenantId::parse(tenant).unwrap(),
    )
}
fn fixture() -> (tempfile::TempDir, HelpConfig) {
    fixture_with(&json!({}))
}
/// チャンクに項目を足した（上書きした）コーパス。
fn fixture_with(extra: &serde_json::Value) -> (tempfile::TempDir, HelpConfig) {
    let dir = tempfile::tempdir().unwrap();
    let version = "a".repeat(40);
    let text = "設定からパスキーを登録します。";
    let mut chunk = json!({"schema_version":1,"corpus_version":version,
        "page_id":"security/passkey","chunk_id":"b".repeat(64),"title":"パスキーの登録",
        "heading_path":["手順"],"relative_url":"/help/security/passkey/#手順", "text":text,
        "status":"reviewed","content_sha256":hash(text.as_bytes())});
    for (k, v) in extra.as_object().unwrap() {
        chunk[k] = v.clone();
    }
    let chunks = format!("{chunk}\n");
    let manifest = json!({"schema_version":1,"corpus_id":"shiki-help", "corpus_version":version,
        "source_commit":version,"visibility":"public","inclusion":"reviewed/final",
        "chunk_count":1,"asset_count":0,"chunks_bytes":chunks.len(),"chunks_sha256":hash(chunks.as_bytes())});
    let vectors = format!(
        "{}\n",
        json!({"chunk_id":"b".repeat(64),"vector":[1.0,0.0]})
    );
    let vm = json!({"schema_version":1,"corpus_version":version,"chunks_sha256":hash(chunks.as_bytes()),
        "model_version":"model-v1","dimension":2,"normalized":true,"query_prefix":"検索クエリ: ",
        "document_prefix":"検索文書: ","vectors_sha256":hash(vectors.as_bytes())});
    for (name, bytes) in [
        ("manifest.json", manifest.to_string()),
        ("chunks.jsonl", chunks),
        ("vectors.manifest.json", vm.to_string()),
        ("vectors.jsonl", vectors),
    ] {
        fs::write(dir.path().join(name), bytes).unwrap();
    }
    let config = HelpConfig {
        data_path: dir.path().to_str().unwrap().into(),
        ..HelpConfig::default()
    };
    (dir, config)
}
fn load(config: HelpConfig, version: &'static str, fail: bool) -> HelpSearchService {
    HelpSearchService::load(config, Arc::new(Embed { version, fail }))
        .unwrap()
        .unwrap()
}
#[tokio::test]
async fn public_help_is_identical_across_tenants_and_bounded() {
    let (_dir, config) = fixture();
    let s = load(config, "model-v1", false);
    let a = s.search(&context("a"), "パスキー", 1).await.unwrap();
    let b = s.search(&context("b"), "パスキー", 1).await.unwrap();
    assert_eq!(a[0].chunk_id, b[0].chunk_id);
    assert!(a[0].url.starts_with("https://shiki.imanect.app/help/"));
    assert!(a[0].url.contains('#'));
    assert_eq!(s.keyword_fallbacks(), 0);
    assert!(s.search(&context("a"), "x", 0).await.is_err());
    assert!(s.search(&context("a"), &"x".repeat(1001), 1).await.is_err());
}
#[tokio::test]
async fn mismatch_and_failed_embedding_fall_back_without_tenant_documents() {
    for (version, fail) in [("other-model", false), ("model-v1", true)] {
        let (_dir, config) = fixture();
        let s = load(config, version, fail);
        assert_eq!(
            s.search(&context("a"), "パスキー", 1).await.unwrap().len(),
            1
        );
        assert!(s.keyword_fallbacks() > 0);
        assert!(s
            .search(&context("a"), "社内極秘給与台帳", 1)
            .await
            .unwrap()
            .is_empty());
    }
}
#[test]
fn missing_disabled_and_tampered_corpus() {
    let (dir, mut config) = fixture();
    config.enabled = false;
    assert!(HelpSearchService::load(
        config.clone(),
        Arc::new(Embed {
            version: "v",
            fail: false
        })
    )
    .unwrap()
    .is_none());
    config.enabled = true;
    fs::write(dir.path().join("chunks.jsonl"), "tampered").unwrap();
    assert!(HelpSearchService::load(
        config,
        Arc::new(Embed {
            version: "v",
            fail: false
        })
    )
    .is_err());
    assert!(HelpSearchService::load(
        HelpConfig {
            data_path: dir.path().join("missing").to_str().unwrap().into(),
            ..HelpConfig::default()
        },
        Arc::new(Embed {
            version: "v",
            fail: false
        })
    )
    .unwrap()
    .is_none());
}
#[test]
fn rejects_paths_and_external_links() {
    for url in [
        "https://evil.test/help/a",
        "//evil.test/a",
        "/help/../secret",
        "/help/%2e%2e/secret",
        "/help/a\\b",
        "/help/a?url=x",
    ] {
        assert!(super::corpus::validate_relative_url(url).is_err(), "{url}");
    }
}
#[test]
fn rejects_version_and_vector_membership_substitution() {
    for (name, key, value) in [
        ("manifest.json", "schema_version", json!(2)),
        ("manifest.json", "corpus_version", json!("c".repeat(40))),
        (
            "vectors.manifest.json",
            "corpus_version",
            json!("c".repeat(40)),
        ),
        (
            "vectors.manifest.json",
            "vectors_sha256",
            json!("0".repeat(64)),
        ),
    ] {
        let (dir, config) = fixture();
        let path = dir.path().join(name);
        let mut data: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        data[key] = value;
        fs::write(path, data.to_string()).unwrap();
        assert!(HelpSearchService::load(
            config,
            Arc::new(Embed {
                version: "v",
                fail: false
            })
        )
        .is_err());
    }
}

#[test]
fn rejects_unlisted_chunks_and_vectors_even_with_updated_file_digest() {
    for kind in [
        "extra_chunk",
        "wrong_version",
        "draft",
        "text_hash",
        "foreign_vector",
    ] {
        let (dir, config) = fixture();
        if kind == "foreign_vector" {
            let vector = format!(
                "{}\n",
                json!({"chunk_id":"c".repeat(64),"vector":[1.0,0.0]})
            );
            fs::write(dir.path().join("vectors.jsonl"), &vector).unwrap();
            let path = dir.path().join("vectors.manifest.json");
            let mut vm: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            vm["vectors_sha256"] = json!(hash(vector.as_bytes()));
            fs::write(path, vm.to_string()).unwrap();
        } else {
            let path = dir.path().join("chunks.jsonl");
            let original = fs::read_to_string(&path).unwrap();
            let mut chunk: serde_json::Value = serde_json::from_str(&original).unwrap();
            match kind {
                "wrong_version" => chunk["corpus_version"] = json!("c".repeat(40)),
                "draft" => chunk["status"] = json!("draft"),
                "text_hash" => chunk["text"] = json!("別の本文"),
                _ => {}
            }
            let data = if kind == "extra_chunk" {
                format!("{original}{original}")
            } else {
                format!("{chunk}\n")
            };
            fs::write(&path, &data).unwrap();
            let path = dir.path().join("manifest.json");
            let mut m: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            m["chunks_sha256"] = json!(hash(data.as_bytes()));
            m["chunks_bytes"] = json!(data.len());
            fs::write(path, m.to_string()).unwrap();
        }
        assert!(
            HelpSearchService::load(
                config,
                Arc::new(Embed {
                    version: "model-v1",
                    fail: false
                })
            )
            .is_err(),
            "{kind}"
        );
    }
}

#[tokio::test]
async fn figures_and_app_screens_come_from_the_corpus_and_the_configured_origin() {
    let (_dir, config) = fixture_with(&json!({
        "images":[{"relative_url":"/help/media/security/images/passkey--add.png","alt":"ログイン方法の画面"}],
        "app_links":[{"path":"/settings/security","label":"個人設定 › ログイン方法"},{"path":"/","label":"ホーム"}]
    }));
    let s = load(config, "model-v1", false);
    let hit = &s.search(&context("a"), "パスキー", 1).await.unwrap()[0];
    assert_eq!(
        hit.images[0].url,
        "https://shiki.imanect.app/help/media/security/images/passkey--add.png"
    );
    assert_eq!(hit.images[0].alt, "ログイン方法の画面");
    assert_eq!(hit.app_links.len(), 2);
    assert_eq!(hit.app_links[0].path, "/settings/security");
}
#[test]
fn rejects_figures_and_app_screens_outside_the_contract() {
    let image = |u: &str| json!({"images":[{"relative_url":u,"alt":"x"}]});
    let link = |p: &str| json!({"app_links":[{"path":p,"label":"画面"}]});
    for extra in [
        image("https://evil.example/a.png"),
        image("/help/security/passkey/"),
        image("/help/media/../secret.png"),
        image("/help/media/a.svg"),
        image("/help/media/a%2e.png"),
        link("https://evil.example/"),
        link("//evil.example/x"),
        link("/\\evil.example"),
        link("/api/threads"),
        link("/auth/logout"),
        link("/login"),
        link("/settings/security?x=1"),
        link("/settings/"),
        link(""),
        json!({"app_links":[{"path":"/drive","label":""}]}),
    ] {
        let (_dir, config) = fixture_with(&extra);
        assert!(
            HelpSearchService::load(
                config,
                Arc::new(Embed {
                    version: "model-v1",
                    fail: false
                })
            )
            .is_err(),
            "{extra}"
        );
    }
}
