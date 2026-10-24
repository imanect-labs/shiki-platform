//! 更新時のリンク値のマージ（data-platform §7.1・§7.2 の存在秘匿の書込側）。
//!
//! 読取ではリンク列の値が閲覧者に見える参照先だけに絞られる（[`crate::derived`] の ⓪）。書き手は
//! その絞られた値を見て patch を作るので、patch をそのまま保存すると、書き手に見えない既存の参照先を
//! 黙って消してしまう。そこで更新（1 件・一括 upsert の更新行。どちらも `update_locked`）は:
//!
//! - **patch に含まれるリンク列だけ**を扱う。含まれない列は検証もしない（参照先が見えなくなった
//!   リンクを持つ行でも、関係ない列は編集できる）。
//! - 保存する値 = patch の値（書き手に見える参照先） ＋ 既存値のうち**書き手に見えない参照先**
//!   （元の位置に残す。見えていた位置に patch の値を順に詰め、余りは末尾）。見えている並びをそのまま
//!   送り返せば元の値と一致する。
//! - 検証するのは patch の値のうち「既存値で書き手に見えていなかったもの」＝**今回足した参照先**だけ
//!   （書き手から見えること・PIT-20）。見えない既存の参照先の id を patch に入れても「足した」扱いで
//!   不可視と同じ 400 になる（「その id がこの行に付いている」ことのオラクルを作らない）。
//! - 単一値のリンク（`one_to_one` / `many_to_one`・`link` の無い v1 の `record_ref`）に見えない既存値が
//!   ある場合、別の参照先への付け替えは 403（見えない参照先を黙って外させない）。値の除去（null・空配列）は
//!   見えない参照先を残すだけで、書き手から見た結果（空）と一致する。

use std::collections::{BTreeSet, HashSet};

use authz::AuthContext;
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::model::{DataTable, FieldDef, FieldType};
use crate::policy::memo::AuthzMemo;
use crate::store::DataStore;
use crate::DataError;

/// patch のリンク値を参照先 id の並びとして読む（null は空）。UUID 文字列以外を含む値は `None`
/// （マージせず、そのまま検証器に渡して入力誤りとして拒否させる）。
fn patch_ids(v: &Value) -> Option<Vec<Uuid>> {
    match v {
        Value::Null => Some(Vec::new()),
        Value::String(s) => Uuid::parse_str(s).ok().map(|u| vec![u]),
        Value::Array(a) => a
            .iter()
            .map(|e| e.as_str().and_then(|s| Uuid::parse_str(s).ok()))
            .collect(),
        _ => None,
    }
}

/// 既存の保存値（外部形）の参照先 id。
fn stored_ids(v: Option<&Value>) -> Vec<Uuid> {
    match v {
        Some(Value::String(s)) => Uuid::parse_str(s).into_iter().collect(),
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .filter_map(|s| Uuid::parse_str(s).ok())
            .collect(),
        _ => Vec::new(),
    }
}

/// この列の値が 1 件までか（`link` の無い v1 の `record_ref` は `many_to_one`）。
fn single_valued(f: &FieldDef) -> bool {
    f.link.as_ref().is_none_or(|l| l.cardinality.src_single())
}

/// 1 列分のマージの結果。
#[derive(Debug, PartialEq, Eq)]
enum Merged {
    /// 保存する値（`None` は patch の値をそのまま使う）。
    Value(Option<Vec<Uuid>>),
    /// 今回足した参照先が書き手に見えない。
    NotFound,
    /// 単一値のリンクで、見えない既存の参照先を付け替えようとした。
    Forbidden,
}

/// 1 列分のマージ（純粋関数）。`visible` は既存値と patch の値を合わせた集合のうち書き手に見えるもの。
fn merge_one(old: &[Uuid], new: &[Uuid], visible: &HashSet<Uuid>, single: bool) -> Merged {
    let old_visible: HashSet<Uuid> = old
        .iter()
        .copied()
        .filter(|i| visible.contains(i))
        .collect();
    // 今回足した参照先（既存値で書き手に見えていなかったもの）は、書き手から見えること。
    if new
        .iter()
        .any(|i| !old_visible.contains(i) && !visible.contains(i))
    {
        return Merged::NotFound;
    }
    let mut seen: HashSet<Uuid> = HashSet::new();
    let hidden: Vec<Uuid> = old
        .iter()
        .copied()
        .filter(|i| !visible.contains(i) && seen.insert(*i))
        .collect();
    if hidden.is_empty() {
        return Merged::Value(None);
    }
    if single && !new.is_empty() {
        return Merged::Forbidden;
    }
    // 見えない参照先は元の位置に残し、見えていた位置に patch の値を順に詰める（余りは末尾・足りなければ
    // 位置ごと詰める）。見えている並びをそのまま送り返せば元の値と一致する（差分なし）。
    let hidden: HashSet<Uuid> = hidden.into_iter().collect();
    let mut out: Vec<Uuid> = Vec::with_capacity(new.len() + hidden.len());
    let mut taken: HashSet<Uuid> = HashSet::new();
    let mut next = new.iter().filter(|i| !hidden.contains(i));
    for i in old {
        let pick = if hidden.contains(i) {
            Some(*i)
        } else {
            next.next().copied()
        };
        if let Some(p) = pick.filter(|p| taken.insert(*p)) {
            out.push(p);
        }
    }
    for i in next {
        if taken.insert(*i) {
            out.push(*i);
        }
    }
    Merged::Value(Some(out))
}

