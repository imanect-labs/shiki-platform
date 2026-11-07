# AI によるワークフロー作成の評価（GAP-30）

実験: e1a-v0（45 試行）, e1a-v1（45 試行）, e1a-vn（180 試行）, e1b-v0（15 試行）, e1b-v1（15 試行）, e1b-vn（60 試行）。モデル claude-haiku-5-5。

採点器の版が違うので外した実験: e0a（採点器の版 7）・e0b（採点器の版 7）。表は 採点器の版 9 で採点した実験だけ（違う規則で採点した試行を同じ表で数えない）。同じ条件で流し直すか、`abeval grade` で採点し直す。

## 1. 見出し

| 条件 | 課題 | 試行 | S1 | S2s | S2d | S4 | 見出し（課題の 95% 区間） | 完了した課題あたりの費用 | 外した課題（採点しなかった試行） |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| v0/a/medium | 5 | 45 | 0%（0/45） | — | — | — | 0%［0〜0］ | — | 対象外 15 課題 |
| v0/b/medium | 5 | 15 | 27%（4/15） | 25%（1/4） | 25%（1/4） | 20%（3/15） | 20%［7〜33］ | $0.0435 | 対象外 15 課題 |
| v1/a/medium | 5 | 45 | 0%（0/45） | — | — | — | 0%［0〜0］ | — | 対象外 15 課題 |
| v1/b/medium | 5 | 15 | 40%（6/15） | 67%（4/6） | 67%（4/6） | 7%（1/15） | 7%［0〜20］ | $0.1295 | 対象外 15 課題 |
| vn/a/medium | 20 | 180 | 74%（133/180） | 1%（1/133） | 18%（24/133） | — | 60%［43〜76］ | $0.0410 | なし |
| vn/b/medium | 20 | 60 | 88%（53/60） | 0%（0/53） | 13%（7/53） | 67%（40/60） | 67%［52〜80］ | $0.0370 | なし |

見出し: Mode A は S1 ∧ ¬S2s ∧ ¬S2d、Mode B は S1 ∧ ¬S2s ∧ ¬S2d ∧ S4。

「課題」は見出しの分母に入った課題の数。条件ごとに課題の集合が違うことがある（版に無い機能の対象外・予定の週に流さない date_window・基盤の失敗で採点しなかった試行）。モードや条件の見出しを並べて読むときは、課題の数と外した課題を見る。下の比べ方は同じモードの中の共通の課題だけを使う。

| 比べたもの | 差 | 95% 区間 | 課題 | 判定 |
| --- | --- | --- | --- | --- |
| v1/a/medium − v0/a/medium | +0 pt | ［+0〜+0］ | 5 | 決着しない（区間が 0 をまたぐ） |
| vn/a/medium − v0/a/medium | +71 pt | ［+36〜+96］ | 5 | 差がある |
| v1/b/medium − v0/b/medium | -13 pt | ［-27〜+0］ | 5 | 決着しない（区間が 0 をまたぐ） |
| vn/b/medium − v0/b/medium | +73 pt | ［+67〜+87］ | 5 | 差がある |

## 2. 失敗の内訳

- 検証エラーのコード: `ir.schema_violation` 269, `ir.bad_ref` 50, `ir.script_syntax` 20, `abeval.no_ir` 19, `ir.empty_region` 17, `ir.unknown_port` 14, `ir.missing_model` 6, `ir.unknown_scope` 6, `ir.region_exit` 4, `ir.region_leak` 4, `ir.multiple_in_edges` 3, `ir.region_no_entry` 2, `ir.bad_template` 1, `ir.graph_cycle` 1
- ir_lint（S2s）: `ir.schema_violation` 711, `ir.script_syntax` 14, `ir_lint.output_path` 11, `ir.template_escaped_var` 4, `ir.bad_ref` 3, `ir.unknown_scope` 3, `ir.graph_cycle` 1, `ir.region_exit` 1, `ir.region_no_entry` 1, `ir.unconnected_node` 1, `ir.unknown_port` 1
- 課題の検査: `flow_through` 8, `input_ref` 2
- 動的な検査（S2d）: `unexpected_status` 36, `not_delivered` 34, `dead_edge` 6
- stub の配管で落ちた step の誤りのコード: `invalid` 35, `expr_resolve_error` 12, `map_item_failed` 3, `field_unavailable` 1
- stub の配管で落ちた step（ノードの種類/コード）: `data.bulk_upsert/invalid` 20, `data.record.create/expr_resolve_error` 9, `data.record.update/invalid` 7, `data.record.create/invalid` 5, `control.map/map_item_failed` 3, `data.query/invalid` 3, `workflow.reply/expr_resolve_error` 3, `data.query/field_unavailable` 1
- 実モデルの run_case（Mode B）で落ちた step の誤りのコード: `invalid` 13, `expr_resolve_error` 6, `map_item_failed` 1
- 実モデルの run_case（Mode B）で落ちた step（ノードの種類/コード）: `data.record.create/expr_resolve_error` 6, `data.record.create/invalid` 5, `data.bulk_upsert/invalid` 4, `data.query/invalid` 3, `control.map/map_item_failed` 1, `data.record.update/invalid` 1
- 生成の結果（v0/a/medium）: emit 42, no_emit 3
- 生成の結果（v1/a/medium）: emit 42, no_emit 3
- 生成の結果（vn/a/medium）: emit 159, no_emit 21

## 3. 課題ごと

