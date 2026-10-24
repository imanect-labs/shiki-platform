//! 索引済み文書の本文読み出し（`doc_read` ツールの下回り・#425）。
//!
//! 検索で当たった文書を、引用チャンクの外まで読み進めるためのもの。**原本を再パースせず**、
//! インジェスト済みの索引から組み立てる（同期経路にパースを持ち込まない・巨大入力をホストへ
//! 持ち込まない）。
//!
//! - 版ごとのブロック列（`doc_block`・#508）があればそれを読む。見出しの階層・箇条書きの記号・
//!   ページがそのまま残る。
//! - 無ければ（#508 より前に取り込んだ索引）`rag_chunk` の leaf / table を ordinal 順に連結する。
//!   leaf は段落を詰めたもので重なりは無い。parent はセクション全文の写しなので含めない。
//!   同じ節の中では表が段落より先に番号を振られているため、表が節の頭に来ることがある。
//! - 読むのは索引にある**最新版**だけ（旧版は `delete_stale_versions` で消える）。
//!
//! **認可は呼び出し側が StorageService で済ませた証拠（[`storage::Node`]）を受け取る形にする**
//! （[`super::blocks`] と同じ。ここで独自の権限判定はしない）。加えて tenant と org で絞る。
//! 本文を読み出したことは監査に残す（検索を経由しない本文の経路になるため）。

use std::fmt::Write as _;
use storage::audit::AuditAction;

use authz::AuthContext;
use storage::audit::{AuditEntry, Decision};

use crate::authz_filter::readable_set;
use crate::error::RagError;
use crate::search::SearchService;

/// 1 回に読む字数の既定と上限。全文一括はコンテキストを壊すので窓で返す。
pub const DEFAULT_READ_CHARS: usize = 8_000;
pub const MAX_READ_CHARS: usize = 20_000;
/// 1 回の SQL で引く単位数の上限（実際は窓の字数から見積もる・[`SearchService::units`]）。
const UNITS_PER_QUERY: i64 = 400;
/// 位置が見つからなかったときに返す見出しの数。
const OUTLINE_LIMIT: i64 = 40;

/// 読み始める位置。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadFrom {
    /// 文書の先頭。
    Start,
    /// 前回の観測にある続きの位置（[`DocReadPage::next`]）。
    At(Cursor),
    /// このページ以降の最初の箇所。
    Page(i32),
    /// 見出しに、この文字列を含む最初の箇所（大文字小文字を区別しない）。
    Heading(String),
}

/// 続きの位置: 単位（ブロック／チャンク）の ordinal と、その単位の本文の何字目から読むか。
///
/// 1 単位だけで窓に収まらない（巨大な表など）ときは、同じ単位の途中から続きを読む。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub unit: i32,
    pub offset: usize,
}

/// 本文をどこから組み立てたか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadSource {
    /// 版のブロック列（`doc_block`）。
    Blocks,
    /// 検索用チャンク（`rag_chunk` の leaf / table）。
    Chunks,
}

/// 読み出した窓。
#[derive(Debug, Clone, PartialEq)]
pub struct DocReadPage {
    /// 読んだ索引の版。
    pub version: i64,
    pub source: ReadSource,
    /// 窓の本文（見出し・ページの印つき）。
    pub text: String,
    /// 続きがあれば、次に [`ReadFrom::At`] へ渡す位置。
    pub next: Option<Cursor>,
}

/// 読み出しの結果。
#[derive(Debug, Clone, PartialEq)]
pub enum DocRead {
    /// 索引が無い（インジェスト前・未対応形式・解析失敗）。
    NotIndexed,
    /// 可読集合（pre-filter）で読めない。呼び出し側は権限なしと同じ扱いにする。
    Denied,
    /// 指定のページ・見出しが見つからなかった。`outline` は文書の見出し（先頭から）。
    PositionNotFound {
        version: i64,
        outline: Vec<String>,
    },
    Page(DocReadPage),
}

/// 窓に積む 1 単位（ブロックかチャンク）。
#[derive(sqlx::FromRow)]
struct Unit {
    ordinal: i32,
    /// ブロックの型（heading / list_item …）。チャンクでは kind（leaf / table）。
    kind: String,
    level: Option<i32>,
    list_marker: Option<String>,
    page: Option<i32>,
    /// チャンク由来の節の見出し（`a > b`・文書由来なので SQL で 200 字に切って引く）。
    /// ブロック由来は None。
    heading: Option<String>,
    /// 本文のうち、続きの位置から窓に入り得る分だけ（SQL の substr で切って引く）。
    text: String,
    /// 続きの位置から単位の末尾までの字数（`text` はこの先頭の一部のことがある）。
    remaining: i32,
}

