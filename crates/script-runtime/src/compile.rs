//! shiki script のコンパイル（swc で TS→JS・型剥がし・禁止構文 lint）。
//!
//! 保存時検証 V6（ir.md §8・script.md §3）と実行前コンパイルで共用する。パイプライン:
//! [1] swc parse（構文）→ [2] AST lint（禁止構文・`main` 存在）→ [3] TS 型剥がし＋ES2020 emit。
//! ソースが正・`compiled_js` は導出物（`swc_version` を記録し更新時に再コンパイル）。

use swc_common::comments::SingleThreadedComments;
use swc_common::sync::Lrc;
use swc_common::{FileName, Globals, Mark, SourceMap, GLOBALS};
use swc_ecma_ast::{EsVersion, Module, ModuleItem, Pass, Program, Stmt};
use swc_ecma_codegen::text_writer::JsWriter;
use swc_ecma_codegen::Emitter;
use swc_ecma_parser::{lexer::Lexer, Parser, StringInput, Syntax, TsSyntax};
use swc_ecma_transforms_base::resolver;
use swc_ecma_transforms_typescript::strip;

/// inline script の上限（ir.md §7.6・V7）。
pub const MAX_SOURCE_BYTES: usize = 64 * 1024;

/// このコンパイラが記録する swc の版（更新時に再コンパイルの契機・script.md §3）。
pub const SWC_VERSION: &str = "swc-ecma-41";

/// コンパイル結果（導出物）。
#[derive(Debug, Clone)]
pub struct CompiledScript {
    /// ES2020 へ変換した JS（ゲストが評価する）。
    pub compiled_js: String,
    /// コンパイルに用いた swc 版。
    pub swc_version: &'static str,
}

/// コンパイル/検証エラー（保存時は V6 として返す）。
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompileError {
    #[error("script が大きすぎます（{0} bytes > {MAX_SOURCE_BYTES} bytes）")]
    TooLarge(usize),
    #[error("構文エラー: {0}")]
    Syntax(String),
    #[error("禁止構文: {0}")]
    Forbidden(String),
    #[error("script は function main(input) を定義する必要があります")]
    MissingMain,
    #[error("コード生成に失敗しました: {0}")]
    Codegen(String),
}

/// TS 風ソースを検証しつつ ES2020 の JS へコンパイルする。
pub fn compile(source: &str) -> Result<CompiledScript, CompileError> {
    if source.len() > MAX_SOURCE_BYTES {
        return Err(CompileError::TooLarge(source.len()));
    }
    let cm: Lrc<SourceMap> = Lrc::default();
    let fm = cm.new_source_file(
        Lrc::new(FileName::Custom("script.ts".into())),
        source.to_string(),
    );

    let comments = SingleThreadedComments::default();
    let lexer = Lexer::new(
        Syntax::Typescript(TsSyntax::default()),
        EsVersion::Es2020,
        StringInput::from(&*fm),
        Some(&comments),
    );
    let mut parser = Parser::new_from(lexer);
    let module = parser
        .parse_module()
        .map_err(|e| CompileError::Syntax(format!("{e:?}")))?;
    if let Some(err) = parser.take_errors().into_iter().next() {
        return Err(CompileError::Syntax(format!("{err:?}")));
    }

    lint_module(&module)?;

    // 型剥がし＋ES2020 emit は swc の Globals コンテキスト内で行う（Mark 採番のため必須）。
    let globals = Globals::default();
    let js = GLOBALS.set(&globals, || -> Result<String, CompileError> {
        let unresolved = Mark::new();
        let top_level = Mark::new();
        let mut program = Program::Module(module);
        resolver(unresolved, top_level, true).process(&mut program);
        strip(unresolved, top_level).process(&mut program);
        let module = match program {
            Program::Module(m) => m,
            Program::Script(_) => {
                return Err(CompileError::Codegen("script モードは未対応".into()))
            }
        };
        emit_module(&cm, &module)
    })?;

    Ok(CompiledScript {
        compiled_js: js,
        swc_version: SWC_VERSION,
    })
}

/// 禁止構文（import/export/async/await/with・トップレベル return）と `main` 存在を検査する。
fn lint_module(module: &Module) -> Result<(), CompileError> {
    let mut has_main = false;
    for item in &module.body {
        match item {
            // import / export はモジュール構文＝禁止（npm import 不可・script.md §2）。
            ModuleItem::ModuleDecl(_) => {
                return Err(CompileError::Forbidden(
                    "import / export は使用できません".into(),
                ))
            }
            ModuleItem::Stmt(stmt) => {
                if let Stmt::Decl(swc_ecma_ast::Decl::Fn(f)) = stmt {
                    if &*f.ident.sym == "main" {
                        has_main = true;
                    }
                }
                lint_stmt(stmt)?;
            }
        }
    }
    if !has_main {
        return Err(CompileError::MissingMain);
    }
    Ok(())
}

