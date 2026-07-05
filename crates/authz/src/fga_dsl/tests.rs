//! `.fga` ↔ `.json` userset drift 検査（#66）のテスト。親 [`super`] のパーサ/正規化を検証する。

use super::*;

const FGA: &str = include_str!("../../model/authorization-model.fga");

/// #66 の本丸: `.fga` と `.json` が **userset ルール本体まで**構造的に一致すること。
/// type 名の増減だけでなく、`editor`/`viewer` の継承や公開ワイルドカードの
/// 紛れ込みといった relation 定義の乖離を CI で落とす。
#[test]
fn fga_and_json_are_structurally_equivalent() {
    let dsl = parse_fga(FGA);
    let json = normalize_json(&crate::model::default_model());
    assert_eq!(
        dsl, json,
        ".fga と .json の userset 構造が乖離しています（#66 drift 検出）\n\
         左=.fga（人がレビューする正本） 右=.json（OpenFGA へ投入）"
    );
}

/// パーサが本モデルの userset 種別を正しく写せることの明示確認（正の足場）。
#[test]
fn parses_known_folder_relations() {
    let dsl = parse_fga(FGA);
    let folder = dsl.get("folder").expect("folder 型");
    // parent: [folder]（直接付与のみ）。
    assert_eq!(
        folder.get("parent"),
        Some(&Userset::This(vec![Assignable {
            ty: "folder".into(),
            relation: None,
            wildcard: false,
            condition: None,
        }]))
    );
    // editor: [user, role#member] or owner or editor from parent（union の 3 子）。
    let editor = folder.get("editor").expect("folder.editor");
    let Userset::Union(children) = editor else {
        panic!("editor は union: {editor:?}");
    };
    assert_eq!(children.len(), 3);
    assert!(children.contains(&Userset::Computed("owner".into())));
    assert!(children.contains(&Userset::Ttu {
        tupleset: "parent".into(),
        computed: "editor".into(),
    }));
}

/// role#member が付与型として双方で一致すること（共有語彙 #76 の要）。
#[test]
fn role_member_assignable_matches_on_both_sides() {
    let dsl = parse_fga(FGA);
    let json = normalize_json(&crate::model::default_model());
    for ty in ["folder", "file"] {
        for rel in ["editor", "viewer"] {
            let d = &dsl[ty][rel];
            let j = &json[ty][rel];
            assert_eq!(d, j, "{ty}.{rel} の付与型/継承が一致すること");
        }
    }
}

/// 負例: `.json` の `file.viewer` に公開ワイルドカード `[user:*]` が紛れ込むと
/// 検査が乖離を検出すること（現状の type 名・relation 名検査は素通りする穴）。
#[test]
fn detects_injected_public_wildcard_in_json() {
    let mut model = crate::model::default_model();
    // file.viewer の directly_related_user_types に user:* を追加する。
    let types = model
        .get_mut("type_definitions")
        .and_then(Value::as_array_mut)
        .unwrap();
    let file = types
        .iter_mut()
        .find(|t| t.get("type").and_then(Value::as_str) == Some("file"))
        .unwrap();
    let drut = file
        .get_mut("metadata")
        .and_then(|m| m.get_mut("relations"))
        .and_then(|r| r.get_mut("viewer"))
        .and_then(|r| r.get_mut("directly_related_user_types"))
        .and_then(Value::as_array_mut)
        .unwrap();
    drut.push(serde_json::json!({ "type": "user", "wildcard": {} }));

    let dsl = parse_fga(FGA);
    let json = normalize_json(&model);
    assert_ne!(dsl, json, "公開ワイルドカードの紛れ込みを検出すること");
}

/// 負例: `.json` の継承 relation を差し替えると検出すること
/// （`viewer from parent` を `editor from parent` へ改竄）。
#[test]
fn detects_altered_inheritance_in_json() {
    let mut model = crate::model::default_model();
    let types = model
        .get_mut("type_definitions")
        .and_then(Value::as_array_mut)
        .unwrap();
    let folder = types
        .iter_mut()
        .find(|t| t.get("type").and_then(Value::as_str) == Some("folder"))
        .unwrap();
    // folder.viewer の union 内 tupleToUserset.computedUserset を editor へ改竄。
    let children = folder
        .get_mut("relations")
        .and_then(|r| r.get_mut("viewer"))
        .and_then(|v| v.get_mut("union"))
        .and_then(|u| u.get_mut("child"))
        .and_then(Value::as_array_mut)
        .unwrap();
    for child in children.iter_mut() {
        if let Some(ttu) = child.get_mut("tupleToUserset") {
            ttu["computedUserset"]["relation"] = Value::String("editor".into());
        }
    }
    let dsl = parse_fga(FGA);
    let json = normalize_json(&model);
    assert_ne!(dsl, json, "継承 relation の改竄を検出すること");
}

