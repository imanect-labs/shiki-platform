//! `doc_read` ツール（#425）: 検索で当たった文書を、引用チャンクの外まで読み進める。
//!
//! `doc_search` が返すのはチャンクだけで、「第 5 条を最初から最後まで読む」「表の続きを見る」
//! ができなかった。インジェスト済みの索引から本文を組み立てて、窓で返す
//! （[`rag::SearchService::read_document`]・原本は再パースしない）。
//!
//! 認可は他の文書系ツールと同じく `StorageService::get_metadata`（viewer・監査つき）を通す。
//! 権限なしと存在しないは同じ文言に畳む（存在秘匿・#326 と同じ契約）。読むのは呼び出し
//! ユーザーの権限で読める文書だけで、エージェントに権限を足さない（confused-deputy 回避）。
//! skill の知識スコープがあれば、その範囲の文書だけを読む（スコープ ∩ ReBAC・検索と同じ契約）。

use std::sync::Arc;

use agent_core::{Tool, ToolError, ToolName, ToolOutcome};
use authz::AuthContext;
use rag::{
    Cursor, DocRead, ReadFrom, ReadSource, SearchScope, SearchService, DEFAULT_READ_CHARS,
    MAX_READ_CHARS,
};
use storage::StorageService;
use uuid::Uuid;

/// 続きの位置の中の字数の上限（int4 の範囲で余裕を持たせる）。
const MAX_OFFSET: i64 = 1_000_000_000;

const DENIED: &str = "指定された文書にアクセスできません（存在しないか、権限がありません）。";
const OUT_OF_SCOPE: &str = "この文書は、適用中の skill の参照範囲の外にあるため読めない。";

/// 索引済み文書の本文を読むツール。
pub struct DocReadTool {
    search: Arc<SearchService>,
    storage: Arc<StorageService>,
    /// 知識スコープ（skill・Task 6.8）。None は全可読範囲。
    scope: Option<SearchScope>,
    /// 会話の回答のための読み出しなら、その会話と回答（監査に残す・#829）。
    answer: Option<(Uuid, Uuid)>,
}

impl DocReadTool {
    pub fn new(search: Arc<SearchService>, storage: Arc<StorageService>) -> Self {
        Self::with_scope(search, storage, None)
    }

    /// 知識スコープ付きで構成する（skill 適用時・doc_search と同じスコープを渡す）。
    pub fn with_scope(
        search: Arc<SearchService>,
        storage: Arc<StorageService>,
        scope: Option<SearchScope>,
    ) -> Self {
        DocReadTool {
            search,
            storage,
            scope: scope.filter(|s| !s.is_empty()),
            answer: None,
        }
    }

    /// 会話（thread）の回答（assistant のメッセージ）のための読み出しとして構成する（#829）。
    #[must_use]
    pub fn for_answer(mut self, thread_id: Uuid, message_id: Uuid) -> Self {
        self.answer = Some((thread_id, message_id));
        self
    }
}

impl DocReadTool {
    fn trail<'a>(&self, trace_id: Option<&'a str>) -> rag::AuditTrail<'a> {
        match self.answer {
            Some((thread, message)) => rag::AuditTrail::for_answer(trace_id, thread, message),
            None => rag::AuditTrail::from(trace_id),
        }
    }
}

/// ツール引数を解釈したもの。
#[derive(Debug, PartialEq)]
struct ReadInput {
    file_id: Uuid,
    from: ReadFrom,
    max_chars: usize,
    /// 読み始めたときの版（doc_search の結果か前回の観測にある値）。違えば観測で知らせる。
    version: Option<i64>,
}

