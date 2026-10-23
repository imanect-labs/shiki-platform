//! テナント束縛の結合テスト（#810）。create 以外の全 RPC が呼び出し元 tenant_id を registry の登録
//! テナントと照合し、不一致・空を fail-closed で弾くこと、不一致と未登録が区別できないこと
//! （存在を漏らさない）を確かめる。後半は実 tonic サーバ越しに `GrpcSandboxClient` が
//! ハンドルのテナントを全 RPC へ確実に載せることを検証する。
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use futures::StreamExt;
use sandbox_client::pb;
use sandbox_client::server::{SandboxService, SandboxServiceServer};
use sandbox_client::{
    ExecEvent, ExecRequest, GrpcSandboxClient, Sandbox, SandboxBackend, SandboxError,
    SandboxHandle, SandboxSpec,
};
use sandbox_orchestrator::backend::fake::{FakeBackend, FakeExec};
use sandbox_orchestrator::config::OrchestratorEnv;
use sandbox_orchestrator::registry::Registry;
use sandbox_orchestrator::server::SandboxSvc;
use tonic::{Code, Request, Status};

const OWNER: &str = "tenant-a";
const OTHER: &str = "tenant-b";

fn spec(tenant: &str) -> SandboxSpec {
    SandboxSpec::code_interpreter(
        SandboxBackend::Wasm,
        tenant.into(),
        "o".into(),
        "u:1".into(),
    )
}

fn exec_script() -> FakeBackend {
    FakeBackend::new().with_exec(FakeExec {
        events: vec![
            ExecEvent::Stdout(b"hi\n".to_vec()),
            ExecEvent::Exited { code: 0 },
        ],
        artifacts: Vec::new(),
    })
}

fn svc(backend: FakeBackend) -> (SandboxSvc, Arc<Registry>) {
    let registry = Arc::new(Registry::new());
    let svc = SandboxSvc::new(
        Arc::new(backend),
        Arc::clone(&registry),
        OrchestratorEnv::default(),
    );
    (svc, registry)
}

async fn create(svc: &SandboxSvc, tenant: &str) -> String {
    svc.create(Request::new(pb::CreateRequest {
        spec: Some(spec(tenant).into()),
    }))
    .await
    .expect("create ok")
    .into_inner()
    .sandbox_id
}

async fn exec(svc: &SandboxSvc, id: &str, tenant: &str) -> Result<(), Status> {
    let mut stream = svc
        .exec(Request::new(pb::ExecRequest {
            sandbox_id: id.into(),
            kind: pb::ExecKind::Python as i32,
            payload: "print('hi')".into(),
            timeout_ms: 0,
            tenant_id: tenant.into(),
        }))
        .await?
        .into_inner();
    while let Some(ev) = stream.next().await {
        ev?;
    }
    Ok(())
}

async fn put(svc: &SandboxSvc, id: &str, tenant: &str) -> Result<(), Status> {
    svc.put_file(Request::new(pb::PutFileRequest {
        sandbox_id: id.into(),
        path: "out/data.txt".into(),
        content: b"secret".to_vec(),
        tenant_id: tenant.into(),
    }))
    .await
    .map(|_| ())
}

async fn get(svc: &SandboxSvc, id: &str, tenant: &str) -> Result<Vec<u8>, Status> {
    svc.get_file(Request::new(pb::GetFileRequest {
        sandbox_id: id.into(),
        path: "out/data.txt".into(),
        tenant_id: tenant.into(),
    }))
    .await
    .map(|r| r.into_inner().content)
}

async fn list(svc: &SandboxSvc, id: &str, tenant: &str) -> Result<(), Status> {
    svc.list_dir(Request::new(pb::ListDirRequest {
        sandbox_id: id.into(),
        path: "out".into(),
        tenant_id: tenant.into(),
    }))
    .await
    .map(|_| ())
}

