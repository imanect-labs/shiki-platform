//! 道具の object の引数の受け口（GAP-119・GAP-130・D-179・D-186）。
//!
//! モデルは object の引数を JSON の文字列にして送ることがある。どの道具でも同じ規則で受ける（写しを作らない）:
//! - object はそのまま。省いた・null は道具の扱い（必須か省略できるか）に任せる。
//! - 文字列は JSON として **1 段だけ**読み、object になれば object で来たものとして扱う。入力の中で置き換えるので、
//!   後の検証と上限は object で来たときと同じものを通る（受ける形を広げても権限や検証は変わらない）。
//! - 読めない文字列・object にならない文字列（配列・数・文字列の JSON）・object でない値は、直し方を言って断る。
//!   同じ道具が SQL の文を別の引数で受けるとき（data_query の `sql`）、SQL の文が来たらそちらに書くよう言う。
//!
//! 使う道具: emit_workflow の `ir`・data_query の `query`・data_record_write の `data`・save_skill の
//! `knowledge_scope` と `model`（input_schema で `type: object` の引数を持つ道具のすべて）。

use serde_json::Value;

/// object の引数（名前と、断る文に添える中身の説明）。
#[derive(Debug, Clone, Copy)]
pub(crate) struct ObjectArg {
    /// 引数の名前。
    pub key: &'static str,
    /// 何を持つオブジェクトか（断る文の括弧の中に入れる）。
    pub shape: &'static str,
    /// 同じ道具で SQL の文を受ける引数の名前（data_query の `sql`）。この引数に SQL の文（SELECT・WITH で始まる、
    /// JSON として読めない文字列）が来たら、JSON にしたことではなく、書く引数の取り違えを言う。
    pub sql_arg: Option<&'static str>,
}

/// SQL の文に見えるか（前の空白を除いた最初の語が SELECT・WITH・大文字小文字は問わない）。
fn looks_like_sql(raw: &str) -> bool {
    raw.split_whitespace().next().is_some_and(|word| {
        word.eq_ignore_ascii_case("select") || word.eq_ignore_ascii_case("with")
    })
}

impl ObjectArg {
    /// object で渡すよう言う文（読めない文字列のときは前に読めなかった位置を足す）。
    pub(crate) fn not_object(&self) -> String {
        format!(
            "{} は JSON の文字列にせず、オブジェクトのまま渡してください（{}）。",
            self.key, self.shape
        )
    }

    /// 値を object として読む。object はそのまま（`None`）、JSON として読むと object になる文字列は読んだもの
    /// （`Some`）。ほかは断る文を返す。
    pub(crate) fn read(&self, value: &Value) -> Result<Option<Value>, String> {
        match value {
            Value::Object(_) => Ok(None),
            Value::String(raw) => match serde_json::from_str::<Value>(raw) {
                Ok(parsed @ Value::Object(_)) => Ok(Some(parsed)),
                Ok(_) => Err(self.not_object()),
                Err(_) if self.sql_arg.is_some() && looks_like_sql(raw) => Err(format!(
                    "{key} に SQL の文が入っています。SQL は {key} ではなく {sql} に書いてください（{key} は {shape}）。",
                    key = self.key,
                    sql = self.sql_arg.unwrap_or_default(),
                    shape = self.shape
                )),
                Err(e) => Err(format!(
                    "{} の文字列を JSON として読めませんでした（{e}）。{}",
                    self.key,
                    self.not_object()
                )),
            },
            _ => Err(self.not_object()),
        }
    }
}

/// data_query の `query`。
pub(crate) const DATA_QUERY: ObjectArg = ObjectArg {
    key: "query",
    shape: "filter・sort・select・limit などを持つオブジェクト",
    sql_arg: Some("sql"),
};

/// data_record_write の `data`。
pub(crate) const DATA_RECORD: ObjectArg = ObjectArg {
    key: "data",
    shape: "列の参照名 → 値",
    sql_arg: None,
};

/// save_skill の `knowledge_scope` と `model`。
pub(crate) const SKILL_DRAFT: [ObjectArg; 2] = [
    ObjectArg {
        key: "knowledge_scope",
        shape: "folders・files を持つオブジェクト",
        sql_arg: None,
    },
    ObjectArg {
        key: "model",
        shape: "model・temperature・max_tokens を持つオブジェクト",
        sql_arg: None,
    },
];