impl SearchService {
    /// `node` の索引済み最新版を、`from` の位置から `max_chars` 字ほど読む。
    pub async fn read_document(
        &self,
        ctx: &AuthContext,
        node: &storage::Node,
        from: &ReadFrom,
        max_chars: usize,
        trace_id: Option<&str>,
    ) -> Result<DocRead, RagError> {
        let max_chars = max_chars.clamp(1, MAX_READ_CHARS);
        // 索引にある最新版と、その版にブロック列があるか（1 本の SQL で引く）。
        let Some((version, has_blocks)) = sqlx::query_as::<_, (i64, bool)>(
            "select c.version, exists(select 1 from doc_block b \
                 where b.tenant_id = $1 and b.org = $2 and b.node_id = $3 \
                   and b.version = c.version) \
             from rag_chunk c \
             where c.tenant_id = $1 and c.org = $2 and c.node_id = $3 \
             order by c.version desc limit 1",
        )
        .bind(&ctx.tenant_id)
        .bind(&ctx.org)
        .bind(node.id)
        .fetch_optional(&self.pool)
        .await?
        else {
            return Ok(DocRead::NotIndexed);
        };
        let source = if has_blocks {
            ReadSource::Blocks
        } else {
            ReadSource::Chunks
        };
        // 二段 authz の pre-filter 側: 呼び出し側の viewer 判定（post-filter 相当）とは独立に、
        // 検索と同じ可読集合でこのファイルのチャンクの authz_tags を照合する。片方が誤って
        // 許可しても本文は返さない。可読集合が上限を超えて縮退したときは検索と同じく
        // tenant 境界だけになる（viewer 判定が最終の防壁）。
        let readable =
            readable_set(ctx, self.authz.as_ref(), self.config.readable_tags_max).await?;
        if !readable.overflowed {
            let allowed: bool = sqlx::query_scalar(
                "select exists(select 1 from rag_chunk \
                 where tenant_id = $1 and org = $2 and node_id = $3 and version = $4 \
                   and authz_tags && $5)",
            )
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .bind(node.id)
            .bind(version)
            .bind(&readable.tags)
            .fetch_one(&self.pool)
            .await?;
            if !allowed {
                return Ok(DocRead::Denied);
            }
        }

        let source_name = match source {
            ReadSource::Blocks => "blocks",
            ReadSource::Chunks => "chunks",
        };
        let Some(start) = self.locate(ctx, node, version, source, from).await? else {
            // 見出し一覧も文書の中身の一部なので、読み出しとして監査に残す。
            let outline = self.outline(ctx, node, version, source).await?;
            self.audit_read(
                ctx,
                node,
                trace_id,
                serde_json::json!({
                    "version": version,
                    "source": source_name,
                    "outline": outline.len(),
                }),
            )
            .await?;
            return Ok(DocRead::PositionNotFound { version, outline });
        };
        let skip = match from {
            ReadFrom::At(c) => c.offset,
            _ => 0,
        };
        let (units, more) = self
            .units(ctx, node, version, source, start, skip, max_chars)
            .await?;
        let (text, next) = render(&units, source, max_chars, skip, more);

        let first = units
            .first()
            .map(|u| [u.ordinal, i32::try_from(skip).unwrap_or(i32::MAX)]);
        self.audit_read(
            ctx,
            node,
            trace_id,
            serde_json::json!({
                "version": version,
                "source": source_name,
                "from": first,
                "next": next.map(|c| [c.unit, i32::try_from(c.offset).unwrap_or(i32::MAX)]),
                "chars": text.chars().count(),
            }),
        )
        .await?;
        Ok(DocRead::Page(DocReadPage {
            version,
            source,
            text,
            next,
        }))
    }

    /// 本文の読み出しを監査に残す（検索を経由しない本文の経路・#425）。
    async fn audit_read(
        &self,
        ctx: &AuthContext,
        node: &storage::Node,
        trace_id: Option<&str>,
        metadata: serde_json::Value,
    ) -> Result<(), RagError> {
        self.audit
            .record(
                ctx,
                AuditEntry {
                    action: AuditAction::RagDocRead,
                    object_type: "file",
                    object_id: &node.id.to_string(),
                    decision: Decision::Allow,
                    trace_id,
                    metadata,
                },
            )
            .await?;
        Ok(())
    }

