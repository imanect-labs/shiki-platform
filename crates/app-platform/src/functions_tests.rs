#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::*;

#[test]
fn egress_is_default_deny_with_exact_and_suffix() {
    let allow = vec!["api.example.com".to_string(), "*.trusted.io".to_string()];
    assert!(egress_allowed(&allow, "api.example.com"));
    assert!(egress_allowed(&allow, "API.EXAMPLE.COM"));
    assert!(egress_allowed(&allow, "a.trusted.io"));
    assert!(egress_allowed(&allow, "deep.a.trusted.io"));
    // default-deny: 部分一致・apex・前方付加はすべて拒否。
    assert!(!egress_allowed(&allow, "example.com"));
    assert!(!egress_allowed(&allow, "evil-api.example.com.attacker.net"));
    assert!(!egress_allowed(&allow, "trusted.io"));
    assert!(!egress_allowed(&allow, "xtrusted.io"));
    assert!(!egress_allowed(&[], "api.example.com"));
}

#[test]
fn actor_str_round_trip() {
    assert_eq!(FunctionActor::User.as_str(), "user");
    assert_eq!(FunctionActor::Service.as_str(), "service");
}

/// エラーヘルパの code は分類のために安定していること（監査・再試行判断に使う）。
#[test]
fn error_helpers_carry_stable_codes() {
    assert!(
        matches!(invalid("x"), HostResponse::Err { code, retryable, .. } if code == "invalid" && !retryable)
    );
    assert!(
        matches!(internal("x"), HostResponse::Err { code, retryable, .. } if code == "internal" && !retryable)
    );
    assert!(
        matches!(deny("h", "r"), HostResponse::Err { code, retryable, .. } if code == "egress_denied" && !retryable)
    );
}

#[test]
fn arg_parsers_reject_missing_and_malformed_ids() {
    let good = Uuid::new_v4();
    // table_and_body
    assert!(table_and_body(&serde_json::json!({})).is_err());
    assert!(table_and_body(&serde_json::json!({ "table_id": "not-a-uuid" })).is_err());
    let (t, body) = table_and_body(&serde_json::json!({ "table_id": good, "body": { "k": 1 } }))
        .expect("valid table_id parses");
    assert_eq!(t, good);
    assert_eq!(body, Some(serde_json::json!({ "k": 1 })));
    // table_and_id
    assert!(table_and_id(&serde_json::json!({ "table_id": good })).is_err());
    assert!(table_and_id(&serde_json::json!({ "table_id": good, "record_id": "bad" })).is_err());
    let rec = Uuid::new_v4();
    let (t2, r2) = table_and_id(&serde_json::json!({ "table_id": good, "record_id": rec }))
        .expect("valid ids parse");
    assert_eq!((t2, r2), (good, rec));
}

fn handler(allow: Vec<String>) -> GatewayHostCallHandler {
    GatewayHostCallHandler {
        http: reqwest::Client::new(),
        // 到達不能な origin: ネットワークに出る分岐はこのテストでは踏まない。
        gateway_origin: "http://127.0.0.1:1".into(),
        bearer: "test-bearer".into(),
        egress_allowlist: allow,
        app_id: Uuid::nil(),
        function: "fn".into(),
        actor: FunctionActor::Service,
    }
}

fn call(api: &str, args: serde_json::Value) -> HostCall {
    HostCall {
        exec_id: "e".into(),
        seq: 1,
        api: api.into(),
        args,
    }
}

/// ネットワークに出ない分岐（ローカル応答・引数不正・未提供 api）を網羅する。
#[tokio::test]
async fn handle_local_and_rejection_branches() {
    let h = handler(vec![]);

    // log / context はゲートウェイに出ずローカルで応答。
    assert!(matches!(
        h.handle(&call("log", serde_json::json!({ "message": "hi" })))
            .await,
        HostResponse::Ok(_)
    ));
    let ctx = h.handle(&call("context", serde_json::json!({}))).await;
    match ctx {
        HostResponse::Ok(v) => {
            assert_eq!(v["actor"], serde_json::json!("service"));
            assert_eq!(v["function"], serde_json::json!("fn"));
        }
        HostResponse::Err { message, .. } => panic!("context should be Ok: {message}"),
    }

    // Stage A 能力は B2 では未提供（unsupported・fail-closed）。
    assert!(matches!(
        h.handle(&call("storage.read", serde_json::json!({}))).await,
        HostResponse::Err { code, .. } if code == "unsupported"
    ));
    // 閉集合外は unknown_api。
    assert!(matches!(
        h.handle(&call("evil.exfiltrate", serde_json::json!({}))).await,
        HostResponse::Err { code, .. } if code == "unknown_api"
    ));

    // data.* は table_id/record_id 不正ならゲートウェイへ出る前に invalid で弾く。
    for api in ["data.query", "data.get", "data.create", "data.update"] {
        assert!(
            matches!(
                h.handle(&call(api, serde_json::json!({ "table_id": "bad" }))).await,
                HostResponse::Err { code, .. } if code == "invalid"
            ),
            "{api} should reject bad table_id before network"
        );
    }
}

/// external_http のガード（url/scheme/port/allowlist）は send() 前に判定される。
#[tokio::test]
async fn external_http_guards_reject_before_send() {
    let h = handler(vec!["api.allowed.com".to_string()]);
    // url 欠落・不正 url・非 http スキームは invalid。
    for (args, code) in [
        (serde_json::json!({}), "invalid"),
        (serde_json::json!({ "url": "::::" }), "invalid"),
        (
            serde_json::json!({ "url": "ftp://api.allowed.com/x" }),
            "invalid",
        ),
        // 既定ポート以外は拒否（内部ポート到達防止）。
        (
            serde_json::json!({ "url": "http://api.allowed.com:8080/x" }),
            "egress_denied",
        ),
        // allowlist 外は default-deny。
        (
            serde_json::json!({ "url": "https://evil.example/x" }),
            "egress_denied",
        ),
    ] {
        match h.external_http(&args).await {
            HostResponse::Err { code: c, .. } => assert_eq!(c, code, "for {args}"),
            HostResponse::Ok(v) => panic!("expected Err for {args}: got Ok({v})"),
        }
    }
}