async fn destroy(svc: &SandboxSvc, id: &str, tenant: &str) -> Result<(), Status> {
    svc.destroy(Request::new(pb::DestroyRequest {
        sandbox_id: id.into(),
        tenant_id: tenant.into(),
    }))
    .await
    .map(|_| ())
}

/// 越境応答が「未登録 ID」と同じ code・message であること（存在を漏らさない）を確かめる。
fn assert_same_as_missing(foreign: &Status, missing: &Status, op: &str) {
    assert_eq!(
        foreign.code(),
        Code::NotFound,
        "{op}: foreign must be not_found"
    );
    assert_eq!(foreign.code(), missing.code(), "{op}: code differs");
    assert_eq!(
        foreign.message(),
        missing.message(),
        "{op}: message differs"
    );
}

#[tokio::test]
async fn foreign_tenant_is_indistinguishable_from_missing() {
    let (svc, _) = svc(exec_script());
    let id = create(&svc, OWNER).await;
    put(&svc, &id, OWNER).await.expect("owner put");
    let missing_id = uuid_like();

    let foreign = exec(&svc, &id, OTHER).await.expect_err("foreign exec");
    let missing = exec(&svc, &missing_id, OTHER)
        .await
        .expect_err("missing exec");
    assert_same_as_missing(&foreign, &missing, "exec");

    let foreign = put(&svc, &id, OTHER).await.expect_err("foreign put");
    let missing = put(&svc, &missing_id, OTHER)
        .await
        .expect_err("missing put");
    assert_same_as_missing(&foreign, &missing, "put_file");

    let foreign = get(&svc, &id, OTHER).await.expect_err("foreign get");
    let missing = get(&svc, &missing_id, OTHER)
        .await
        .expect_err("missing get");
    assert_same_as_missing(&foreign, &missing, "get_file");

    let foreign = list(&svc, &id, OTHER).await.expect_err("foreign list");
    let missing = list(&svc, &missing_id, OTHER)
        .await
        .expect_err("missing list");
    assert_same_as_missing(&foreign, &missing, "list_dir");

    // 越境 put は所有者のファイルを書き換えていない。
    assert_eq!(get(&svc, &id, OWNER).await.expect("owner get"), b"secret");
    // 所有テナントなら exec も通る（照合が正の経路を壊していない）。
    exec(&svc, &id, OWNER).await.expect("owner exec");
}

#[tokio::test]
async fn foreign_destroy_is_noop_and_looks_idempotent() {
    let (svc, registry) = svc(FakeBackend::new());
    let id = create(&svc, OWNER).await;
    put(&svc, &id, OWNER).await.expect("owner put");

    // 他テナントの destroy は未登録 ID の destroy と同じく成功応答だが、何も破棄しない。
    destroy(&svc, &id, OTHER).await.expect("foreign destroy ok");
    destroy(&svc, &uuid_like(), OTHER)
        .await
        .expect("missing destroy ok");
    assert_eq!(registry.len().await, 1);
    assert_eq!(get(&svc, &id, OWNER).await.expect("still alive"), b"secret");

    // 所有テナントの destroy で消え、以降は not_found。
    destroy(&svc, &id, OWNER).await.expect("owner destroy");
    assert!(registry.is_empty().await);
    let err = get(&svc, &id, OWNER).await.expect_err("gone");
    assert_eq!(err.code(), Code::NotFound);
}

#[tokio::test]
async fn empty_tenant_is_rejected_on_every_rpc() {
    let (svc, registry) = svc(exec_script());
    let id = create(&svc, OWNER).await;
    for blank in ["", "  "] {
        let errs = [
            ("exec", exec(&svc, &id, blank).await.expect_err("exec")),
            ("put_file", put(&svc, &id, blank).await.expect_err("put")),
            (
                "get_file",
                get(&svc, &id, blank).await.map(|_| ()).expect_err("get"),
            ),
            ("list_dir", list(&svc, &id, blank).await.expect_err("list")),
            (
                "destroy",
                destroy(&svc, &id, blank).await.expect_err("destroy"),
            ),
        ];
        for (op, err) in errs {
            assert_eq!(err.code(), Code::InvalidArgument, "{op} with {blank:?}");
        }
    }
    // 空テナントの destroy では破棄されない。
    assert_eq!(registry.len().await, 1);
}

