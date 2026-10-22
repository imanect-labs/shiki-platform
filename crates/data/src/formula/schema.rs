//! [`FormulaExpr`] の OpenAPI スキーマ（再帰する外部タグ付き enum を utoipa の derive が扱えないため、
//! serde の直列化形と同じ形をここで組み立てる）。演算子の語彙（`RollupOp` / `FormulaCmpOp`）は派生
//! スキーマを参照し、手書きの対応物を作らない。

use std::borrow::Cow;

use utoipa::openapi::schema::{
    ArrayBuilder, ObjectBuilder, OneOfBuilder, Schema, SchemaType, Type,
};
use utoipa::openapi::{Ref, RefOr};
use utoipa::{PartialSchema, ToSchema};

use super::ast::{FormulaCmpOp, FormulaExpr};
use crate::derived::types::RollupOp;

fn expr() -> RefOr<Schema> {
    Ref::from_schema_name("FormulaExpr").into()
}

fn string() -> RefOr<Schema> {
    ObjectBuilder::new()
        .schema_type(Type::String)
        .build()
        .into()
}

/// `{ "<tag>": <value> }`（外部タグ）。
fn tagged(tag: &str, value: impl Into<RefOr<Schema>>) -> RefOr<Schema> {
    ObjectBuilder::new()
        .property(tag, value)
        .required(tag)
        .build()
        .into()
}

/// 要素数 `n` の配列（`items` は要素のいずれか）。
fn tuple(items: RefOr<Schema>, min: usize, max: Option<usize>) -> RefOr<Schema> {
    ArrayBuilder::new()
        .items(items)
        .min_items(Some(min))
        .max_items(max)
        .build()
        .into()
}

fn either(a: RefOr<Schema>, b: RefOr<Schema>) -> RefOr<Schema> {
    RefOr::T(Schema::OneOf(OneOfBuilder::new().item(a).item(b).build()))
}

impl PartialSchema for FormulaExpr {
    fn schema() -> RefOr<Schema> {
        let pair = || tuple(expr(), 2, Some(2));
        let list = || tuple(expr(), 1, None);
        let integer = || -> RefOr<Schema> {
            ObjectBuilder::new()
                .schema_type(Type::Integer)
                .minimum(Some(0))
                .build()
                .into()
        };
        let switch = ObjectBuilder::new()
            .property("value", expr())
            .required("value")
            .property(
                "cases",
                tuple(
                    ObjectBuilder::new()
                        .property("when", expr())
                        .required("when")
                        .property("then", expr())
                        .required("then")
                        .build()
                        .into(),
                    1,
                    None,
                ),
            )
            .required("cases")
            .property("default", expr())
            .build();
        let rollup = ObjectBuilder::new()
            .property("link", string())
            .required("link")
            .property("target", string())
            .property("op", RollupOp::name().as_ref().to_string().pipe_ref())
            .required("op")
            .build();
        let one_of = OneOfBuilder::new()
            .description(Some(
                "数式（閉じた AST・外部タグ形式。例: {\"mul\": [{\"field\": \"price\"}, {\"lit\": 3}]}）",
            ))
            .item(tagged("field", string()))
            .item(tagged(
                "lit",
                ObjectBuilder::new().schema_type(SchemaType::AnyValue).build(),
            ))
            .item(tagged("add", pair()))
            .item(tagged("sub", pair()))
            .item(tagged("mul", pair()))
            .item(tagged("div", pair()))
            .item(tagged("round", tuple(either(expr(), integer()), 2, Some(2))))
            .item(tagged("concat", list()))
            .item(tagged("upper", expr()))
            .item(tagged("lower", expr()))
            .item(tagged("date_add", pair()))
            .item(tagged("date_diff", pair()))
            .item(RefOr::T(
                ObjectBuilder::new()
                    .schema_type(Type::String)
                    .enum_values(Some(["now"]))
                    .build()
                    .into(),
            ))
            .item(tagged("if", tuple(expr(), 3, Some(3))))
            .item(tagged("switch", switch))
            .item(tagged("and", list()))
            .item(tagged("or", list()))
            .item(tagged("not", expr()))
            .item(tagged(
                "cmp",
                tuple(
                    either(expr(), Ref::from_schema_name(FormulaCmpOp::name()).into()),
                    3,
                    Some(3),
                ),
            ))
            .item(tagged("rollup", rollup))
            .build();
        RefOr::T(Schema::OneOf(one_of))
    }
}

/// 参照名からスキーマ参照を作る。
trait PipeRef {
    fn pipe_ref(self) -> RefOr<Schema>;
}

impl PipeRef for String {
    fn pipe_ref(self) -> RefOr<Schema> {
        Ref::from_schema_name(self).into()
    }
}

impl ToSchema for FormulaExpr {
    fn name() -> Cow<'static, str> {
        Cow::Borrowed("FormulaExpr")
    }

    fn schemas(schemas: &mut Vec<(String, RefOr<Schema>)>) {
        schemas.push((RollupOp::name().into_owned(), RollupOp::schema()));
        schemas.push((FormulaCmpOp::name().into_owned(), FormulaCmpOp::schema()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_is_a_closed_one_of_with_refs() {
        let s = serde_json::to_string(&FormulaExpr::schema()).unwrap();
        for tag in [
            "\"field\"",
            "\"rollup\"",
            "\"switch\"",
            "\"now\"",
            "#/components/schemas/FormulaExpr",
        ] {
            assert!(s.contains(tag), "{tag}: {s}");
        }
        let mut v = Vec::new();
        FormulaExpr::schemas(&mut v);
        assert!(v.iter().any(|(n, _)| n == "RollupOp"));
    }
}