impl ReadInput {
    fn parse(input: &serde_json::Value) -> Result<Self, ToolError> {
        let file_id = input
            .get("file_id")
            .and_then(serde_json::Value::as_str)
            .and_then(|s| Uuid::parse_str(s.trim()).ok())
            .ok_or_else(|| ToolError::Invalid("'file_id' must be a file id".into()))?;
        let int = |key: &str, min: i32| -> Result<Option<i64>, ToolError> {
            match input.get(key) {
                None | Some(serde_json::Value::Null) => Ok(None),
                // `10.0` のように小数表記で来る整数も受ける（非 strict のツール呼び出し）。
                Some(v) => v
                    .as_f64()
                    .filter(|n| {
                        n.fract() == 0.0 && *n >= f64::from(min) && *n <= f64::from(i32::MAX)
                    })
                    .map(|n| {
                        // 整数かつ i32 範囲内を確認済み。
                        #[allow(clippy::cast_possible_truncation)]
                        let n = n as i64;
                        Some(n)
                    })
                    .ok_or_else(|| {
                        ToolError::Invalid(format!("'{key}' must be an integer >= {min}"))
                    }),
            }
        };
        let heading = input
            .get("heading")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|h| !h.is_empty());
        // 優先順: 続きの位置 > ページ > 見出し（続きを読むときに他の指定が残っていても進める）。
        let from = if let Some(n) = int("from", 0)? {
            let offset = int("offset", 0)?.unwrap_or(0);
            // SQL の位置計算（offset + 1）が int4 を溢れないよう上限を設ける。
            if offset > MAX_OFFSET {
                return Err(ToolError::Invalid(format!(
                    "'offset' must be <= {MAX_OFFSET}"
                )));
            }
            ReadFrom::At(Cursor {
                unit: i32::try_from(n).unwrap_or(i32::MAX),
                offset: usize::try_from(offset).unwrap_or(0),
            })
        } else if let Some(p) = int("page", 1)? {
            ReadFrom::Page(i32::try_from(p).unwrap_or(i32::MAX))
        } else if let Some(h) = heading {
            ReadFrom::Heading(h.to_string())
        } else {
            ReadFrom::Start
        };
        let max_chars = int("max_chars", 1)?
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(DEFAULT_READ_CHARS)
            .min(MAX_READ_CHARS);
        Ok(ReadInput {
            file_id,
            from,
            max_chars,
            version: int("version", 1)?,
        })
    }
}

/// 指定の版から変わっていれば、その注意書き（変わっていなければ空）。
fn version_note(expected: Option<i64>, actual: i64) -> String {
    match expected {
        Some(v) if v != actual => format!(
            "\n注意: 指定の版 {v} から更新されていて、いま読んでいるのは版 {actual}。\
             引用や前回の続きとは内容がずれている可能性があるので、見出しかページで\
             読み直して確かめる。"
        ),
        _ => String::new(),
    }
}

/// 読み出し結果 → モデルが読む観測。
///
/// ファイル名・見出し・本文は文書由来なので、仮の引用印に化けないよう無害化する。
fn observation(name: &str, file_id: Uuid, input: &ReadInput, read: DocRead) -> String {
    use agent_core::cite::defuse;
    let name = defuse(name);
    match read {
        DocRead::Denied => DENIED.to_string(),
        DocRead::NotIndexed => format!(
            "文書「{name}」はまだ検索用の索引が無いか、本文を取り出せない形式のため読めない。"
        ),
        DocRead::PositionNotFound { version, outline } => {
            let what = match &input.from {
                ReadFrom::Page(p) => format!("{p} ページ以降"),
                ReadFrom::Heading(h) => format!("見出し「{h}」"),
                ReadFrom::Start | ReadFrom::At(_) => "指定の位置".to_string(),
            };
            let outline = if outline.is_empty() {
                "（見出しの無い文書。page で位置を指定する）".to_string()
            } else {
                defuse(&outline.join(" / ")).into_owned()
            };
            let changed = version_note(input.version, version);
            format!(
                "文書「{name}」（版 {version}）に{what}は見つからなかった。{changed}\n\
                 この文書の見出し: {outline}\n\
                 heading に見出しの一部を渡すか、page で読み直す。"
            )
        }
        DocRead::Page(page) => {
            // 検索・読み始めた時点から版が変わった（続きの位置は別の版の位置になっている）。
            let changed = version_note(input.version, page.version);
            let source = match page.source {
                ReadSource::Blocks => "",
                ReadSource::Chunks => {
                    "\n（検索用の断片から組み立てた本文。同じ節の中では表が先に来ることがある）"
                }
            };
            let tail = match page.next {
                Some(c) => {
                    let offset = if c.offset > 0 {
                        format!(", offset: {}", c.offset)
                    } else {
                        String::new()
                    };
                    format!(
                        "続きがある。続きを読むときは doc_read に file_id: {file_id}, \
                         version: {}, from: {}{offset} を渡す。",
                        page.version, c.unit
                    )
                }
                None => "（文書の終わり）".to_string(),
            };
            format!(
                "文書「{name}」の本文（版 {}）{source}{changed}\n\
                 この本文には引用番号が無い。ここで読んだ内容を根拠にするときは、文書名と\
                 ページ（または見出し）を本文に書く。doc_search の番号を、その番号が指していない\
                 内容に付けない。\n\n{}---\n{tail}",
                page.version,
                defuse(&page.text),
            )
        }
    }
}