impl DataStore {
    /// 更新の patch のリンク列を、書き手に見えない既存の参照先を残すようにマージし、今回足した参照先
    /// だけを書き手の権限で検証する（モジュール doc の規則）。返り値はマージ後の patch。
    ///
    /// `old` は現在値（外部形）。可視性は参照先テーブルの viewer・行述語の二段を書き手本人の権限で
    /// 判定する（読取の存在秘匿と同じ [`DataStore::visible_ref_rows`]・リンク列ごとに 1 回）。
    pub(crate) async fn merge_link_patch(
        &self,
        ctx: &AuthContext,
        az: &AuthzMemo<'_>,
        table: &DataTable,
        old: &Map<String, Value>,
        patch: &Map<String, Value>,
    ) -> Result<Map<String, Value>, DataError> {
        let mut out = patch.clone();
        for f in &table.schema.fields {
            if f.field_type != FieldType::RecordRef || f.is_reverse_link() {
                continue;
            }
            let (Some(rt), Some(pv)) = (f.ref_table, patch.get(&f.key)) else {
                continue;
            };
            let Some(new) = patch_ids(pv) else {
                continue; // 形の誤りは検証器が拒否する。
            };
            let old_ids = stored_ids(old.get(&f.key));
            let all: Vec<Uuid> = old_ids
                .iter()
                .chain(new.iter())
                .copied()
                .collect::<BTreeSet<_>>()
                .into_iter()
                .collect();
            let visible: HashSet<Uuid> = if all.is_empty() {
                HashSet::new()
            } else {
                self.visible_ref_rows(ctx, az, rt, &all, &BTreeSet::new(), 0)
                    .await?
                    .rows
                    .into_keys()
                    .collect()
            };
            match merge_one(&old_ids, &new, &visible, single_valued(f)) {
                Merged::Value(None) => {}
                Merged::Value(Some(ids)) => {
                    out.insert(
                        f.key.clone(),
                        Value::Array(
                            ids.into_iter()
                                .map(|i| Value::String(i.to_string()))
                                .collect(),
                        ),
                    );
                }
                Merged::NotFound => {
                    return Err(DataError::Invalid(format!(
                        "'{}' の参照先レコードが見つかりません",
                        f.key
                    )));
                }
                Merged::Forbidden => return Err(DataError::Forbidden),
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(n: u128) -> Uuid {
        Uuid::from_u128(n)
    }

    #[test]
    fn keeps_hidden_targets_and_validates_only_added_ones() {
        let (a, b, h1, h2, x) = (u(1), u(2), u(10), u(11), u(99));
        let visible: HashSet<Uuid> = [a, b].into();
        let old = [h1, a, h2, b];
        // 見える分だけを送り返す → 見えない 2 件は元の位置に残る。
        assert_eq!(
            merge_one(&old, &[a, b], &visible, false),
            Merged::Value(Some(old.to_vec()))
        );
        assert_eq!(
            merge_one(&old, &[b], &visible, false),
            Merged::Value(Some(vec![h1, b, h2]))
        );
        // 並べ替え・追加（余りは末尾）。
        assert_eq!(
            merge_one(&[h1, a], &[b, a], &visible, false),
            Merged::Value(Some(vec![h1, b, a]))
        );
        // 全部外す（空）→ 見えない分だけ残る。
        assert_eq!(
            merge_one(&old, &[], &visible, false),
            Merged::Value(Some(vec![h1, h2]))
        );
        // 見えない参照先を足そうとする・見えない既存の id を送る → どちらも「見つからない」。
        assert_eq!(merge_one(&old, &[a, x], &visible, false), Merged::NotFound);
        assert_eq!(merge_one(&old, &[a, h1], &visible, false), Merged::NotFound);
        // 見えない既存値が無ければ patch の値をそのまま使う。
        assert_eq!(merge_one(&[a], &[b], &visible, false), Merged::Value(None));
    }

    #[test]
    fn single_valued_link_with_hidden_target_cannot_be_repointed() {
        let (a, h) = (u(1), u(10));
        let visible: HashSet<Uuid> = [a].into();
        assert_eq!(merge_one(&[h], &[a], &visible, true), Merged::Forbidden);
        // 除去は見えない参照先を残すだけ（書き手から見た結果＝空と一致）。
        assert_eq!(
            merge_one(&[h], &[], &visible, true),
            Merged::Value(Some(vec![h]))
        );
    }

    #[test]
    fn patch_values_are_read_leniently_but_malformed_ones_are_left_to_the_validator() {
        let id = u(5);
        assert_eq!(patch_ids(&Value::Null), Some(vec![]));
        assert_eq!(patch_ids(&Value::String(id.to_string())), Some(vec![id]));
        assert_eq!(
            patch_ids(&serde_json::json!([id.to_string()])),
            Some(vec![id])
        );
        assert_eq!(patch_ids(&serde_json::json!(["nope"])), None);
        assert_eq!(patch_ids(&serde_json::json!(3)), None);
    }
}
