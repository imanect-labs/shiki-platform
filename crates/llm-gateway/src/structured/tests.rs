use serde_json::json;

use super::*;
use crate::model::{Message, Role};

fn entry(real_id: &str, mode: Option<StructuredOutputMode>) -> ModelEntry {
    ModelEntry {
        id: "logical".into(),
        real_id: Some(real_id.into()),
        structured_output: mode,
        ..Default::default()
    }
}

fn schema() -> Value {
    json!({
        "type": "object",
        "required": ["category"],
        "properties": {
            "category": { "type": "string", "enum": ["billing", "bug"] },
            "summary": { "type": "string", "maxLength": 200 },
        },
    })
}

fn request(system: Option<&str>) -> GenerateRequest {
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "分類して")]);
    req.system = system.map(str::to_string);
    req.response_format = Some(ResponseFormat::JsonSchema {
        name: "triage".into(),
        schema: schema(),
    });
    req
}

#[test]
fn anthropic_defaults_to_json_schema_except_legacy_models() {
    use StructuredOutputMode::{JsonSchema, Prompt};
    for m in [
        "claude-haiku-5-5",
        "claude-opus-5-5",
        "claude-sonnet-4-5-20250929",
        "claude-haiku-4-5-20251001",
        "claude-opus-4-5-20251101",
        // 未知の新しいモデルは対応扱い。
        "claude-fable-9",
    ] {
        assert_eq!(
            mode_for(ProviderKind::Anthropic, &entry(m, None)),
            JsonSchema,
            "{m}"
        );
    }
    for m in [
        "claude-3-7-sonnet-latest",
        "claude-3-5-haiku-20241022",
        "claude-opus-4-1-20250805",
        "claude-opus-4-20250514",
        "claude-sonnet-4-20250514",
        "claude-sonnet-4-0",
        "claude-haiku-4-0",
    ] {
        assert_eq!(
            mode_for(ProviderKind::Anthropic, &entry(m, None)),
            Prompt,
            "{m}"
        );
    }
}

#[test]
fn openai_compatible_defaults_to_prompt_and_is_enabled_per_model() {
    use StructuredOutputMode::{JsonObject, JsonSchema, Prompt};
    // vLLM・Gemini は対応差があるので既定は指示だけ。
    assert_eq!(
        mode_for(ProviderKind::Openai, &entry("gpt-x", None)),
        Prompt
    );
    assert_eq!(
        mode_for(ProviderKind::Openai, &entry("gpt-x", Some(JsonSchema))),
        JsonSchema
    );
    assert_eq!(
        mode_for(ProviderKind::Openai, &entry("gpt-x", Some(JsonObject))),
        JsonObject
    );
    assert_eq!(mode_for(ProviderKind::Stub, &entry("s", None)), JsonSchema);
}

#[test]
fn modes_the_kind_cannot_carry_fall_back_to_prompt() {
    use StructuredOutputMode::{JsonObject, JsonSchema, Prompt};
    // Anthropic に json_object は無い・Responses API と ChatGPT プランは写していない。
    assert_eq!(
        mode_for(
            ProviderKind::Anthropic,
            &entry("claude-haiku-5-5", Some(JsonObject))
        ),
        Prompt
    );
    for kind in [ProviderKind::OpenaiResponses, ProviderKind::ChatgptPlan] {
        assert_eq!(mode_for(kind, &entry("m", Some(JsonSchema))), Prompt);
        assert_eq!(mode_for(kind, &entry("m", None)), Prompt);
    }
    // 旧モデルでも運営が明示すれば従う（対応を確かめた上での上書き）。
    assert_eq!(
        mode_for(
            ProviderKind::Anthropic,
            &entry("claude-opus-4-1-20250805", Some(JsonSchema))
        ),
        JsonSchema
    );
}

#[test]
fn json_schema_mode_passes_the_format_through_untouched() {
    let mut req = request(Some("あなたは仕分け係です。"));
    apply_mode(&mut req, StructuredOutputMode::JsonSchema);
    assert_eq!(req.system.as_deref(), Some("あなたは仕分け係です。"));
    assert!(matches!(
        req.response_format,
        Some(ResponseFormat::JsonSchema { ref name, .. }) if name == "triage"
    ));
}

