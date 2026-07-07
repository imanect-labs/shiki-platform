//! DAG 前進の readiness/skip 判定（**純関数**・engine.md §4.3/§4.4）。
//!
//! エッジ状態は永続化せず、源 step の `taken_ports` から導出する:
//! - `live`  = `from_port ∈ 源step.taken_ports`
//! - `dead`  = 源step が terminal かつ非 live
//! - `unresolved` = 源step が未 terminal
//!
//! この判定はワーカーの前進 TX が使う中核ロジックで、単体＋proptest の主戦場。

/// 1 本の入エッジの解決状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EdgeState {
    /// 源 step が terminal で from_port が taken_ports に含まれる。
    Live,
    /// 源 step が terminal で from_port が含まれない。
    Dead,
    /// 源 step が未 terminal（まだ確定していない）。
    Unresolved,
}

/// 入エッジ 1 本の情報（源ノード id ＋そのエッジの状態）。
#[derive(Debug, Clone)]
pub struct InEdge {
    /// 源ノード id（join の出力ラベルに使う）。
    pub from: String,
    pub state: EdgeState,
}

/// join のモード（engine.md §4.4）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinMode {
    /// 全入エッジ解決まで待つ。
    All,
    /// 初回 live 1 回で発火。
    Any,
}

/// readiness 判定の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Readiness {
    /// 実行可能（ready へ）。
    Ready,
    /// スキップ（skipped へ・全出力ポート dead を下流へ伝播）。
    Skip,
    /// まだ待つ（pending のまま）。
    NotYet,
}

/// join 以外のノードの readiness を判定する。
///
/// 入エッジ制約（V2）で join 以外は入エッジ高々 1 本。0 本（root）は run 開始時に ready 化されるため
/// ここでは扱わない（呼び出しは入エッジありのノードに対して行う）。
pub fn readiness_non_join(edges: &[InEdge]) -> Readiness {
    // 入エッジ 1 本前提（V2）。防御的に「1 本でも live なら ready、全 dead なら skip、他は待つ」。
    if edges.iter().any(|e| e.state == EdgeState::Live) {
        Readiness::Ready
    } else if edges.iter().any(|e| e.state == EdgeState::Unresolved) {
        Readiness::NotYet
    } else if edges.is_empty() {
        // 到達し得ないが安全側で NotYet（root は別経路で ready 化）。
        Readiness::NotYet
    } else {
        // 全 dead。
        Readiness::Skip
    }
}

/// join ノードの readiness を判定する（engine.md §4.4）。
pub fn readiness_join(mode: JoinMode, edges: &[InEdge]) -> Readiness {
    match mode {
        JoinMode::All => {
            // 全解決まで待つ。全解決後、全 dead なら skip、1 本でも live なら ready。
            if edges.iter().any(|e| e.state == EdgeState::Unresolved) {
                Readiness::NotYet
            } else if edges.iter().any(|e| e.state == EdgeState::Live) {
                Readiness::Ready
            } else {
                Readiness::Skip
            }
        }
        JoinMode::Any => {
            // 初回 live で発火。全解決して live 無し（全 dead）なら skip、未解決残あれば待つ。
            if edges.iter().any(|e| e.state == EdgeState::Live) {
                Readiness::Ready
            } else if edges.iter().any(|e| e.state == EdgeState::Unresolved) {
                Readiness::NotYet
            } else {
                Readiness::Skip
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edge(state: EdgeState) -> InEdge {
        InEdge {
            from: "src".into(),
            state,
        }
    }

    #[test]
    fn non_join_single_edge() {
        assert_eq!(
            readiness_non_join(&[edge(EdgeState::Live)]),
            Readiness::Ready
        );
        assert_eq!(
            readiness_non_join(&[edge(EdgeState::Dead)]),
            Readiness::Skip
        );
        assert_eq!(
            readiness_non_join(&[edge(EdgeState::Unresolved)]),
            Readiness::NotYet
        );
    }

    #[test]
    fn join_all_waits_for_full_resolution() {
        let edges = vec![edge(EdgeState::Live), edge(EdgeState::Unresolved)];
        assert_eq!(readiness_join(JoinMode::All, &edges), Readiness::NotYet);

        let resolved_live = vec![edge(EdgeState::Live), edge(EdgeState::Dead)];
        assert_eq!(
            readiness_join(JoinMode::All, &resolved_live),
            Readiness::Ready
        );

        let all_dead = vec![edge(EdgeState::Dead), edge(EdgeState::Dead)];
        assert_eq!(readiness_join(JoinMode::All, &all_dead), Readiness::Skip);
    }

    #[test]
    fn join_any_fires_on_first_live() {
        let one_live = vec![edge(EdgeState::Live), edge(EdgeState::Unresolved)];
        assert_eq!(readiness_join(JoinMode::Any, &one_live), Readiness::Ready);

        let pending = vec![edge(EdgeState::Unresolved), edge(EdgeState::Dead)];
        assert_eq!(readiness_join(JoinMode::Any, &pending), Readiness::NotYet);

        let all_dead = vec![edge(EdgeState::Dead), edge(EdgeState::Dead)];
        assert_eq!(readiness_join(JoinMode::Any, &all_dead), Readiness::Skip);
    }

    // proptest: readiness は決して panic せず、Live があれば必ず Ready（join 含む）。
    proptest::proptest! {
        #[test]
        fn readiness_never_panics_and_live_implies_ready(
            states in proptest::collection::vec(
                proptest::sample::select(vec![EdgeState::Live, EdgeState::Dead, EdgeState::Unresolved]),
                1..8usize,
            )
        ) {
            let edges: Vec<InEdge> = states.iter().map(|s| edge(*s)).collect();
            let has_live = states.contains(&EdgeState::Live);
            let has_unresolved = states.contains(&EdgeState::Unresolved);

            // join(any) と non_join は live があれば必ず Ready。
            if has_live {
                proptest::prop_assert_eq!(readiness_join(JoinMode::Any, &edges), Readiness::Ready);
            }
            // join(all) は未解決があれば NotYet、無ければ live→Ready / 全dead→Skip。
            let all_res = readiness_join(JoinMode::All, &edges);
            if has_unresolved {
                proptest::prop_assert_eq!(all_res, Readiness::NotYet);
            } else if has_live {
                proptest::prop_assert_eq!(all_res, Readiness::Ready);
            } else {
                proptest::prop_assert_eq!(all_res, Readiness::Skip);
            }
        }
    }
}
