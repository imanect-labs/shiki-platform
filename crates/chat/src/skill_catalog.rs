//! skill カタログ（skill ツールの動的 description に載せる name+description 一覧・#344 Task 10.11）。
//!
//! モデルはこのカタログ（**name + description のみ**・本文は載せない＝コンテキストを食わない）
//! を見て、必要になったら skill ツールで instructions を引く。掲載は「明示的な人間の行為」
//! （本人 owner／ピン。PR2 で同意インストールを ∪ に追加）に限る — description スクワッティング
//! 防御の核（共有されただけの skill は他人のカタログへ自動掲載しない）。
//!
//! [`SkillCatalogSource`] はカタログの取得元の差し込み点（PR2 で「インストール済み ∪ 本人」実装へ
//! 差し替える）。並び順・切詰め・上限は本モジュールが一元管理する。

use std::sync::Arc;

use authz::AuthContext;
use uuid::Uuid;

use crate::ChatError;

/// カタログに載せる 1 スキルの description 最大文字数（超過は切詰め・スクワッティング防御）。
pub(crate) const MAX_ENTRY_DESCRIPTION_CHARS: usize = 200;
/// ツール定義 description に列挙する最大件数（溢れは件数のみ明記）。
pub(crate) const MAX_LISTED_ENTRIES: usize = 50;

/// カタログの 1 エントリ（name + description のみ）。
#[derive(Debug, Clone)]
pub struct SkillCatalogEntry {
    pub id: Uuid,
    pub version: i64,
    pub name: String,
    pub description: String,
    /// thread にピン済み（=既にロード済みで開始・一覧の先頭に出す）。
    pub pinned: bool,
}

/// カタログの取得元（実行主体から見えるスキル集合）。
///
/// PR1 実装は「本人 owner」（[`OwnedSkillCatalog`]）。PR2 でレジストリの同意インストールを
/// 加えた実装に差し替える（トレイト裏の差し込み点・アプリ本体は分岐しない）。
#[async_trait::async_trait]
pub trait SkillCatalogSource: Send + Sync {
    /// 発話ユーザーの権限で見えるカタログを返す（ピン以外・pinned=false で返す）。
    async fn entries(
        &self,
        ctx: &AuthContext,
        trace_id: Option<&str>,
    ) -> Result<Vec<SkillCatalogEntry>, ChatError>;
}

/// 本人 owner の skill をカタログにする PR1 実装。
pub struct OwnedSkillCatalog {
    artifacts: Arc<artifact::ArtifactStore>,
}

impl OwnedSkillCatalog {
    pub fn new(artifacts: Arc<artifact::ArtifactStore>) -> Self {
        OwnedSkillCatalog { artifacts }
    }
}

#[async_trait::async_trait]
impl SkillCatalogSource for OwnedSkillCatalog {
    async fn entries(
        &self,
        ctx: &AuthContext,
        _trace_id: Option<&str>,
    ) -> Result<Vec<SkillCatalogEntry>, ChatError> {
        // 取得上限（200 = artifact 側 clamp 上限）。超過分はカタログにも closed-set マップにも
        // 載らない（=呼べない）ため、上限到達時は warn で観測可能にする（silent cap にしない）。
        let summaries = self
            .artifacts
            .list_my_skill_summaries(ctx, 200)
            .await
            .map_err(crate::skill::map_skill_err)?;
        if summaries.len() >= 200 {
            tracing::warn!(
                principal = %ctx.principal.id,
                "所有 skill が取得上限 200 に到達（超過分はカタログに載らない。ピンで利用可能）"
            );
        }
        Ok(summaries
            .into_iter()
            .map(|s| SkillCatalogEntry {
                id: s.id,
                version: s.current_version,
                name: s.name,
                description: s.description,
                pinned: false,
            })
            .collect())
    }
}

