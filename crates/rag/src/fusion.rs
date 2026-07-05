//! RRF（Reciprocal Rank Fusion・Task 2.6）。純関数。
//!
//! dense（Qdrant）と keyword（Tantivy BM25）の**順位**だけを使って融合する
//! （スコアのスケールが異なる系統を正規化なしで安全に混ぜられる標準手法）。
//! `score = Σ 1 / (k + rank)`（rank は 1 始まり・k は平滑化定数）。

use std::collections::HashMap;

use uuid::Uuid;

use crate::vector_store::ScoredChunk;

/// RRF の平滑化定数の既定値（原論文と同じ 60）。
pub const RRF_K: f32 = 60.0;

/// 複数の順位付きリストを RRF で融合する。chunk_id 重複は除去され、
/// 両リストに現れるチャンクは融合スコアが加算されて上位に来る。
pub fn rrf_fuse(lists: &[&[ScoredChunk]], k: f32) -> Vec<ScoredChunk> {
    let mut fused: HashMap<Uuid, ScoredChunk> = HashMap::new();
    for list in lists {
        for (rank0, hit) in list.iter().enumerate() {
            // rank は fetch_k（≦256）に有界で f32 の精度内。
            #[allow(clippy::cast_precision_loss)]
            let contribution = 1.0 / (k + (rank0 as f32) + 1.0);
            fused
                .entry(hit.chunk_id)
                .and_modify(|e| e.score += contribution)
                .or_insert_with(|| ScoredChunk {
                    chunk_id: hit.chunk_id,
                    node_id: hit.node_id,
                    score: contribution,
                });
        }
    }
    let mut out: Vec<ScoredChunk> = fused.into_values().collect();
    // 同点は chunk_id で安定化（決定的な出力にする）。
    out.sort_by(|a, b| {
        b.score
            .total_cmp(&a.score)
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });
    out
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
mod tests {
    use super::*;

    fn hit(id: u128, score: f32) -> ScoredChunk {
        ScoredChunk {
            chunk_id: Uuid::from_u128(id),
            node_id: Uuid::from_u128(id + 1000),
            score,
        }
    }

    #[test]
    fn chunk_in_both_lists_ranks_first() {
        // dense: [A, B] / keyword: [C, A] → A が両系統に出るため最上位。
        let dense = vec![hit(1, 0.9), hit(2, 0.8)];
        let keyword = vec![hit(3, 12.0), hit(1, 8.0)];
        let fused = rrf_fuse(&[&dense, &keyword], RRF_K);
        assert_eq!(fused[0].chunk_id, Uuid::from_u128(1));
        assert_eq!(fused.len(), 3, "重複チャンクが除去される");
    }

    #[test]
    fn single_list_preserves_order() {
        let dense = vec![hit(1, 0.9), hit(2, 0.8), hit(3, 0.7)];
        let fused = rrf_fuse(&[&dense], RRF_K);
        let ids: Vec<_> = fused.iter().map(|h| h.chunk_id).collect();
        assert_eq!(
            ids,
            vec![Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)]
        );
    }

    #[test]
    fn empty_lists_produce_empty_output() {
        let empty: Vec<ScoredChunk> = vec![];
        assert!(rrf_fuse(&[&empty, &empty], RRF_K).is_empty());
        assert!(rrf_fuse(&[], RRF_K).is_empty());
    }

    #[test]
    fn output_is_deterministic_for_ties() {
        // 同点（同じ順位で片系統のみ）のとき chunk_id 順で安定する。
        let a = vec![hit(5, 1.0)];
        let b = vec![hit(2, 1.0)];
        let fused = rrf_fuse(&[&a, &b], RRF_K);
        assert_eq!(fused[0].chunk_id, Uuid::from_u128(2));
        assert_eq!(fused[1].chunk_id, Uuid::from_u128(5));
    }
}