#[test]
fn prompt_mode_moves_the_schema_into_the_system_prompt() {
    let mut req = request(Some("あなたは仕分け係です。"));
    apply_mode(&mut req, StructuredOutputMode::Prompt);
    assert!(req.response_format.is_none());
    let system = req.system.unwrap();
    assert!(
        system.starts_with("あなたは仕分け係です。\n\n出力の形式: "),
        "{system}"
    );
    // 元のスキーマ（制約込み）を読み戻せる。
    assert_eq!(schema_in_system(&system), Some(schema()));
}

#[test]
fn prompt_mode_without_system_sets_the_instruction_alone() {
    let mut req = request(None);
    apply_mode(&mut req, StructuredOutputMode::Prompt);
    let system = req.system.unwrap();
    assert!(system.starts_with("出力の形式: "), "{system}");
    assert!(system.contains(SCHEMA_OPEN) && system.ends_with(SCHEMA_CLOSE));
}

#[test]
fn json_object_mode_keeps_the_schema_in_the_instruction() {
    let mut req = request(None);
    apply_mode(&mut req, StructuredOutputMode::JsonObject);
    assert_eq!(req.response_format, Some(ResponseFormat::JsonObject));
    assert_eq!(schema_in_system(&req.system.unwrap()), Some(schema()));
}

#[test]
fn json_object_request_becomes_an_instruction_without_schema() {
    for (mode, kept) in [
        (StructuredOutputMode::JsonSchema, false),
        (StructuredOutputMode::JsonObject, true),
        (StructuredOutputMode::Prompt, false),
    ] {
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "x")]);
        req.response_format = Some(ResponseFormat::JsonObject);
        apply_mode(&mut req, mode);
        assert_eq!(req.response_format.is_some(), kept, "{mode:?}");
        let system = req.system.unwrap();
        assert!(system.contains("JSON オブジェクトを 1 つだけ"), "{system}");
        assert!(schema_in_system(&system).is_none());
    }
}

#[test]
fn requests_without_format_are_left_alone() {
    let mut req = GenerateRequest::new(vec![Message::text(Role::User, "x")]);
    req.system = Some("s".into());
    apply_mode(&mut req, StructuredOutputMode::Prompt);
    assert_eq!(req.system.as_deref(), Some("s"));
    assert!(req.response_format.is_none());
}

#[test]
fn wire_schema_drops_unsupported_constraints_and_describes_them() {
    let wire = wire_schema(&json!({
        "type": "object",
        "required": ["score"],
        "x-order": ["score", "note"],
        "properties": {
            "score": { "type": "integer", "minimum": 1, "maximum": 5, "description": "評点" },
            "note": { "type": "string", "maxLength": 200, "pattern": "^[a-z]+$", "default": "" },
            "when": { "type": "string", "format": "date" },
            "phone": { "type": "string", "format": "phone" },
        },
    }));
    let props = &wire["properties"];
    assert_eq!(
        props["score"]["description"],
        "評点（制約: 1 以上、5 以下）"
    );
    assert!(props["score"].get("minimum").is_none() && props["score"].get("maximum").is_none());
    assert_eq!(
        props["note"]["description"],
        "制約: 200 文字以内、正規表現 ^[a-z]+$ に合う"
    );
    assert!(props["note"].get("default").is_none());
    // 受ける format は残し、受けない format は落として書く。
    assert_eq!(props["when"]["format"], "date");
    assert!(props["phone"].get("format").is_none());
    assert_eq!(props["phone"]["description"], "制約: 形式 phone");
    assert!(wire.get("x-order").is_none());
    assert_eq!(wire["additionalProperties"], false);
}

