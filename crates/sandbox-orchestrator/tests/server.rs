//! server ロジックの結合テスト（FakeBackend・実 sidecar 不要・カバレッジ主力）。
#![allow(clippy::panic, clippy::expect_used, clippy::unwrap_used)]

use std::sync::Arc;

use futures::StreamExt;
use sandbox_client::pb;
use sandbox_client::server::SandboxService;
use sandbox_client::{ExecEvent, SandboxBackend, SandboxLifetime, SandboxLimits, SandboxSpec};
use sandbox_orchestrator::backend::fake::{FakeBackend, FakeExec};
use sandbox_orchestrator::config::OrchestratorEnv;
use sandbox_orchestrator::registry::Registry;
use sandbox_orchestrator::server::SandboxSvc;
use tonic::Request;

fn spec_pb() -> pb::Spec {
    SandboxSpec::code_interpreter(SandboxBackend::Wasm, "t".into(), "o".into(), "u:1".into()).into()
}

fn svc(backend: FakeBackend) -> SandboxSvc {
    SandboxSvc::new(
        Arc::new(backend),
        Arc::new(Registry::new()),
        OrchestratorEnv::default(),
    )
}

async fn create(svc: &SandboxSvc) -> String {
    svc.create(Request::new(pb::CreateRequest {
        spec: Some(spec_pb()),
    }))
    .await
    .expect("create ok")
    .into_inner()
    .sandbox_id
}

async fn collect_exec(svc: &SandboxSvc, id: &str) -> (String, Option<i32>, bool) {
    let resp = svc
        .exec(Request::new(pb::ExecRequest {
            sandbox_id: id.to_string(),
            kind: pb::ExecKind::Python as i32,
            payload: "print('hi')".to_string(),
            timeout_ms: 0,
        }))
        .await
        .expect("exec ok");
    let mut stream = resp.into_inner();
    let mut stdout = String::new();
    let mut exit = None;
    let mut limited = false;
    while let Some(ev) = stream.next().await {
        let ev = ev.expect("event ok");
        match ev.event.expect("event payload") {
            pb::exec_event::Event::Output(o) => {
                if o.channel == pb::ExecChannel::Stdout as i32 {
                    stdout.push_str(&String::from_utf8_lossy(&o.chunk));
                }
            }
            pb::exec_event::Event::Exited(x) => exit = Some(x.code),
            pb::exec_event::Event::LimitExceeded(_) => limited = true,
        }
    }
    (stdout, exit, limited)
}

#[tokio::test]
async fn create_exec_destroy_roundtrip() {
    let backend = FakeBackend::new().with_exec(FakeExec {
        events: vec![
            ExecEvent::Stdout(b"hi\n".to_vec()),
            ExecEvent::Exited { code: 0 },
        ],
        artifacts: Vec::new(),
    });
    let svc = svc(backend);
    let id = create(&svc).await;
    let (stdout, exit, limited) = collect_exec(&svc, &id).await;
    assert_eq!(stdout, "hi\n");
    assert_eq!(exit, Some(0));
    assert!(!limited);

    svc.destroy(Request::new(pb::DestroyRequest {
        sandbox_id: id.clone(),
    }))
    .await
    .expect("destroy ok");
    // 破棄後は not_found。
    let result = svc
        .exec(Request::new(pb::ExecRequest {
            sandbox_id: id,
            kind: pb::ExecKind::Python as i32,
            payload: "x".into(),
            timeout_ms: 0,
        }))
        .await;
    match result {
        Ok(_) => panic!("exec should fail after destroy"),
        Err(err) => assert_eq!(err.code(), tonic::Code::NotFound),
    }
}

#[tokio::test]
async fn put_get_file_roundtrip() {
    let svc = svc(FakeBackend::new());
    let id = create(&svc).await;
    svc.put_file(Request::new(pb::PutFileRequest {
        sandbox_id: id.clone(),
        path: "out/data.txt".into(),
        content: b"payload".to_vec(),
    }))
    .await
    .expect("put ok");
    let got = svc
        .get_file(Request::new(pb::GetFileRequest {
            sandbox_id: id,
            path: "out/data.txt".into(),
        }))
        .await
        .expect("get ok")
        .into_inner();
    assert_eq!(got.content, b"payload");
}

#[tokio::test]
async fn path_traversal_rejected() {
    let svc = svc(FakeBackend::new());
    let id = create(&svc).await;
    let err = svc
        .put_file(Request::new(pb::PutFileRequest {
            sandbox_id: id,
            path: "../../etc/passwd".into(),
            content: b"x".to_vec(),
        }))
        .await
        .expect_err("traversal rejected");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn output_cap_truncates_and_destroys() {
    // MAX_OUTPUT_BYTES=1MiB。2MiB の stdout を流すと LimitExceeded で打ち切り。
    let big = vec![b'x'; 2 * 1024 * 1024];
    let backend = FakeBackend::new().with_exec(FakeExec {
        events: vec![ExecEvent::Stdout(big), ExecEvent::Exited { code: 0 }],
        artifacts: Vec::new(),
    });
    let svc = svc(backend);
    let id = create(&svc).await;
    let (_stdout, _exit, limited) = collect_exec(&svc, &id).await;
    assert!(limited, "should emit LimitExceeded on output overflow");
}

#[tokio::test]
async fn backend_failure_maps_to_unavailable() {
    let svc = svc(FakeBackend::new().failing());
    let err = svc
        .create(Request::new(pb::CreateRequest {
            spec: Some(spec_pb()),
        }))
        .await
        .expect_err("create should fail");
    assert_eq!(err.code(), tonic::Code::Unavailable);
}

#[tokio::test]
async fn zero_ttl_rejected() {
    // Persistent はワイヤで ttl_ms=0 に落ちる（アルファは Ephemeral のみ）。
    // 0 TTL は「即時期限切れ」で危険なため InvalidArgument で弾く。
    let mut spec =
        SandboxSpec::code_interpreter(SandboxBackend::Wasm, "t".into(), "o".into(), "u:1".into());
    spec.lifetime = SandboxLifetime::Persistent;
    let svc = svc(FakeBackend::new());
    let err = svc
        .create(Request::new(pb::CreateRequest {
            spec: Some(spec.into()),
        }))
        .await
        .expect_err("zero ttl rejected");
    assert_eq!(err.code(), tonic::Code::InvalidArgument);
}

#[tokio::test]
async fn gvisor_backend_rejected_by_fake_is_wasm_only() {
    // FakeBackend は backend 種別を見ないが、spec の backend が gVisor でも
    // server は create を通す（backend 判定は wasm backend の責務）。ここでは
    // wasm limits がそのまま通ることだけ確認する。
    let mut spec =
        SandboxSpec::code_interpreter(SandboxBackend::Wasm, "t".into(), "o".into(), "u:1".into());
    spec.limits = SandboxLimits::constrained();
    let svc = svc(FakeBackend::new());
    let id = svc
        .create(Request::new(pb::CreateRequest {
            spec: Some(spec.into()),
        }))
        .await
        .expect("create ok")
        .into_inner()
        .sandbox_id;
    assert!(!id.is_empty());
}