| 課題 | v0/a/medium | v0/b/medium | v1/a/medium | v1/b/medium | vn/a/medium | vn/b/medium |
| --- | --- | --- | --- | --- | --- | --- |
| app01-claim-submit | 0/9 | 1/3 | 0/9 | 0/3 | 7/9 | 3/3 |
| app01-posting-batch | — | — | — | — | 7/9 | 2/3 |
| app02-triage | — | — | — | — | 7/9 | 3/3 |
| app03-daily-submit | — | — | — | — | 9/9 | 1/3 |
| app03-pair | — | — | — | — | 1/9 | 1/3 |
| app03-weekly-summary | — | — | — | — | 0/9 | 1/3 |
| app05-screening-text | — | — | — | — | 7/9 | 3/3 |
| app06-reorder-check | — | — | — | — | 0/9 | 2/3 |
| app06-stock-move | — | — | — | — | 7/9 | 3/3 |
| app08-weekly-kpi | — | — | — | — | 0/9 | 0/3 |
| app09-minutes-tasks | — | — | — | — | 7/9 | 2/3 |
| app10-survey-csv | — | — | — | — | 5/9 | 1/3 |
| app11-checkout | — | — | — | — | 8/9 | 2/3 |
| app11-overdue | — | — | — | — | 7/9 | 0/3 |
| app11-return | — | — | — | — | 2/9 | 2/3 |
| app12-faq-table | 0/9 | 0/3 | 0/9 | 0/3 | 0/9 | 2/3 |
| basic-branch-amount | 0/9 | 0/3 | 0/9 | 0/3 | 9/9 | 3/3 |
| basic-map-rows | 0/9 | 1/3 | 0/9 | 1/3 | 9/9 | 3/3 |
| basic-summary-file | — | — | — | — | 9/9 | 3/3 |
| basic-switch-category | 0/9 | 1/3 | 0/9 | 0/3 | 7/9 | 3/3 |

## 4. effort と手数

| 条件 | 見出し | pass@1 | 費用（合計） | 平均の時間（秒） | 再試行 |
| --- | --- | --- | --- | --- | --- |
| v0/a/medium | 0%（0/45） | 0% | $0.0472 | 6.1 | 0 |
| v0/b/medium | 20%（3/15） | 20% | $0.1305 | 32.0 | 0 |
| v1/a/medium | 0%（0/45） | 0% | $0.0472 | 6.1 | 0 |
| v1/b/medium | 7%（1/15） | 7% | $0.1295 | 32.7 | 0 |
| vn/a/medium | 60%（108/180） | 60% | $0.6558 | 12.1 | 0 |
| vn/b/medium | 67%（40/60） | 67% | $0.6667 | 32.0 | 0 |

| 条件（Mode B） | emit の回数 | 検証エラー | 手数 | 上限に達した | emit を読めた | 承認の要求（却下） | 重大 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| v0/b/medium | 4.4 | 8.3 | 5.7 | 80%（12/15） | 100%（15/15） | 0（0） | 0 |
| v1/b/medium | 4.1 | 9.4 | 5.4 | 73%（11/15） | 93%（14/15） | 0（0） | 0 |
| vn/b/medium | 1.5 | 0.6 | 3.0 | 10%（6/60） | 93%（56/60） | 4（4） | 0 |

## 5. 費用

- e1a-v0: 試行の分 $0.0472・評価の台帳の累計 $0.0472
- e1a-v1: 試行の分 $0.0472・評価の台帳の累計 $0.0000
- e1a-vn: 試行の分 $0.6558・評価の台帳の累計 $0.6558
- e1b-v0: 試行の分 $0.1305・評価の台帳の累計 $0.1396
- e1b-v1: 試行の分 $0.1295・評価の台帳の累計 $0.1295
- e1b-vn: 試行の分 $0.6667・評価の台帳の累計 $2.0221
- e0a: 試行の分 $0.0438・評価の台帳の累計 $0.0438
- e0b: 試行の分 $0.1282・評価の台帳の累計 $0.1282
- 試行の分は、試行に割り当てた費用（キャッシュから流し直した試行と `--regrade-from` で採点し直した試行は、もとの生成の費用を数える）。評価の台帳の累計は、その実験の名前で実際に払った額（流し直した分も含む）。
- 評価の累計 $3.1662 / $35.0（残り $31.8338）
- 総額（手元の中継の台帳・月で区切らない） $3.3723 / $95.0（残り $91.6277）

## 6. 限界

- 生成と評価が同じモデル（Claude Haiku 5.5）。中核の課題は手で書き、生成した課題は分けて集計する。
- このマシンで外した機能: RAG・agent.invoke・Office・ai.decide（評価の設定に判定用のモデルが無い）。
- 1 試行の費用の見積もり（`eval.toml` の `[estimates]`）は、Mode A と Mode B を E0（2026-11-07・4 課題）の実測で置き換えた。effort high と手数 12 は推測のまま。
- 製品は手の間で思考ブロックを送り返さない（anthropic.rs）。説明文の外の理由で Mode B の品質が下がり得る。
- 温度で決定性を担保できないので、課題あたり数回の試行の幅で見る。20 課題 × 3 試行で見分けられる差は 15〜20 ポイントまで。
- 聞き返しの判定は単純な規則（「？」で終わる・課題の語）なので取りこぼしがある（S8 の「判定できない」に残す）。
- 1 発話のトークンの上限（30 万）と、手数の上限（既定 6）が効く条件で測っている。

版:

- v0: commit 81a9899b5f38・バイナリ bfb4f4ca2672・設定 17b0be22fe40・取り込み eab2739c95d6
- v1: commit 71e9b79aaad5・バイナリ b4e9311436cb・設定 17b0be22fe40・取り込み eab2739c95d6
- vn: commit eeb6f2e34ef9・バイナリ 637562e91903・設定 17b0be22fe40・取り込み a46b11e4b9fd