/// パーサ単体: union の子は表層順序に依らず正規化（ソート）されること。
#[test]
fn union_children_are_order_independent() {
    let a = parse_expr("owner or viewer or editor from parent");
    let b = parse_expr("editor from parent or viewer or owner");
    assert_eq!(a, b, "union は順序非依存で等価");
}

/// パーサ単体: intersection / but not（差集合）を正しく写すこと。
#[test]
fn parses_intersection_and_exclusion() {
    assert_eq!(
        parse_expr("editor and viewer"),
        Userset::Intersection(vec![
            Userset::Computed("editor".into()),
            Userset::Computed("viewer".into()),
        ])
    );
    assert_eq!(
        parse_expr("viewer but not owner"),
        Userset::Exclusion {
            base: Box::new(Userset::Computed("viewer".into())),
            subtract: Box::new(Userset::Computed("owner".into())),
        }
    );
}

/// パーサ単体: ワイルドカードと userset 参照の付与型を区別して写すこと。
#[test]
fn parses_wildcard_and_userset_assignables() {
    let Userset::This(types) = parse_term("[user, role#member, user:*]") else {
        panic!("This を期待");
    };
    assert!(types.contains(&Assignable {
        ty: "user".into(),
        relation: None,
        wildcard: false,
        condition: None,
    }));
    assert!(types.contains(&Assignable {
        ty: "role".into(),
        relation: Some("member".into()),
        wildcard: false,
        condition: None,
    }));
    assert!(types.contains(&Assignable {
        ty: "user".into(),
        relation: None,
        wildcard: true,
        condition: None,
    }));
}

/// パーサ契約（逸脱は失敗）: 未対応の複合式を裸 relation として黙って受理せず
/// panic すること（`or`/`and` 混在の残余が `Computed("... and ...")` にならない）。
#[test]
#[should_panic(expected = "未対応の userset 式")]
fn rejects_unhandled_compound_expression() {
    // `or` で分割後に `viewer and editor` が単項として残り、複合式のまま届く。
    let _ = parse_expr("owner or viewer and editor");
}

/// JSON one-of 検証: rewrite に種別キーが複数同居する不正 JSON を検出（panic）すること。
#[test]
#[should_panic(expected = "userset は種別キーをちょうど 1 つ持つこと")]
fn rejects_json_rewrite_with_multiple_kinds() {
    // `this` と `computedUserset` を同居させた不正 rewrite。
    let bad = serde_json::json!({ "this": {}, "computedUserset": { "relation": "owner" } });
    let _ = parse_json_rewrite(&bad, None, "viewer");
}

/// 条件付き付与（OpenFGA condition）が DSL・JSON 双方から等価に写り、
/// 条件の有無・条件名の差が drift として検出されること。
#[test]
fn parses_and_compares_conditional_assignable() {
    // DSL: `[user with is_valid]` → 条件名 is_valid。
    let Userset::This(dsl_types) = parse_term("[user with is_valid]") else {
        panic!("This を期待");
    };
    let expected = Assignable {
        ty: "user".into(),
        relation: None,
        wildcard: false,
        condition: Some("is_valid".into()),
    };
    assert_eq!(dsl_types, vec![expected.clone()]);

    // JSON: 同じ条件付き付与を metadata から復元して一致すること。
    let metadata = serde_json::json!({
        "relations": {
            "viewer": {
                "directly_related_user_types": [
                    { "type": "user", "condition": "is_valid" }
                ]
            }
        }
    });
    let json_types = assignable_from_metadata(Some(&metadata), "viewer");
    assert_eq!(json_types, vec![expected]);

    // 条件名が異なれば非等価（drift 検出の要）。
    let other = assignable_from_metadata(
        Some(&serde_json::json!({
            "relations": { "viewer": { "directly_related_user_types": [
                { "type": "user", "condition": "is_admin" }
            ] } }
        })),
        "viewer",
    );
    assert_ne!(dsl_types, other, "条件名の差を drift として検出すること");
}
