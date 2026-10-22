//! セッション（認可・直列化・保存）に包んだエンジンの入口（#725）。
//!
//! 1 回のツール呼び出し＝1 headless セッション（`LiveEditor::with_session`）。権限は毎回
//! OpenFGA で再判定され（読み取りは viewer・編集は editor）、編集は同じファイルの AI 編集と
//! 直列化される。編集が文書を変えたときだけ保存し、版の前進で永続化を確かめる。

use std::sync::Arc;

use authz::AuthContext;
use uuid::Uuid;

use super::a1;
use super::calc::normalize::{normalize_with_sheets, OpError};
use super::calc::parse::parse_ops;
use super::calc::run;
use super::chart_insert::{self, ChartInsert};
use super::writer;
use super::EngineError;
use crate::live::{LiveEditError, LiveEditor, LiveSaveResult, SessionAccess};
use crate::templates::{DOCX_CONTENT_TYPE, XLSX_CONTENT_TYPE};

/// エンジンで扱う表計算の形式。
pub const SHEET_CONTENT_TYPES: &[&str] = &[
    XLSX_CONTENT_TYPE,
    "application/vnd.oasis.opendocument.spreadsheet",
];

/// エンジンで扱う文書の形式。
pub const DOC_CONTENT_TYPES: &[&str] =
    &[DOCX_CONTENT_TYPE, "application/vnd.oasis.opendocument.text"];

/// 読み取りに使える形式（表計算と文書）。
const READ_CONTENT_TYPES: &[&str] = &[
    XLSX_CONTENT_TYPE,
    "application/vnd.oasis.opendocument.spreadsheet",
    DOCX_CONTENT_TYPE,
    "application/vnd.oasis.opendocument.text",
];