#[test]
fn wire_schema_closes_every_nested_object() {
    let wire = wire_schema(&json!({
        "type": "object",
        "properties": {
            "items": {
                "type": "array",
                "minItems": 2,
                "items": { "type": "object", "properties": { "a": { "type": "string" } } },
            },
            "maybe": { "anyOf": [ { "type": "object", "properties": {} }, { "type": "null" } ] },
            "nullable": { "type": ["object", "null"], "properties": {} },
            // 開いたオブジェクトも閉じる（呼び出し側の検査は false か省略しか通さない）。
            "open": { "type": "object", "additionalProperties": { "type": "string" } },
        },
        "$defs": { "d": { "type": "object" } },
    }));
    let p = &wire["properties"];
    assert_eq!(p["items"]["items"]["additionalProperties"], false);
    assert_eq!(p["items"]["description"], "制約: 2 件以上");
    assert_eq!(p["maybe"]["anyOf"][0]["additionalProperties"], false);
    assert!(p["maybe"]["anyOf"][1].get("additionalProperties").is_none());
    assert_eq!(p["nullable"]["additionalProperties"], false);
    assert_eq!(p["open"]["additionalProperties"], false);
    assert_eq!(wire["$defs"]["d"]["additionalProperties"], false);
    // 文字列やスカラーの項目は閉じない（オブジェクトだけ）。
    assert!(p["items"].get("additionalProperties").is_none());
}

#[test]
fn wire_schema_does_not_touch_property_names_that_look_like_keywords() {
    let wire = wire_schema(&json!({
        "type": "object",
        "properties": { "minimum": { "type": "number" }, "pattern": { "type": "string" } },
        "required": ["minimum", "pattern"],
    }));
    assert_eq!(wire["properties"]["minimum"]["type"], "number");
    assert_eq!(wire["properties"]["pattern"]["type"], "string");
    assert!(wire.get("description").is_none());
}

#[test]
fn strict_is_claimed_only_when_every_property_is_required() {
    let all = wire_schema(&json!({
        "type": "object",
        "required": ["a", "b"],
        "properties": {
            "a": { "type": "string" },
            "b": { "type": "object", "required": ["c"], "properties": { "c": { "type": "integer" } } },
        },
    }));
    assert!(openai_strict_eligible(&all));
    let nested_optional = wire_schema(&json!({
        "type": "object",
        "required": ["b"],
        "properties": {
            "b": { "type": "object", "properties": { "c": { "type": "integer" } } },
        },
    }));
    assert!(!openai_strict_eligible(&nested_optional));
    let in_array = wire_schema(&json!({
        "type": "object",
        "required": ["xs"],
        "properties": {
            "xs": { "type": "array", "items": { "type": "object", "properties": { "c": {} } } },
        },
    }));
    assert!(!openai_strict_eligible(&in_array));
    assert!(!openai_strict_eligible(&wire_schema(&schema())));
}

#[test]
fn openai_response_format_shapes() {
    let rf = openai_response_format(&ResponseFormat::JsonSchema {
        name: "問い合わせ.triage v1".into(),
        schema: schema(),
    });
    assert_eq!(rf["type"], "json_schema");
    assert_eq!(rf["json_schema"]["name"], "______triage_v1");
    assert_eq!(rf["json_schema"]["strict"], false);
    assert_eq!(rf["json_schema"]["schema"]["additionalProperties"], false);
    assert!(rf["json_schema"]["schema"]["properties"]["summary"]
        .get("maxLength")
        .is_none());
    assert_eq!(
        openai_response_format(&ResponseFormat::JsonObject),
        json!({ "type": "json_object" })
    );
    let empty = openai_response_format(&ResponseFormat::JsonSchema {
        name: String::new(),
        schema: json!({ "type": "object", "properties": {} }),
    });
    assert_eq!(empty["json_schema"]["name"], "output");
    assert_eq!(empty["json_schema"]["strict"], true);
}

#[test]
fn anthropic_format_shape() {
    let f = anthropic_format(&ResponseFormat::JsonSchema {
        name: "n".into(),
        schema: schema(),
    })
    .unwrap();
    assert_eq!(f["type"], "json_schema");
    assert!(f.get("name").is_none());
    assert_eq!(f["schema"]["additionalProperties"], false);
    assert!(anthropic_format(&ResponseFormat::JsonObject).is_none());
}