#[tokio::test]
async fn create_rejects_empty_tenant() {
    let (svc, registry) = svc(FakeBackend::new());
    let err = svc
        .create(Request::new(pb::CreateRequest {
            spec: Some(spec("").into()),
        }))
        .await
        .expect_err("empty tenant create");
    assert_eq!(err.code(), Code::InvalidArgument);
    assert!(registry.is_empty().await);
}

fn uuid_like() -> String {
    "00000000-0000-4000-8000-000000000000".into()
}

// ---- 実 tonic サーバ越し（GrpcSandboxClient がテナントを載せること） ----

/// tokio ランタイム上で呼ぶこと（サーバを spawn する）。
fn serve(backend: FakeBackend) -> GrpcSandboxClient {
    let (svc, _) = svc(backend);
    let incoming =
        tonic::transport::server::TcpIncoming::bind("127.0.0.1:0".parse().unwrap()).expect("bind");
    let addr = incoming.local_addr().expect("local addr");
    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(SandboxServiceServer::new(svc))
            .serve_with_incoming(incoming)
            .await
            .expect("serve");
    });
    GrpcSandboxClient::connect_lazy(format!("http://{addr}")).expect("client")
}

#[tokio::test]
async fn grpc_client_binds_handle_to_creating_tenant() {
    let client = serve(exec_script());
    let handle = client.create(spec(OWNER)).await.expect("create");
    assert_eq!(handle.tenant_id, OWNER);

    // 正の経路: クライアントがハンドルのテナントを全 RPC に載せている（空なら invalid_argument で落ちる）。
    client
        .put_file(&handle, "out/data.txt", b"secret".to_vec())
        .await
        .expect("put");
    assert_eq!(
        client.get_file(&handle, "out/data.txt").await.expect("get"),
        b"secret"
    );
    assert!(!client
        .list_dir(&handle, "out")
        .await
        .expect("list")
        .is_empty());
    let mut stream = client
        .exec(
            &handle,
            ExecRequest::Python {
                code: "print('hi')".into(),
                timeout_ms: None,
            },
        )
        .await
        .expect("exec");
    while let Some(ev) = stream.next().await {
        ev.expect("exec event");
    }

    // 漏洩した sandbox_id を別テナントのハンドルで使っても引けない。
    let forged = SandboxHandle {
        id: handle.id.clone(),
        tenant_id: OTHER.into(),
    };
    assert!(matches!(
        client.get_file(&forged, "out/data.txt").await,
        Err(SandboxError::NotFound(_))
    ));
    assert!(matches!(
        client.list_dir(&forged, "out").await,
        Err(SandboxError::NotFound(_))
    ));
    assert!(matches!(
        client
            .put_file(&forged, "out/data.txt", b"x".to_vec())
            .await,
        Err(SandboxError::NotFound(_))
    ));
    assert!(matches!(
        client
            .exec(
                &forged,
                ExecRequest::Shell {
                    cmd: "ls".into(),
                    timeout_ms: None
                }
            )
            .await,
        Err(SandboxError::NotFound(_))
    ));
    client
        .destroy(&forged)
        .await
        .expect("foreign destroy no-op");
    assert_eq!(
        client
            .get_file(&handle, "out/data.txt")
            .await
            .expect("alive"),
        b"secret"
    );

    client.destroy(&handle).await.expect("destroy");
    assert!(matches!(
        client.get_file(&handle, "out/data.txt").await,
        Err(SandboxError::NotFound(_))
    ));
}