    /// 読み始める単位の ordinal（見つからなければ None）。
    async fn locate(
        &self,
        ctx: &AuthContext,
        node: &storage::Node,
        version: i64,
        source: ReadSource,
        from: &ReadFrom,
    ) -> Result<Option<i32>, RagError> {
        let (table, scope) = source_sql(source);
        let found: Option<i32> = match from {
            ReadFrom::Start => return Ok(Some(0)),
            ReadFrom::At(c) => return Ok(Some(c.unit.max(0))),
            ReadFrom::Page(page) => {
                sqlx::query_scalar(&format!(
                    "select min(ordinal) from {table} \
                     where tenant_id = $1 and org = $2 and node_id = $3 and version = $4 \
                       and {scope} and page >= $5"
                ))
                .bind(&ctx.tenant_id)
                .bind(&ctx.org)
                .bind(node.id)
                .bind(version)
                .bind(page)
                .fetch_one(&self.pool)
                .await?
            }
            ReadFrom::Heading(heading) => {
                let pattern = format!("%{}%", escape_like(heading.trim()));
                let column = match source {
                    ReadSource::Blocks => "text",
                    ReadSource::Chunks => "array_to_string(heading_path, ' > ')",
                };
                let heading_only = match source {
                    ReadSource::Blocks => "type = 'heading'",
                    ReadSource::Chunks => "kind in ('leaf', 'table')",
                };
                sqlx::query_scalar(&format!(
                    "select min(ordinal) from {table} \
                     where tenant_id = $1 and org = $2 and node_id = $3 and version = $4 \
                       and {heading_only} and {column} ilike $5 escape '\\'"
                ))
                .bind(&ctx.tenant_id)
                .bind(&ctx.org)
                .bind(node.id)
                .bind(version)
                .bind(pattern)
                .fetch_one(&self.pool)
                .await?
            }
        };
        Ok(found)
    }

    /// `start` 以降の単位を ordinal 順に引く。
    ///
    /// 件数は窓の字数から見積もる（全部引いてから捨てない）。見積もりが外れて足りなければ、
    /// 窓が短くなり続きの位置が返るだけで、読み落としは起きない（[`render`]）。本文も
    /// 続きの位置から窓の字数＋1 字だけを SQL の `substr` で切って引く（巨大な 1 ブロックを
    /// 毎回まるごと転送・展開しない）。字数は `char_length` で別に返す。
    #[allow(clippy::too_many_arguments)] // 読み出し位置の束（呼び出しは read_document の 1 箇所）。
    async fn units(
        &self,
        ctx: &AuthContext,
        node: &storage::Node,
        version: i64,
        source: ReadSource,
        start: i32,
        skip: usize,
        max_chars: usize,
    ) -> Result<(Vec<Unit>, bool), RagError> {
        // 1 単位の字数の目安: ブロックは段落・見出し 1 つ、チャンクは段落を詰めた leaf（上限 600）。
        let per_unit = match source {
            ReadSource::Blocks => 40,
            ReadSource::Chunks => 300,
        };
        let limit = i64::try_from(max_chars / per_unit + 2)
            .unwrap_or(UNITS_PER_QUERY)
            .clamp(8, UNITS_PER_QUERY);
        let sql = match source {
            // 各行の本文は窓の字数＋1 字まで（substr）。さらに、手前までの累積字数が窓を
            // 超えた行は引かない（窓を越える最初の 1 行までで止める）。取得の総量は窓の
            // 2 倍程度に収まる（Codex: 行ごとの上限だけでは件数倍に膨らむ）。
            ReadSource::Blocks => {
                "select ordinal, kind, level, list_marker, page, heading, text, remaining \
                 from (select ordinal, type as kind, level, list_marker, page, \
                              null::text as heading, \
                              substr(text, case when ordinal = $5 then $7 + 1 else 1 end, $8) \
                                  as text, \
                              greatest(char_length(text) \
                                       - case when ordinal = $5 then $7 else 0 end, 0) \
                                  as remaining, \
                              coalesce(sum(char_length(text)) over (order by ordinal \
                                  rows between unbounded preceding and 1 preceding), 0) \
                                  as before \
                       from doc_block \
                       where tenant_id = $1 and org = $2 and node_id = $3 and version = $4 \
                         and ordinal >= $5 \
                       order by ordinal limit $6) w \
                 where before - $7 < $8 \
                 order by ordinal"
            }
            ReadSource::Chunks => {
                "select ordinal, kind, level, list_marker, page, heading, text, remaining \
                 from (select ordinal, kind, null::int as level, null::text as list_marker, \
                              page, \
                              nullif(left(array_to_string(heading_path, ' > '), 200), '') \
                                  as heading, \
                              substr(content, case when ordinal = $5 then $7 + 1 else 1 end, $8) \
                                  as text, \
                              greatest(char_length(content) \
                                       - case when ordinal = $5 then $7 else 0 end, 0) \
                                  as remaining, \
                              coalesce(sum(char_length(content)) over (order by ordinal \
                                  rows between unbounded preceding and 1 preceding), 0) \
                                  as before \
                       from rag_chunk \
                       where tenant_id = $1 and org = $2 and node_id = $3 and version = $4 \
                         and kind in ('leaf', 'table') and ordinal >= $5 \
                       order by ordinal limit $6) w \
                 where before - $7 < $8 \
                 order by ordinal"
            }
        };
        let units: Vec<Unit> = sqlx::query_as(sql)
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .bind(node.id)
            .bind(version)
            .bind(start)
            .bind(limit)
            .bind(i32::try_from(skip).unwrap_or(i32::MAX))
            .bind(i32::try_from(max_chars + 1).unwrap_or(i32::MAX))
            .fetch_all(&self.pool)
            .await?;
        // 件数の上限まで引けた = 続きがあるかもしれない（render が末尾で続きの位置を返す）。
        // 累積字数で打ち切った場合は、打ち切った手前の行で窓を超えるので render が続きを返す。
        let more = units.len() as i64 >= limit;
        Ok((units, more))
    }