#[test]
fn schema_rejections_are_classified_without_leaking_the_body() {
    let too_complex = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Schema is too complex for compilation."},"request_id":"req_abc"}"#;
    let e = schema_rejection("anthropic", reqwest::StatusCode::BAD_REQUEST, too_complex).unwrap();
    assert!(is_schema_rejection(&e));
    let LlmError::BadRequest(msg) = &e else {
        panic!("{e:?}")
    };
    assert!(
        msg.contains("複雑すぎて") && !msg.contains("req_abc"),
        "{msg}"
    );

    // 実 API の文言（2026-10-27・Haiku 5.5）。任意項目の上限は "too complex" を含まない。
    let optional = r#"{"type":"error","error":{"type":"invalid_request_error","message":"Schemas contains too many optional parameters (26), which would make grammar compilation inefficient. Reduce the number of optional parameters in your tool schemas (limit: 24)."},"request_id":"req_x"}"#;
    let e = schema_rejection("anthropic", reqwest::StatusCode::BAD_REQUEST, optional).unwrap();
    assert!(
        is_schema_rejection(&e) && e.to_string().contains("複雑すぎて"),
        "{e}"
    );
    let unsupported = r#"{"type":"error","error":{"type":"invalid_request_error","message":"output_config.format.schema: For 'integer' type, property 'minimum' is not supported"},"request_id":"req_y"}"#;
    let e = schema_rejection("anthropic", reqwest::StatusCode::BAD_REQUEST, unsupported).unwrap();
    assert!(is_schema_rejection(&e));
    assert!(!e.to_string().contains("minimum"));

    let openai = r#"{"error":{"message":"Invalid schema for response_format 'triage'"}}"#;
    assert!(
        schema_rejection("openai", reqwest::StatusCode::BAD_REQUEST, openai)
            .is_some_and(|e| is_schema_rejection(&e))
    );

    // 思考の深さ（output_config.effort）を受けないモデルの 400 は出力の形の誤りにしない（従来の 400 のまま）。
    let effort = r#"{"type":"error","error":{"type":"invalid_request_error","message":"output_config.effort: Extra inputs are not permitted"},"request_id":"req_z"}"#;
    assert!(schema_rejection("anthropic", reqwest::StatusCode::BAD_REQUEST, effort).is_none());

    // スキーマと関係のない 400・400 以外は分類しない（従来の扱いに任せる）。
    let too_long = r#"{"error":{"message":"prompt is too long: 250000 tokens > 200000 maximum"}}"#;
    assert!(schema_rejection("anthropic", reqwest::StatusCode::BAD_REQUEST, too_long).is_none());
    assert!(
        schema_rejection("anthropic", reqwest::StatusCode::UNAUTHORIZED, too_complex).is_none()
    );
    assert!(!is_schema_rejection(&LlmError::BadRequest(
        "anthropic 400: x".into()
    )));
}

#[test]
fn schema_in_system_ignores_text_without_a_schema() {
    assert!(schema_in_system("ふつうの system").is_none());
    assert!(schema_in_system("<json_schema>\nnot json\n</json_schema>").is_none());
}

#[test]
fn mode_reads_from_the_catalog_spelling() {
    // TOML（[[llm.models]] の structured_output）の綴り。未知の値は設定の誤りとして読み込みで止める。
    for (text, mode) in [
        ("json_schema", StructuredOutputMode::JsonSchema),
        ("json_object", StructuredOutputMode::JsonObject),
        ("prompt", StructuredOutputMode::Prompt),
    ] {
        assert_eq!(
            serde_json::from_value::<StructuredOutputMode>(json!(text)).unwrap(),
            mode
        );
        assert_eq!(mode.as_str(), text);
    }
    assert!(serde_json::from_value::<StructuredOutputMode>(json!("strict")).is_err());
    let e: ModelEntry =
        serde_json::from_value(json!({ "id": "m", "structured_output": "json_object" })).unwrap();
    assert_eq!(e.structured_output, Some(StructuredOutputMode::JsonObject));
    let e: ModelEntry = serde_json::from_value(json!({ "id": "m" })).unwrap();
    assert_eq!(e.structured_output, None);
}