/// 文レベルの禁止構文（トップレベル return / with）。async/await は Visit で全走査する。
fn lint_stmt(stmt: &Stmt) -> Result<(), CompileError> {
    use swc_ecma_visit::{Visit, VisitWith};

    struct Forbid(Option<CompileError>);
    impl Visit for Forbid {
        fn visit_await_expr(&mut self, _: &swc_ecma_ast::AwaitExpr) {
            self.flag("await は使用できません（同期スタイルのみ）");
        }
        fn visit_with_stmt(&mut self, _: &swc_ecma_ast::WithStmt) {
            self.flag("with は使用できません");
        }
        fn visit_fn_decl(&mut self, f: &swc_ecma_ast::FnDecl) {
            if f.function.is_async {
                self.flag("async 関数は使用できません（同期スタイルのみ）");
            }
            f.function.visit_children_with(self);
        }
        fn visit_arrow_expr(&mut self, a: &swc_ecma_ast::ArrowExpr) {
            if a.is_async {
                self.flag("async 関数は使用できません（同期スタイルのみ）");
            }
            a.visit_children_with(self);
        }
        fn visit_fn_expr(&mut self, f: &swc_ecma_ast::FnExpr) {
            if f.function.is_async {
                self.flag("async 関数は使用できません（同期スタイルのみ）");
            }
            f.function.visit_children_with(self);
        }
    }
    impl Forbid {
        fn flag(&mut self, msg: &str) {
            if self.0.is_none() {
                self.0 = Some(CompileError::Forbidden(msg.to_string()));
            }
        }
    }

    // トップレベル return（関数外の return）。
    if matches!(stmt, Stmt::Return(_)) {
        return Err(CompileError::Forbidden(
            "トップレベル return は使用できません".into(),
        ));
    }
    let mut f = Forbid(None);
    stmt.visit_with(&mut f);
    match f.0 {
        Some(e) => Err(e),
        None => Ok(()),
    }
}

/// AST を JS 文字列へ emit する。
fn emit_module(cm: &Lrc<SourceMap>, module: &Module) -> Result<String, CompileError> {
    let mut buf = Vec::new();
    {
        let writer = JsWriter::new(cm.clone(), "\n", &mut buf, None);
        let mut emitter = Emitter {
            cfg: swc_ecma_codegen::Config::default().with_target(EsVersion::Es2020),
            cm: cm.clone(),
            comments: None,
            wr: writer,
        };
        emitter
            .emit_module(module)
            .map_err(|e| CompileError::Codegen(format!("{e}")))?;
    }
    String::from_utf8(buf).map_err(|e| CompileError::Codegen(format!("utf8: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_types_and_keeps_main() {
        let out = compile("function main(input: {n: number}): number { return input.n + 1; }")
            .expect("compile");
        assert!(out.compiled_js.contains("function main"));
        // 型注釈は剥がれている。
        assert!(!out.compiled_js.contains(": number"));
        assert_eq!(out.swc_version, SWC_VERSION);
    }

    #[test]
    fn requires_main() {
        assert!(matches!(
            compile("function helper() { return 1; }"),
            Err(CompileError::MissingMain)
        ));
    }

    #[test]
    fn rejects_import_export() {
        assert!(matches!(
            compile("import x from 'y'; function main() { return 1; }"),
            Err(CompileError::Forbidden(_))
        ));
        assert!(matches!(
            compile("export const z = 1; function main() { return 1; }"),
            Err(CompileError::Forbidden(_))
        ));
    }

    #[test]
    fn rejects_async_await() {
        assert!(matches!(
            compile("async function main() { return 1; }"),
            Err(CompileError::Forbidden(_))
        ));
        assert!(matches!(
            compile("function main() { async function g(){}; return 1; }"),
            Err(CompileError::Forbidden(_))
        ));
        assert!(matches!(
            compile("function main() { const f = async () => 1; return f; }"),
            Err(CompileError::Forbidden(_))
        ));
    }

    #[test]
    fn rejects_top_level_return_and_with() {
        // トップレベル return は swc がパースエラー（Syntax）にするか lint（Forbidden）で弾く。
        // どちらでも「拒否される」ことが要件。
        assert!(matches!(
            compile("return 1;"),
            Err(CompileError::Syntax(_) | CompileError::Forbidden(_))
        ));
        // with は TS パーサ（常に strict）が Syntax エラーにするか lint で禁止する。
        assert!(matches!(
            compile("function main(o) { with (o) { return x; } }"),
            Err(CompileError::Syntax(_) | CompileError::Forbidden(_))
        ));
    }

    #[test]
    fn rejects_syntax_error() {
        assert!(matches!(
            compile("function main( { return }"),
            Err(CompileError::Syntax(_))
        ));
    }

    #[test]
    fn rejects_too_large() {
        let big = format!(
            "function main() {{ return \"{}\"; }}",
            "x".repeat(MAX_SOURCE_BYTES)
        );
        assert!(matches!(compile(&big), Err(CompileError::TooLarge(_))));
    }

    #[test]
    fn allows_helpers_and_consts() {
        let out = compile(
            "const K = 3;\nfunction helper(x) { return x * K; }\nfunction main(input) { return helper(input.n); }",
        )
        .expect("compile");
        assert!(out.compiled_js.contains("function main"));
        assert!(out.compiled_js.contains("helper"));
    }
}