    /// 文書の見出し（先頭から・位置の指定を選び直すための材料）。
    async fn outline(
        &self,
        ctx: &AuthContext,
        node: &storage::Node,
        version: i64,
        source: ReadSource,
    ) -> Result<Vec<String>, RagError> {
        let sql = match source {
            ReadSource::Blocks => {
                "select left(text, 200) from doc_block \
                 where tenant_id = $1 and org = $2 and node_id = $3 and version = $4 \
                   and type = 'heading' \
                 order by ordinal limit $5"
            }
            // parent はセクション（見出し境界）ごとに 1 つ。その見出しの並びが目次になる。
            ReadSource::Chunks => {
                "select left(array_to_string(heading_path, ' > '), 200) from rag_chunk \
                 where tenant_id = $1 and org = $2 and node_id = $3 and version = $4 \
                   and kind = 'parent' and cardinality(heading_path) > 0 \
                 order by ordinal limit $5"
            }
        };
        Ok(sqlx::query_scalar(sql)
            .bind(&ctx.tenant_id)
            .bind(&ctx.org)
            .bind(node.id)
            .bind(version)
            .bind(OUTLINE_LIMIT)
            .fetch_all(&self.pool)
            .await?)
    }
}

/// 単位を引くテーブルと、本文になる行の条件。
fn source_sql(source: ReadSource) -> (&'static str, &'static str) {
    match source {
        ReadSource::Blocks => ("doc_block", "true"),
        ReadSource::Chunks => ("rag_chunk", "kind in ('leaf', 'table')"),
    }
}

/// LIKE の特殊文字を無効にする（見出しの部分一致にワイルドカードを持ち込ませない）。
fn escape_like(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace('%', "\\%")
        .replace('_', "\\_")
}