// ---- 項目の並び（BUG-123・D-103） ----

/// 作者が「理由を先に、答えを後に」と並べた形（辞書順なら answer が先）。入れ子と配列の中にも並びを書く。
fn ordered_schema() -> Value {
    json!({
        "type": "object",
        "x-order": ["reason", "answer", "detail", "steps"],
        "required": ["reason", "answer"],
        "properties": {
            "answer": { "type": "string", "enum": ["yes", "no"] },
            "reason": { "type": "string", "maxLength": 200 },
            "detail": {
                "type": "object",
                "x-order": ["zeta", "alpha"],
                "properties": { "alpha": { "type": "string" }, "zeta": { "type": "string" } },
            },
            "steps": {
                "type": "array",
                "items": {
                    "type": "object",
                    "x-order": ["why", "what"],
                    "properties": { "what": { "type": "string" }, "why": { "type": "string" } },
                },
            },
        },
    })
}

/// `text` の中で `keys` がこの順に現れるか（JSON の文字列のキーとして探す）。
fn appear_in_order(text: &str, keys: &[&str]) -> bool {
    let at: Vec<Option<usize>> = keys
        .iter()
        .map(|k| text.find(&format!("\"{k}\":")))
        .collect();
    at.iter().all(Option::is_some) && at.windows(2).all(|w| w[0] < w[1])
}

#[test]
fn wire_schema_text_writes_properties_in_the_authors_order() {
    let schema = ordered_schema();
    let text = wire_schema_text(&schema);
    assert!(
        appear_in_order(&text, &["reason", "answer", "detail", "steps"]),
        "{text}"
    );
    assert!(appear_in_order(&text, &["zeta", "alpha"]), "{text}");
    assert!(appear_in_order(&text, &["why", "what"]), "{text}");
    // 並びの注記は送らず、中身は wire_schema と同じ（受けない制約を落とし、オブジェクトを閉じる）。
    assert!(!text.contains("x-order"), "{text}");
    let parsed: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(parsed, wire_schema(&schema));
    // 値の Map のまま書くと辞書順になる（直す前の送り方）。
    assert!(appear_in_order(
        &wire_schema(&schema).to_string(),
        &["answer", "detail", "reason", "steps"]
    ));
}

#[test]
fn without_x_order_the_text_is_the_same_as_before() {
    // 判定（llm_decider）の形など、並びの注記の無い形は今までと同じバイト列で送る（決定器の版を変えない）。
    let s = schema();
    assert_eq!(wire_schema_text(&s), wire_schema(&s).to_string());
}

#[test]
fn keys_missing_from_x_order_follow_in_the_saved_order() {
    // 書かれていないキーは後ろに保存された順（辞書順）で置く。誤った注記（無いキー・文字でない値・重なり）は
    // 読み飛ばす（ミニアプリは任意の形を送れる）。
    let s = json!({
        "type": "object",
        "x-order": ["delta", "nope", 3, "delta"],
        "properties": {
            "alpha": { "type": "string" },
            "charlie": { "type": "string" },
            "delta": { "type": "string" },
        },
    });
    let text = wire_schema_text(&s);
    assert!(
        appear_in_order(&text, &["delta", "alpha", "charlie"]),
        "{text}"
    );
    assert!(!text.contains("nope"), "{text}");
}