/// 入力の object の引数のうち、JSON の文字列で来たものを読んだ object に置き換える（省いた・null は触らない）。
///
/// 断るときは最初に当たった引数の文を返す（呼ぶ側が [`agent_core::ToolError::Invalid`] にする）。
pub(crate) fn accept_object_args(input: &mut Value, args: &[ObjectArg]) -> Result<(), String> {
    let Some(map) = input.as_object_mut() else {
        return Ok(());
    };
    for arg in args {
        let Some(slot) = map.get_mut(arg.key) else {
            continue;
        };
        if slot.is_null() {
            continue;
        }
        if let Some(parsed) = arg.read(slot)? {
            *slot = parsed;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const ARG: ObjectArg = ObjectArg {
        key: "data",
        shape: "列の参照名 → 値",
        sql_arg: None,
    };

    /// object・JSON の文字列（1 行・整形）は同じ object になり、省いた・null は触らない。
    #[test]
    fn objects_and_json_strings_of_one_are_the_same() {
        let data = json!({ "title": "交通費", "amount": 1200 });
        for given in [
            data.clone(),
            json!(data.to_string()),
            json!(serde_json::to_string_pretty(&data).unwrap()),
        ] {
            let mut input = json!({ "table": "経費", "data": given });
            accept_object_args(&mut input, &[ARG]).unwrap();
            assert_eq!(input, json!({ "table": "経費", "data": data }));
        }
        for untouched in [json!({ "table": "経費" }), json!({ "data": null })] {
            let mut input = untouched.clone();
            accept_object_args(&mut input, &[ARG]).unwrap();
            assert_eq!(input, untouched);
        }
    }

    /// 壊れた文字列は読めなくなった位置と直し方、配列・数・文字列の JSON の文字列と object でない値は直し方を言う。
    /// 文字列の JSON の文字列（2 段）は読まない。
    #[test]
    fn other_values_are_refused_with_how_to_fix() {
        let refused = |value: Value| {
            let mut input = json!({ "data": value });
            accept_object_args(&mut input, &[ARG]).unwrap_err()
        };
        let broken = refused(json!(r#"{"title": "交通費", "amount": }"#));
        assert!(
            broken.starts_with("data の文字列を JSON として読めませんでした（")
                && broken.contains("line 1 column")
                && broken.ends_with(&ARG.not_object()),
            "{broken}"
        );
        assert_eq!(
            ARG.not_object(),
            "data は JSON の文字列にせず、オブジェクトのまま渡してください（列の参照名 → 値）。"
        );
        let object = json!({ "title": "交通費" });
        for bad in [
            json!(format!("[{object}]")),
            json!(json!(object.to_string()).to_string()),
            json!("12"),
            json!([object.clone()]),
            json!(12),
            json!(true),
        ] {
            assert_eq!(refused(bad.clone()), ARG.not_object(), "{bad}");
        }
    }

    /// data_query の query に SQL の文を書いた（sql と取り違えた）ときは、JSON にしたことではなく sql に書くよう言う。
    /// SQL に見えない壊れた文字列は今までどおり。SQL を受ける引数の無い道具では言わない。
    #[test]
    fn sql_in_the_query_points_at_the_sql_arg() {
        let refused = |arg: ObjectArg, value: &str| {
            let mut input = json!({ arg.key: value });
            accept_object_args(&mut input, &[arg]).unwrap_err()
        };
        let expected =
            "query に SQL の文が入っています。SQL は query ではなく sql に書いてください\
             （query は filter・sort・select・limit などを持つオブジェクト）。";
        for sql in [
            "SELECT title FROM 経費",
            "  select title from 経費",
            "WITH a AS (SELECT 1) SELECT * FROM a",
        ] {
            assert_eq!(refused(DATA_QUERY, sql), expected, "{sql}");
        }
        let broken = refused(DATA_QUERY, r#"{"limit": }"#);
        assert!(
            broken.starts_with("query の文字列を JSON として読めませんでした（")
                && broken.ends_with(&DATA_QUERY.not_object()),
            "{broken}"
        );
        assert!(
            refused(ARG, "SELECT 1").starts_with("data の文字列を JSON として読めませんでした（")
        );
    }

    /// 入力が object でなければ何もしない（道具の型の検証に任せる）。複数の引数は順に見て、最初の誤りを返す。
    #[test]
    fn several_args_are_read_in_order() {
        let mut not_object = json!("x");
        accept_object_args(&mut not_object, &[ARG]).unwrap();
        assert_eq!(not_object, json!("x"));
        let other = ObjectArg {
            key: "model",
            shape: "model・temperature・max_tokens",
            sql_arg: None,
        };
        let mut input = json!({ "data": "{\"a\":1}", "model": "{\"model\":\"fast\"}" });
        accept_object_args(&mut input, &[ARG, other]).unwrap();
        assert_eq!(
            input,
            json!({ "data": { "a": 1 }, "model": { "model": "fast" } })
        );
        let mut bad = json!({ "data": "[1]", "model": "x" });
        assert!(accept_object_args(&mut bad, &[ARG, other])
            .unwrap_err()
            .starts_with("data は"));
    }
}