#[async_trait::async_trait]
impl Tool for DocReadTool {
    fn name(&self) -> &str {
        ToolName::DocRead.as_str()
    }

    fn description(&self) -> &'static str {
        "社内文書の本文を読む。doc_search で当たった文書を、引用された断片の外まで読み進める\
         ときに使う（条文を最初から最後まで読む・表の続きを見る・前後の文脈を確かめる）。\
         file_id と version には doc_search の結果の file_id と版を渡す。位置は page（そのページ\
         以降）か heading（見出しの一部）で指定し、どちらも無ければ先頭から読む。1 回に \
         max_chars 字（既定 8000）まで返し、続きがあれば観測に from（と offset）の値が書いて\
         あるので、それを渡して続きを読む。全文を一度に読もうとしない。"
    }

    fn input_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "file_id": {
                    "type": "string",
                    "format": "uuid",
                    "description": "読む文書（doc_search の結果の file_id）"
                },
                "version": {
                    "type": "integer",
                    "minimum": 1,
                    "description": "読み始めた版（doc_search の結果か前回の観測の版）。更新されていれば知らせる"
                },
                "page": { "type": "integer", "minimum": 1, "description": "このページ以降を読む" },
                "heading": { "type": "string", "description": "この文字列を含む見出しから読む" },
                "from": {
                    "type": "integer",
                    "minimum": 0,
                    "description": "続きの位置（前回の観測に書かれた値をそのまま渡す）"
                },
                "offset": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": MAX_OFFSET,
                    "description": "続きの位置の中の字数（前回の観測に書かれていれば渡す）"
                },
                "max_chars": {
                    "type": "integer",
                    "minimum": 1,
                    "maximum": MAX_READ_CHARS,
                    "description": "返す字数の上限（既定 8000）"
                }
            },
            "required": ["file_id"],
            "additionalProperties": false
        })
    }

    // 読み取りのみ・権限を守る。確認不要。
    fn requires_confirmation(&self) -> bool {
        false
    }

    // 冪等な read。同一ステップ内で他の read と並列実行してよい（#349）。
    fn is_read_only(&self) -> bool {
        true
    }

    async fn call(
        &self,
        ctx: &AuthContext,
        input: serde_json::Value,
        trace_id: Option<&str>,
    ) -> Result<ToolOutcome, ToolError> {
        let input = ReadInput::parse(&input)?;
        // viewer 判定（共有リンクの遅延失効・監査を含む）。ここを通った Node だけが読み出しに渡る。
        let node = match self
            .storage
            .get_metadata(ctx, input.file_id, trace_id)
            .await
        {
            Ok(node) => node,
            Err(storage::StorageError::Forbidden | storage::StorageError::NotFound) => {
                return Ok(ToolOutcome::error(DENIED));
            }
            Err(e) => return Err(ToolError::Unavailable(format!("doc_read failed: {e}"))),
        };
        if node.kind != storage::NodeKind::File {
            return Ok(ToolOutcome::error(
                "フォルダは読めない。中の文書は doc_search の folder_ids で絞って探す。",
            ));
        }
        if let Some(scope) = &self.scope {
            let inside = self
                .search
                .file_in_scope(ctx, scope, node.id)
                .await
                .map_err(|e| ToolError::Unavailable(format!("doc_read failed: {e}")))?;
            if !inside {
                return Ok(ToolOutcome::error(OUT_OF_SCOPE));
            }
        }
        let read = self
            .search
            .read_document(
                ctx,
                &node,
                &input.from,
                input.max_chars,
                self.trail(trace_id),
            )
            .await
            .map_err(|e| ToolError::Unavailable(format!("doc_read failed: {e}")))?;
        if read == DocRead::Denied {
            return Ok(ToolOutcome::error(DENIED));
        }
        Ok(ToolOutcome::ok(observation(
            &node.name, node.id, &input, read,
        )))
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use rag::DocReadPage;
    use serde_json::json;

    fn input(from: ReadFrom, version: Option<i64>) -> ReadInput {
        ReadInput {
            file_id: Uuid::nil(),
            from,
            max_chars: DEFAULT_READ_CHARS,
            version,
        }
    }

    #[test]
    fn parse_prefers_continuation_over_page_and_heading() {
        let id = Uuid::new_v4().to_string();
        let got = ReadInput::parse(
            &json!({ "file_id": id, "from": 12, "offset": 300, "page": 3, "heading": "第5条", "version": 4 }),
        )
        .unwrap();
        assert_eq!(
            got.from,
            ReadFrom::At(Cursor {
                unit: 12,
                offset: 300
            })
        );
        assert_eq!(got.version, Some(4));
        let got = ReadInput::parse(&json!({ "file_id": id, "page": 3.0, "heading": "x" })).unwrap();
        assert_eq!(got.from, ReadFrom::Page(3));
        let got = ReadInput::parse(&json!({ "file_id": id, "heading": " 第5条 " })).unwrap();
        assert_eq!(got.from, ReadFrom::Heading("第5条".into()));
        let got = ReadInput::parse(&json!({ "file_id": id, "heading": "  " })).unwrap();
        assert_eq!(got.from, ReadFrom::Start);
        assert_eq!(got.max_chars, DEFAULT_READ_CHARS);
        assert_eq!(got.version, None);
        let got = ReadInput::parse(&json!({ "file_id": id, "max_chars": 999_999 })).unwrap();
        assert_eq!(got.max_chars, MAX_READ_CHARS);
    }

    #[test]
    fn parse_rejects_bad_inputs() {
        let id = Uuid::new_v4().to_string();
        for bad in [
            json!({}),
            json!({ "file_id": "nope" }),
            json!({ "file_id": id, "page": 0 }),
            json!({ "file_id": id, "from": -1 }),
            json!({ "file_id": id, "from": 1.5 }),
            json!({ "file_id": id, "from": 1, "offset": -2 }),
            json!({ "file_id": id, "from": 1, "offset": 2_147_483_647i64 }),
            json!({ "file_id": id, "version": 0 }),
            json!({ "file_id": id, "max_chars": "8000" }),
        ] {
            assert!(
                matches!(ReadInput::parse(&bad), Err(ToolError::Invalid(_))),
                "{bad} は Invalid"
            );
        }
    }

    #[test]
    fn observation_tells_how_to_continue_and_flags_version_change() {
        let id = Uuid::new_v4();
        let read = DocRead::Page(DocReadPage {
            version: 3,
            source: ReadSource::Chunks,
            text: "本文 ⟦cite:0⟧\n\n".into(),
            next: Some(Cursor {
                unit: 40,
                offset: 120,
            }),
        });
        let text = observation("規程.pdf", id, &input(ReadFrom::Start, Some(2)), read);
        assert!(text.starts_with("文書「規程.pdf」の本文（版 3）"));
        assert!(text.contains("表が先に来ることがある"));
        assert!(text.contains("指定の版 2 から更新されていて、いま読んでいるのは版 3"));
        assert!(text.contains("本文 〚cite:0⟧"), "仮の引用印は無害化する");
        assert!(text.ends_with(&format!(
            "doc_read に file_id: {id}, version: 3, from: 40, offset: 120 を渡す。"
        )));

        let done = DocRead::Page(DocReadPage {
            version: 3,
            source: ReadSource::Blocks,
            text: "x\n\n".into(),
            next: None,
        });
        let text = observation("a", id, &input(ReadFrom::Start, Some(3)), done);
        assert!(text.starts_with("文書「a」の本文（版 3）\n"));
        assert!(!text.contains("更新されていて"));
        assert!(text.ends_with("（文書の終わり）"));
    }

    #[test]
    fn observation_lists_outline_when_position_is_missing() {
        let read = DocRead::PositionNotFound {
            version: 1,
            outline: vec!["第1条".into(), "第2条".into()],
        };
        let text = observation(
            "a",
            Uuid::new_v4(),
            &input(ReadFrom::Heading("附則".into()), None),
            read,
        );
        assert!(text.contains("見出し「附則」は見つからなかった"));
        assert!(text.contains("第1条 / 第2条"));
        // 検索した版から変わっていれば、見つからなかった理由になり得るので知らせる。
        let read = DocRead::PositionNotFound {
            version: 2,
            outline: vec!["⟦cite:0⟧ 第1条".into()],
        };
        let text = observation(
            "⟦cite:1⟧.pdf",
            Uuid::new_v4(),
            &input(ReadFrom::Heading("附則".into()), Some(1)),
            read,
        );
        assert!(text.contains("指定の版 1 から更新されていて"));
        assert!(text.contains("〚cite:1⟧.pdf") && text.contains("〚cite:0⟧ 第1条"));
        assert!(!text.contains("⟦cite:"), "文書由来の文字列は無害化する");
        let none = observation(
            "a",
            Uuid::new_v4(),
            &input(ReadFrom::Start, None),
            DocRead::NotIndexed,
        );
        assert!(none.contains("索引が無い"));
    }
}