/// ピン済み（ロード済み）とカタログ源のエントリを統合する（id で重複排除・ピン優先）。
///
/// 並び順: ピン → その他（源の順序＝name 順を維持）。ピンの version が源の latest と
/// 異なる場合もピン版が正（再現性はピンが担う）。
pub(crate) fn merge_entries(
    pinned: Vec<SkillCatalogEntry>,
    source: Vec<SkillCatalogEntry>,
) -> Vec<SkillCatalogEntry> {
    let mut out = pinned;
    let pinned_ids: std::collections::HashSet<Uuid> = out.iter().map(|e| e.id).collect();
    out.extend(source.into_iter().filter(|e| !pinned_ids.contains(&e.id)));
    out
}

/// skill ツールのツール定義 description を組み立てる（run 開始時に 1 回・run 単位で固定）。
pub(crate) fn render_tool_description(entries: &[SkillCatalogEntry]) -> String {
    let mut out = String::from(
        "スキル（社内で定義された作業手順・指示文）を名前で読み込む。読み込むと instructions が\
         返り、以降その指示に従って作業できる。1 メッセージ中に何個でも呼んでよい。\
         下の一覧は name: description。必要になったスキルだけを読み込むこと。\n\n利用可能なスキル:\n",
    );
    for e in entries.iter().take(MAX_LISTED_ENTRIES) {
        let desc: String = e
            .description
            .chars()
            .take(MAX_ENTRY_DESCRIPTION_CHARS)
            .collect();
        out.push_str("- ");
        out.push_str(&e.name);
        if e.pinned {
            out.push_str("（適用済み）");
        }
        out.push_str(": ");
        out.push_str(desc.trim());
        out.push('\n');
    }
    if entries.len() > MAX_LISTED_ENTRIES {
        use std::fmt::Write as _;
        // 一覧外でも by_name マップには載っている（構築時に同じ entries から作る）ため
        // 名前指定で読み込める、は取得上限内でのみ真。上限超過は掲載も解決も不可なので
        // 正直に「ピンで使える」誘導にする（silent cap にしない・#344 レビュー指摘）。
        let _ = writeln!(
            out,
            "（他 {} 件。名前が分かれば一覧外でも読み込める。見つからないスキルはスレッドへの             ピンで使える）",
            entries.len() - MAX_LISTED_ENTRIES
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, pinned: bool) -> SkillCatalogEntry {
        SkillCatalogEntry {
            id: Uuid::new_v4(),
            version: 1,
            name: name.into(),
            description: format!("{name} の説明"),
            pinned,
        }
    }

    #[test]
    fn merge_puts_pins_first_and_dedups() {
        let pin = entry("a", true);
        let pin_id = pin.id;
        // 源に同じ id が latest 版で入っていてもピン版が勝つ。
        let mut dup = entry("a", false);
        dup.id = pin_id;
        dup.version = 9;
        let merged = merge_entries(vec![pin], vec![dup, entry("b", false)]);
        assert_eq!(merged.len(), 2);
        assert!(merged[0].pinned);
        assert_eq!(merged[0].version, 1, "ピン版が正（latest に化けない）");
        assert_eq!(merged[1].name, "b");
    }

    #[test]
    fn description_truncates_and_caps() {
        let mut long = entry("long", false);
        long.description = "あ".repeat(MAX_ENTRY_DESCRIPTION_CHARS + 100);
        let entries: Vec<SkillCatalogEntry> = std::iter::once(long)
            .chain((0..MAX_LISTED_ENTRIES + 5).map(|i| entry(&format!("s{i}"), false)))
            .collect();
        let desc = render_tool_description(&entries);
        // 切詰め（1024 字の description 上限があっても 200 字で切る）。
        assert!(!desc.contains(&"あ".repeat(MAX_ENTRY_DESCRIPTION_CHARS + 1)));
        // 溢れは件数を明記する（silent truncation にしない）。
        assert!(desc.contains("他 6 件"));
    }
}