#[test]
fn prompt_mode_writes_the_schema_in_the_authors_order() {
    for mode in [
        StructuredOutputMode::Prompt,
        StructuredOutputMode::JsonObject,
    ] {
        let mut req = GenerateRequest::new(vec![Message::text(Role::User, "x")]);
        req.response_format = Some(ResponseFormat::JsonSchema {
            name: "judge".into(),
            schema: ordered_schema(),
        });
        apply_mode(&mut req, mode);
        let system = req.system.unwrap();
        assert!(
            appear_in_order(&system, &["reason", "answer", "detail", "steps"]),
            "{mode:?}: {system}"
        );
        assert!(appear_in_order(&system, &["zeta", "alpha"]), "{system}");
        // 指示のモードは制約も書いたまま（モデルが読む）。並びの注記は書かない。
        assert!(system.contains("\"maxLength\":200"), "{system}");
        assert!(!system.contains("x-order"), "{system}");
    }
}

#[test]
fn body_bytes_rewrites_only_the_schema_it_was_given() {
    let format = ResponseFormat::JsonSchema {
        name: "judge".into(),
        schema: ordered_schema(),
    };
    let body = json!({
        "model": "m",
        "messages": [{ "role": "user", "content": "x" }],
        "output_config": { "effort": "low", "format": anthropic_format(&format).unwrap() },
    });
    let bytes = body_bytes(&body, Some(&format), &ANTHROPIC_SCHEMA_AT);
    let text = String::from_utf8(bytes).unwrap();
    assert!(
        appear_in_order(&text, &["reason", "answer", "detail", "steps"]),
        "{text}"
    );
    // 中身は本文と同じ（並びだけが違う）。
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), body);

    // 形の無い要求・別の形が置かれた本文は、そのまま書く。
    let plain = body_bytes(&body, None, &ANTHROPIC_SCHEMA_AT);
    assert_eq!(plain, serde_json::to_vec(&body).unwrap());
    let other = ResponseFormat::JsonSchema {
        name: "other".into(),
        schema: schema(),
    };
    assert_eq!(
        body_bytes(&body, Some(&other), &ANTHROPIC_SCHEMA_AT),
        serde_json::to_vec(&body).unwrap()
    );
    // OpenAI 互換の位置でも同じ。
    let body = json!({ "model": "m", "response_format": openai_response_format(&format) });
    let text = String::from_utf8(body_bytes(&body, Some(&format), &OPENAI_SCHEMA_AT)).unwrap();
    assert!(appear_in_order(&text, &["reason", "answer"]), "{text}");
    assert_eq!(serde_json::from_str::<Value>(&text).unwrap(), body);
}

#[test]
fn many_properties_are_ordered_without_quadratic_work() {
    // ミニアプリは任意の大きさの形を送れる（本文の上限 2MB で 3 文字のキーなら約 20 万項目）。並べ直しが項目の数の
    // 2 乗の手間になると、断られる前にサーバー内で長く CPU を使い、async のタスクをふさぐ（5 万項目で数十秒）。
    const N: usize = 50_000;
    let props: Map<String, Value> = (0..N)
        .map(|i| (format!("k{i}"), json!({ "type": "string" })))
        .collect();
    let order: Vec<Value> = (0..N).rev().map(|i| json!(format!("k{i}"))).collect();
    let with_order = json!({ "type": "object", "x-order": order, "properties": props });
    let without_order = json!({ "type": "object", "properties": with_order["properties"] });

    let started = std::time::Instant::now();
    let text = order::to_text(&with_order);
    let plain = order::to_text(&without_order);
    let took = started.elapsed();
    assert!(took < std::time::Duration::from_secs(2), "{took:?}");
    assert!(appear_in_order(&text, &["k49999", "k25000", "k0"]));
    assert!(appear_in_order(&plain, &["k0", "k25000", "k49999"]));

    // 本文を書く経路（OpenAI 互換の response_format）も同じ並べ方を通る。
    let body =
        json!({ "response_format": { "json_schema": { "schema": wire_schema(&with_order) } } });
    let format = ResponseFormat::JsonSchema {
        name: "big".into(),
        schema: with_order,
    };
    let bytes = body_bytes(&body, Some(&format), &OPENAI_SCHEMA_AT);
    assert!(appear_in_order(
        &String::from_utf8(bytes).unwrap(),
        &["k49999", "k0"]
    ));
}