/// `26.04.2.1` のような版が `min`（年・月）以上か。
fn version_at_least(version: &str, min: (u32, u32)) -> bool {
    let mut parts = version.split('.').map(|p| p.parse::<u32>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    (major, minor) >= min
}

/// 入口の失敗（LLM へ返す文言に写す）。
#[derive(Debug, thiserror::Error)]
pub enum OfficeEngineError {
    /// この Collabora はエンジンを持たない（26.04 未満）。
    #[error("この環境の Office エンジンは古く、レイアウト編集に対応していません")]
    Unsupported,
    /// 操作の内容が不正（セッションを開く前に検出・文書は変わっていない）。
    #[error(transparent)]
    Invalid(#[from] OpError),
    /// 範囲の書き方が不正（読み取り）。
    #[error(transparent)]
    Range(#[from] a1::A1Error),
    /// 権限・形式・混雑・接続の失敗。
    #[error(transparent)]
    Session(#[from] LiveEditError),
    /// エンジンの失敗（セッション切断・kit の例外）。
    #[error(transparent)]
    Engine(#[from] EngineError),
}

/// Excel 編集の報告。
#[derive(Debug, Clone)]
pub struct SheetEditReport {
    pub file_name: String,
    /// LLM への観測テキスト（適用結果・読み戻し・検査）。
    pub text: String,
    /// 文書を変えたか。
    pub applied: bool,
    pub save: LiveSaveResult,
    /// 参加時点の総 view 数（1 なら AI 単独セッション）。
    pub views: u32,
}

/// 読み取りの指定。
#[derive(Debug, Clone, Default)]
pub struct SheetReadRequest {
    /// 読む範囲（省略でブックの概要＋作業中シートの先頭）。
    pub range: Option<String>,
    /// 範囲にシート名が無いときのシート。
    pub sheet: Option<String>,
    pub max_cells: Option<u32>,
    /// 条件付き書式・グラフ・印刷設定・固定などシート全体の付帯情報も読む。
    pub details: bool,
}

/// Word の読み取り（range: ブロックの範囲・`from:N`・省略で概要）。
async fn read_doc(
    client: &mut crate::live::CoolWsClient,
    file_name: &str,
    range: Option<&str>,
) -> Result<String, OfficeEngineError> {
    let r = match range.map(str::trim) {
        None | Some("") => writer::run::read_overview(client, file_name, 0).await,
        Some(r) if r.starts_with("from:") => {
            let from = r["from:".len()..].trim().parse().unwrap_or(0);
            writer::run::read_overview(client, file_name, from).await
        }
        Some(r) => {
            let (from, to) = match r.split_once(':') {
                Some((a, b)) => (a.trim(), Some(b.trim())),
                None => (r, None),
            };
            writer::run::read_blocks(client, from, to).await
        }
    };
    r.map_err(OfficeEngineError::from)
}

/// エンジンの入口。
pub struct OfficeEngine {
    live: Arc<LiveEditor>,
    http: reqwest::Client,
    /// Collabora の内部 URL（版の確認用）。
    collabora_base_url: String,
    /// この Collabora がエンジン（executescript）を持つか。初回の確認結果を覚える。
    supported: tokio::sync::OnceCell<bool>,
}

/// エンジンが要る Collabora の最小の版（executescript は 26.04 から）。
const MIN_VERSION: (u32, u32) = (26, 4);

impl OfficeEngine {
    pub fn new(
        live: Arc<LiveEditor>,
        http: reqwest::Client,
        collabora_base_url: impl Into<String>,
    ) -> Self {
        OfficeEngine {
            live,
            http,
            collabora_base_url: collabora_base_url.into(),
            supported: tokio::sync::OnceCell::new(),
        }
    }

    /// この Collabora でエンジンが使えるか（`/hosting/capabilities` の productVersion で判定）。
    ///
    /// 確認できなかった（Collabora 停止中など）ときは覚えずに `true` を返し、実際の接続で失敗させる
    /// （一時的な停止で「使えない」と覚えてしまわない）。
    pub async fn is_supported(&self) -> bool {
        if let Some(v) = self.supported.get() {
            return *v;
        }
        let url = format!(
            "{}/hosting/capabilities",
            self.collabora_base_url.trim_end_matches('/')
        );
        let fetched = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let resp = self.http.get(&url).send().await.ok()?;
            let v: serde_json::Value = resp.json().await.ok()?;
            Some(v["productVersion"].as_str()?.to_string())
        })
        .await
        .ok()
        .flatten();
        match fetched {
            Some(version) => {
                let ok = version_at_least(&version, MIN_VERSION);
                if !ok {
                    tracing::warn!(%version, "Collabora が古く Office エンジン（executescript）が使えません");
                }
                *self.supported.get_or_init(|| async { ok }).await
            }
            None => true,
        }
    }

    /// Excel・Word を読む（viewer@file）。文書の種類はセッションを開いてから見て振り分ける。
    ///
    /// Excel の `range` は A1 表記、Word の `range` はブロックの範囲（`b3-xxxx:b9-yyyy`）か
    /// 一覧の続き（`from:200`）。
    pub async fn read(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        req: &SheetReadRequest,
    ) -> Result<String, OfficeEngineError> {
        if !self.is_supported().await {
            return Err(OfficeEngineError::Unsupported);
        }
        let max_cells = req
            .max_cells
            .unwrap_or(run::DEFAULT_MAX_CELLS)
            .clamp(1, 5_000);
        let outcome = Box::pin(self.live.with_session(
            ctx,
            file_id,
            SessionAccess::Read,
            READ_CONTENT_TYPES,
            async |client, info| {
                let r = if DOC_CONTENT_TYPES.contains(&info.content_type.as_str()) {
                    read_doc(client, &info.file_name, req.range.as_deref()).await
                } else {
                    match req.range.as_deref().map(a1::parse_area).transpose() {
                        Err(e) => Err(OfficeEngineError::Range(e)),
                        Ok(Some(a)) => run::read_range(
                            client,
                            &a,
                            req.sheet.as_deref(),
                            max_cells,
                            req.details,
                        )
                        .await
                        .map_err(OfficeEngineError::from),
                        Ok(None) => run::read_overview(client, &info.file_name)
                            .await
                            .map_err(OfficeEngineError::from),
                    }
                };
                (r, false)
            },
        ))
        .await?;
        outcome.value
    }

    /// Word を編集する（editor@file）。操作は全部成功か全部取り消し。
    pub async fn edit_doc(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        label: &str,
        ops: &[serde_json::Value],
    ) -> Result<SheetEditReport, OfficeEngineError> {
        // 誤りはセッションを開く前に返す（kit を起こさない・文書に触れない）。
        let normalized = writer::normalize::normalize_doc_ops(ops)?;
        if !self.is_supported().await {
            return Err(OfficeEngineError::Unsupported);
        }
        let outcome = Box::pin(self.live.with_session(
            ctx,
            file_id,
            SessionAccess::Edit,
            DOC_CONTENT_TYPES,
            async |client, _info| {
                let r = writer::run::apply(client, label, &normalized).await;
                let modified = matches!(&r, Ok(o) if o.applied);
                (r, modified)
            },
        ))
        .await?;
        let applied = outcome.value?;
        Ok(SheetEditReport {
            file_name: outcome.info.file_name,
            text: applied.text,
            applied: applied.applied,
            save: outcome.save,
            views: outcome.info.views,
        })
    }

    /// Excel を編集する（editor@file）。操作は全部成功か全部取り消し。
    pub async fn edit_sheet(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        label: &str,
        default_sheet: Option<&str>,
        ops: &[serde_json::Value],
    ) -> Result<SheetEditReport, OfficeEngineError> {
        // 誤りはセッションを開く前に返す（kit を起こさない・文書に触れない）。
        let parsed = parse_ops(ops)?;
        let items: Vec<_> = parsed.iter().map(|(op, s)| (op, s.as_deref())).collect();
        let normalized = normalize_with_sheets(&items, default_sheet)?;
        if !self.is_supported().await {
            return Err(OfficeEngineError::Unsupported);
        }
        let outcome = Box::pin(self.live.with_session(
            ctx,
            file_id,
            SessionAccess::Edit,
            SHEET_CONTENT_TYPES,
            async |client, _info| {
                let r = run::apply(client, label, &normalized).await;
                let modified = matches!(&r, Ok(o) if o.applied);
                (r, modified)
            },
        ))
        .await?;
        let applied = outcome.value?;
        Ok(SheetEditReport {
            file_name: outcome.info.file_name,
            text: applied.text,
            applied: applied.applied,
            save: outcome.save,
            views: outcome.info.views,
        })
    }

    /// 画面のボタンからチャットの図をグラフとして入れる（editor@file・#731）。
    ///
    /// Excel は新しいシート（題から作った空いている名前）に表を書いてその右にグラフを置き、Word は
    /// 文書の末尾にグラフを入れる。どちらも AI と同じ操作の経路（検証・全部成功か全部取り消し・
    /// 人の 1 回の「元に戻す」）を通る。
    pub async fn insert_chart(
        &self,
        ctx: &AuthContext,
        file_id: Uuid,
        label: &str,
        chart: &ChartInsert,
    ) -> Result<ChartInsertReport, OfficeEngineError> {
        let invalid = |message: String| {
            OfficeEngineError::Invalid(OpError {
                index: 1,
                op: "insert_chart",
                message,
            })
        };
        // Word の形の誤り（系列の数など）はセッションを開く前に返す。
        let doc = writer::normalize::normalize_doc_ops(&chart_insert::doc_ops(chart))?;
        if !self.is_supported().await {
            return Err(OfficeEngineError::Unsupported);
        }
        let outcome = Box::pin(self.live.with_session(
            ctx,
            file_id,
            SessionAccess::Edit,
            READ_CONTENT_TYPES,
            async |client, info| {
                let r: Result<(Option<String>, bool, String), OfficeEngineError> = async {
                    if DOC_CONTENT_TYPES.contains(&info.content_type.as_str()) {
                        let o = writer::run::apply(client, label, &doc).await?;
                        return Ok((None, o.applied, o.text));
                    }
                    chart.check_for_sheet().map_err(invalid)?;
                    let overview =
                        super::call(client, serde_json::json!({"action": "calc.overview"})).await?;
                    let existing: Vec<String> = overview["sheets"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|s| s["name"].as_str().map(str::to_string))
                        .collect();
                    let sheet = chart_insert::sheet_name_for(chart.title.as_deref(), &existing);
                    let parsed = parse_ops(&chart_insert::sheet_ops(chart, &sheet))?;
                    let items: Vec<_> = parsed.iter().map(|(op, s)| (op, s.as_deref())).collect();
                    let normalized = normalize_with_sheets(&items, None)?;
                    let o = run::apply(client, label, &normalized).await?;
                    Ok((Some(sheet), o.applied, o.text))
                }
                .await;
                let modified = matches!(&r, Ok((_, true, _)));
                (r, modified)
            },
        ))
        .await?;
        let (sheet, applied, text) = outcome.value?;
        Ok(ChartInsertReport {
            sheet,
            applied,
            text,
            save: outcome.save,
        })
    }
}

/// グラフの挿入の報告。
#[derive(Debug, Clone)]
pub struct ChartInsertReport {
    /// Excel のとき、表とグラフを置いたシート（Word は None）。
    pub sheet: Option<String>,
    /// 文書を変えたか（false なら丸ごと取り消し済みで、text に理由）。
    pub applied: bool,
    pub text: String,
    pub save: LiveSaveResult,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_collabora_versions() {
        assert!(version_at_least("26.04.2.1", MIN_VERSION));
        assert!(version_at_least("26.10.0", MIN_VERSION));
        assert!(version_at_least("27.01", MIN_VERSION));
        assert!(!version_at_least("25.04.9.5", MIN_VERSION));
        assert!(!version_at_least("", MIN_VERSION));
    }
}