/// 単位を字数の上限まで積んで本文にする。返り値は (本文, 続きの位置)。
///
/// `skip` は先頭の単位の本文を何字飛ばすか（同じ単位の途中からの続き）。少なくとも 1 単位の
/// 一部は載せ、1 単位だけで上限を超えるときは上限で切って、続きを同じ単位の途中に置く。
/// ページが変わる所に `〔p.N〕`、チャンク由来では節が変わる所に見出しの行を挟む。
fn render(
    units: &[Unit],
    source: ReadSource,
    max_chars: usize,
    skip: usize,
    more: bool,
) -> (String, Option<Cursor>) {
    let mut out = String::new();
    let mut chars = 0usize;
    let mut page: Option<i32> = None;
    let mut heading: Option<&str> = None;
    // 単位 `i` の次から読む位置（無ければ、件数の上限まで引けていた場合だけ最後の次）。
    let after = |i: usize| {
        units
            .get(i + 1)
            .map(|u| Cursor {
                unit: u.ordinal,
                offset: 0,
            })
            .or_else(|| {
                more.then(|| {
                    units.last().map(|u| Cursor {
                        unit: u.ordinal + 1,
                        offset: 0,
                    })
                })
                .flatten()
            })
    };
    for (i, unit) in units.iter().enumerate() {
        let skip = if i == 0 { skip } else { 0 };
        let remaining = usize::try_from(unit.remaining).unwrap_or(0);
        if remaining == 0 {
            // 読み切った単位（続きの位置が末尾を越えた・空のブロック）。次の単位へ進む。
            continue;
        }
        let mut head = String::new();
        if unit.page.is_some() && unit.page != page {
            page = unit.page;
            if let Some(p) = page {
                let _ = writeln!(head, "〔p.{p}〕");
            }
        }
        if let Some(path) = unit.heading.as_deref() {
            if source == ReadSource::Chunks && heading != Some(path) {
                heading = Some(path);
                let _ = writeln!(head, "## {path}");
            }
        }
        if skip > 0 {
            head.push_str("（前の続き）\n");
        } else {
            match (source, unit.kind.as_str()) {
                (ReadSource::Blocks, "heading") => {
                    let level = usize::try_from(unit.level.unwrap_or(1).clamp(1, 6)).unwrap_or(1);
                    let _ = write!(head, "{} ", "#".repeat(level));
                }
                (ReadSource::Blocks, "list_item") => {
                    let marker = unit.list_marker.as_deref().unwrap_or("・");
                    let _ = write!(head, "{marker} ");
                }
                _ => {}
            }
        }
        let head_chars = head.chars().count();
        let piece_chars = head_chars + remaining + 2;
        if chars > 0 && chars + piece_chars > max_chars {
            return (
                out,
                Some(Cursor {
                    unit: unit.ordinal,
                    offset: 0,
                }),
            );
        }
        if piece_chars > max_chars {
            // 1 単位だけで上限を超える（巨大な表など）。上限で切り、続きは同じ単位の途中から。
            // 見出しの行（文書由来）も上限に含める。巨大な見出しでも窓を超えない。
            let head: String = head.chars().take(max_chars / 2).collect();
            let head_chars = head.chars().count();
            let take = max_chars.saturating_sub(head_chars).max(1).min(remaining);
            let kept: String = unit.text.chars().take(take).collect();
            let _ = writeln!(out, "{head}{kept}");
            // 本文の末尾まで返したなら次の単位へ（同じ単位の位置を返し続けない）。
            let next = if take >= remaining {
                after(i)
            } else {
                Some(Cursor {
                    unit: unit.ordinal,
                    offset: skip + take,
                })
            };
            return (out, next);
        }
        // 単位の残りが丸ごと入る（`text` は残り全部）。表示上の前後の空白は落とす。
        out.push_str(&head);
        out.push_str(if skip > 0 {
            unit.text.trim_end()
        } else {
            unit.text.trim()
        });
        out.push_str("\n\n");
        chars += piece_chars;
    }
    // 引いた単位を積み切った。件数の上限まで引けていたなら続きがあり得るので次の位置を返す。
    let next = units.len().checked_sub(1).and_then(|last| {
        more.then(|| Cursor {
            unit: units[last].ordinal + 1,
            offset: 0,
        })
    });
    (out, next)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(ordinal: i32, kind: &str, page: Option<i32>, path: &[&str], text: &str) -> Unit {
        Unit {
            ordinal,
            kind: kind.into(),
            level: Some(2),
            list_marker: (kind == "list_item").then(|| "1.".to_string()),
            page,
            heading: (!path.is_empty()).then(|| path.join(" > ")),
            text: text.into(),
            remaining: i32::try_from(text.chars().count()).unwrap(),
        }
    }

    /// 続きの位置 `skip` から引いた単位（SQL の substr と char_length を写す）。
    fn tail(mut u: Unit, skip: usize) -> Unit {
        u.remaining = (u.remaining - i32::try_from(skip).unwrap()).max(0);
        u.text = u.text.chars().skip(skip).collect();
        u
    }

    #[test]
    fn blocks_render_headings_lists_and_page_marks() {
        let units = [
            unit(0, "heading", Some(1), &[], "第5条"),
            unit(1, "paragraph", Some(1), &[], " 本文 "),
            unit(2, "list_item", Some(2), &[], "申請する"),
        ];
        let (text, next) = render(&units, ReadSource::Blocks, 1_000, 0, false);
        assert_eq!(
            text,
            "〔p.1〕\n## 第5条\n\n本文\n\n〔p.2〕\n1. 申請する\n\n"
        );
        assert_eq!(next, None);
    }

    #[test]
    fn chunks_render_section_headings_once() {
        let units = [
            unit(3, "leaf", None, &["第1章", "目的"], "a"),
            unit(4, "leaf", None, &["第1章", "目的"], "b"),
            unit(7, "table", None, &["第2章"], "| x |"),
        ];
        let (text, _) = render(&units, ReadSource::Chunks, 1_000, 0, false);
        assert_eq!(text, "## 第1章 > 目的\na\n\nb\n\n## 第2章\n| x |\n\n");
    }

    #[test]
    fn window_stops_at_budget_and_points_to_next_unit() {
        let units = [
            unit(10, "paragraph", None, &[], &"あ".repeat(6)),
            unit(11, "paragraph", None, &[], &"い".repeat(6)),
        ];
        // 1 単位 = 6 字 + 区切り 2 字。10 字では 2 単位目が入らない。
        let (text, next) = render(&units, ReadSource::Blocks, 10, 0, false);
        assert_eq!(text, format!("{}\n\n", "あ".repeat(6)));
        assert_eq!(
            next,
            Some(Cursor {
                unit: 11,
                offset: 0
            })
        );
    }

    #[test]
    fn oversized_unit_continues_inside_the_same_unit() {
        let units = [
            unit(
                0,
                "table",
                None,
                &[],
                &format!("{}{}", "表".repeat(12), "末".repeat(3)),
            ),
            unit(1, "paragraph", None, &[], "次"),
        ];
        let (text, next) = render(&units, ReadSource::Blocks, 10, 0, false);
        assert_eq!(text, format!("{}\n", "表".repeat(10)));
        assert_eq!(
            next,
            Some(Cursor {
                unit: 0,
                offset: 10
            })
        );
        // 続きは同じ単位の 10 字目から。印を付け、残りと次の単位が読める。
        let [first, second] = units;
        let units = [tail(first, 10), second];
        let (text, next) = render(&units, ReadSource::Blocks, 100, 10, false);
        assert_eq!(text, "（前の続き）\n表表末末末\n\n次\n\n");
        assert_eq!(next, None);
    }

    #[test]
    fn tiny_windows_still_reach_the_end() {
        // 窓が「前の続き」の印より小さくても、1 字ずつ進んで単位の末尾から次へ移る（Codex）。
        let whole = [
            unit(0, "paragraph", None, &[], "あいう"),
            unit(1, "paragraph", None, &[], "え"),
        ];
        let mut cursor = Some(Cursor { unit: 0, offset: 0 });
        let mut read = String::new();
        for _ in 0..20 {
            let Some(c) = cursor else { break };
            let units: Vec<Unit> = whole
                .iter()
                .filter(|u| u.ordinal >= c.unit)
                .map(|u| {
                    let u = unit(u.ordinal, &u.kind, None, &[], &u.text);
                    if u.ordinal == c.unit {
                        tail(u, c.offset)
                    } else {
                        u
                    }
                })
                .collect();
            let (text, next) = render(&units, ReadSource::Blocks, 1, c.offset, false);
            read.push_str(&text);
            cursor = next;
        }
        assert!(cursor.is_none(), "文書の終わりに届く");
        let body: String = read.chars().filter(|c| "あいうえ".contains(*c)).collect();
        assert_eq!(body, "あいうえ");
    }

    #[test]
    fn batch_cut_by_row_limit_points_past_the_last_unit() {
        let units = [
            unit(5, "paragraph", None, &[], "a"),
            unit(6, "paragraph", None, &[], "b"),
        ];
        // 件数の上限まで引けていた（続きがあり得る）なら、最後の次の単位を返す。
        let (_, next) = render(&units, ReadSource::Blocks, 1_000, 0, true);
        assert_eq!(next, Some(Cursor { unit: 7, offset: 0 }));
    }

    #[test]
    fn like_wildcards_are_escaped() {
        assert_eq!(escape_like("50%_a\\b"), "50\\%\\_a\\\\b");
    }
}
