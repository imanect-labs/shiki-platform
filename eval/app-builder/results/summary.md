# AI によるワークフロー作成の評価（GAP-30）

実験: e1a-v0（45 試行）, e1a-v1（45 試行）, e1a-vm（180 試行）, e1a-vn（180 試行）, e1b-v0（15 試行）, e1b-v1（15 試行）, e1b-vm（60 試行）, e1b-vn（60 試行）。モデル claude-haiku-5-5。

採点器の版が違うので外した実験: e0a（採点器の版 7）・e0b（採点器の版 7）。表は 採点器の版 9 で採点した実験だけ（違う規則で採点した試行を同じ表で数えない）。同じ条件で流し直すか、`abeval grade` で採点し直す。

## 1. 見出し

| 条件 | 課題 | 試行 | S1 | S2s | S2d | S4 | 見出し（課題の 95% 区間） | 完了した課題あたりの費用 | 外した課題（採点しなかった試行） |
| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |
| v0/a/medium | 5 | 45 | 0%（0/45） | — | — | — | 0%［0〜0］ | — | 対象外 15 課題 |
| v0/b/medium | 5 | 15 | 27%（4/15） | 25%（1/4） | 25%（1/4） | 20%（3/15） | 20%［7〜33］ | $0.0435 | 対象外 15 課題 |
| v1/a/medium | 5 | 45 | 0%（0/45） | — | — | — | 0%［0〜0］ | — | 対象外 15 課題 |
| v1/b/medium | 5 | 15 | 40%（6/15） | 67%（4/6） | 67%（4/6） | 7%（1/15） | 7%［0〜20］ | $0.1295 | 対象外 15 課題 |
| vm/a/medium | 20 | 180 | 68%（122/180） | 0%（0/122） | 11%（14/122） | — | 60%［44〜75］ | $0.0419 | なし |
| vm/b/medium | 20 | 60 | 93%（56/60） | 2%（1/56） | 11%（6/56） | 70%（42/60） | 70%［53〜85］ | $0.0506 | なし |
| vn/a/medium | 20 | 180 | 74%（133/180） | 1%（1/133） | 18%（24/133） | — | 60%［43〜76］ | $0.0410 | なし |
| vn/b/medium | 20 | 60 | 88%（53/60） | 0%（0/53） | 13%（7/53） | 67%（40/60） | 67%［52〜80］ | $0.0370 | なし |

見出し: Mode A は S1 ∧ ¬S2s ∧ ¬S2d、Mode B は S1 ∧ ¬S2s ∧ ¬S2d ∧ S4。

「課題」は見出しの分母に入った課題の数。条件ごとに課題の集合が違うことがある（版に無い機能の対象外・予定の週に流さない date_window・基盤の失敗で採点しなかった試行）。モードや条件の見出しを並べて読むときは、課題の数と外した課題を見る。下の比べ方は同じモードの中の共通の課題だけを使う。

| 比べたもの | 差 | 95% 区間 | 課題 | 判定 |
| --- | --- | --- | --- | --- |
| v1/a/medium − v0/a/medium | +0 pt | ［+0〜+0］ | 5 | 決着しない（区間が 0 をまたぐ） |
| vm/a/medium − v0/a/medium | +71 pt | ［+33〜+98］ | 5 | 差がある |
| vn/a/medium − v0/a/medium | +71 pt | ［+36〜+96］ | 5 | 差がある |
| vn/a/medium − v1/a/medium（直前の版） | +71 pt | ［+36〜+96］ | 5 | 差がある |
| vm/a/medium − vn/a/medium（直前の版） | +0 pt | ［-8〜+8］ | 20 | 決着しない（区間が 0 をまたぐ） |
| v1/b/medium − v0/b/medium | -13 pt | ［-27〜+0］ | 5 | 決着しない（区間が 0 をまたぐ） |
| vm/b/medium − v0/b/medium | +73 pt | ［+67〜+87］ | 5 | 差がある |
| vn/b/medium − v0/b/medium | +73 pt | ［+67〜+87］ | 5 | 差がある |
| vn/b/medium − v1/b/medium（直前の版） | +87 pt | ［+73〜+100］ | 5 | 差がある |
| vm/b/medium − vn/b/medium（直前の版） | +3 pt | ［-7〜+13］ | 20 | 決着しない（区間が 0 をまたぐ） |

比べ方: 同じモードの中で、1 つ目の条件（名前の順）との差と、版を `eval.toml` の `[report] version_order` の順に並べたときの直前の版（同じモード・effort・手数で測った版のうち、いちばん近いもの）との差（「（直前の版）」の行）を出す。同じ 2 つの条件の組は 1 行にする（直前の版が 1 つ目なら、1 つ目との行だけを出す）。

## 2. 失敗の内訳

- 検証エラーのコード: `ir.schema_violation` 275, `abeval.no_ir` 50, `ir.bad_ref` 50, `ir.script_syntax` 47, `ir.empty_region` 17, `ir.unknown_port` 14, `ir.unknown_scope` 7, `ir.missing_model` 6, `ir.region_leak` 6, `ir.region_exit` 5, `ir.multiple_in_edges` 4, `ir.region_no_entry` 3, `ir.bad_template` 1, `ir.graph_cycle` 1, `ir.unknown_field` 1
- ir_lint（S2s）: `ir.schema_violation` 715, `ir.script_syntax` 36, `ir_lint.output_path` 11, `ir.template_escaped_var` 4, `ir.bad_ref` 3, `ir.unknown_scope` 3, `ir.region_no_entry` 2, `ir.graph_cycle` 1, `ir.region_exit` 1, `ir.region_leak` 1, `ir.unconnected_node` 1, `ir.unknown_port` 1
- 課題の検査: `flow_through` 9, `input_ref` 2
- 動的な検査（S2d）: `unexpected_status` 55, `not_delivered` 43, `dead_edge` 6
- stub の配管で落ちた step の誤りのコード: `invalid` 52, `expr_resolve_error` 16, `map_item_failed` 4, `field_unavailable` 1
- stub の配管で落ちた step（ノードの種類/コード）: `data.bulk_upsert/invalid` 32, `data.record.create/expr_resolve_error` 12, `data.record.update/invalid` 12, `data.record.create/invalid` 5, `control.map/map_item_failed` 4, `data.query/invalid` 3, `workflow.reply/expr_resolve_error` 3, `data.query/field_unavailable` 1, `script.run/expr_resolve_error` 1
- 実モデルの run_case（Mode B）で落ちた step の誤りのコード: `invalid` 17, `expr_resolve_error` 7, `map_item_failed` 1
- 実モデルの run_case（Mode B）で落ちた step（ノードの種類/コード）: `data.bulk_upsert/invalid` 8, `data.record.create/expr_resolve_error` 6, `data.record.create/invalid` 5, `data.query/invalid` 3, `control.map/map_item_failed` 1, `data.record.update/invalid` 1, `script.run/expr_resolve_error` 1
- 生成の結果（v0/a/medium）: emit 42, no_emit 3
- 生成の結果（v1/a/medium）: emit 42, no_emit 3
- 生成の結果（vm/a/medium）: emit 149, no_emit 30, truncated 1
- 生成の結果（vn/a/medium）: emit 159, no_emit 21

## 3. 課題ごと

| 課題 | v0/a/medium | v0/b/medium | v1/a/medium | v1/b/medium | vm/a/medium | vm/b/medium | vn/a/medium | vn/b/medium |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| app01-claim-submit | 0/9 | 1/3 | 0/9 | 0/3 | 6/9 | 3/3 | 7/9 | 3/3 |
| app01-posting-batch | — | — | — | — | 8/9 | 2/3 | 7/9 | 2/3 |
| app02-triage | — | — | — | — | 9/9 | 3/3 | 7/9 | 3/3 |
| app03-daily-submit | — | — | — | — | 7/9 | 3/3 | 9/9 | 1/3 |
| app03-pair | — | — | — | — | 0/9 | 0/3 | 1/9 | 1/3 |
| app03-weekly-summary | — | — | — | — | 0/9 | 0/3 | 0/9 | 1/3 |
| app05-screening-text | — | — | — | — | 9/9 | 3/3 | 7/9 | 3/3 |
| app06-reorder-check | — | — | — | — | 2/9 | 2/3 | 0/9 | 2/3 |
| app06-stock-move | — | — | — | — | 5/9 | 2/3 | 7/9 | 3/3 |
| app08-weekly-kpi | — | — | — | — | 0/9 | 0/3 | 0/9 | 0/3 |
| app09-minutes-tasks | — | — | — | — | 6/9 | 2/3 | 7/9 | 2/3 |
| app10-survey-csv | — | — | — | — | 5/9 | 2/3 | 5/9 | 1/3 |
| app11-checkout | — | — | — | — | 6/9 | 3/3 | 8/9 | 2/3 |
| app11-overdue | — | — | — | — | 4/9 | 0/3 | 7/9 | 0/3 |
| app11-return | — | — | — | — | 6/9 | 3/3 | 2/9 | 2/3 |
| app12-faq-table | 0/9 | 0/3 | 0/9 | 0/3 | 0/9 | 2/3 | 0/9 | 2/3 |
| basic-branch-amount | 0/9 | 0/3 | 0/9 | 0/3 | 9/9 | 3/3 | 9/9 | 3/3 |
| basic-map-rows | 0/9 | 1/3 | 0/9 | 1/3 | 9/9 | 3/3 | 9/9 | 3/3 |
| basic-summary-file | — | — | — | — | 9/9 | 3/3 | 9/9 | 3/3 |
| basic-switch-category | 0/9 | 1/3 | 0/9 | 0/3 | 8/9 | 3/3 | 7/9 | 3/3 |

## 4. effort と手数

| 条件 | 見出し | pass@1 | 費用（合計） | 平均の時間（秒） | 再試行 |
| --- | --- | --- | --- | --- | --- |
| v0/a/medium | 0%（0/45） | 0% | $0.0472 | 6.1 | 0 |
| v0/b/medium | 20%（3/15） | 20% | $0.1305 | 32.0 | 0 |
| v1/a/medium | 0%（0/45） | 0% | $0.0472 | 6.1 | 0 |
| v1/b/medium | 7%（1/15） | 7% | $0.1295 | 32.7 | 0 |
| vm/a/medium | 60%（108/180） | 60% | $0.6710 | 12.0 | 0 |
| vm/b/medium | 70%（42/60） | 70% | $0.8095 | 33.6 | 0 |
| vn/a/medium | 60%（108/180） | 60% | $0.6558 | 12.1 | 0 |
| vn/b/medium | 67%（40/60） | 67% | $0.6667 | 32.0 | 0 |

| 条件（Mode B） | emit の回数 | 検証エラー | 手数 | 上限に達した | emit を読めた | 承認の要求（却下） | 重大 |
| --- | --- | --- | --- | --- | --- | --- | --- |
| v0/b/medium | 4.4 | 8.3 | 5.7 | 80%（12/15） | 100%（15/15） | 0（0） | 0 |
| v1/b/medium | 4.1 | 9.4 | 5.4 | 73%（11/15） | 93%（14/15） | 0（0） | 0 |
| vm/b/medium | 1.7 | 0.7 | 3.4 | 15%（9/60） | 98%（59/60） | 3（3） | 0 |
| vn/b/medium | 1.5 | 0.6 | 3.0 | 10%（6/60） | 93%（56/60） | 4（4） | 0 |

## 5. 費用

- e1a-v0: 試行の分 $0.0472・評価の台帳の累計 $0.0472
- e1a-v1: 試行の分 $0.0472・評価の台帳の累計 $0.0000
- e1a-vm: 試行の分 $0.6710・評価の台帳の累計 $0.6710
- e1a-vn: 試行の分 $0.6558・評価の台帳の累計 $0.6558
- e1b-v0: 試行の分 $0.1305・評価の台帳の累計 $0.1396
- e1b-v1: 試行の分 $0.1295・評価の台帳の累計 $0.1295
- e1b-vm: 試行の分 $0.8095・評価の台帳の累計 $0.8095
- e1b-vn: 試行の分 $0.6667・評価の台帳の累計 $2.0221
- e0a: 試行の分 $0.0438・評価の台帳の累計 $0.0438
- e0b: 試行の分 $0.1282・評価の台帳の累計 $0.1282
- 試行の分は、試行に割り当てた費用（キャッシュから流し直した試行と `--regrade-from` で採点し直した試行は、もとの生成の費用を数える）。評価の台帳の累計は、その実験の名前で実際に払った額（流し直した分も含む）。
- 評価の累計 $4.6466 / $35.0（残り $30.3534）
- 総額（手元の中継の台帳・月で区切らない） $4.8551 / $95.0（残り $90.1449）

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
- vm: commit 4c34dee8ad4e・バイナリ 35a8858f1f30・設定 17b0be22fe40・取り込み 38133a99fc70
- vn: commit eeb6f2e34ef9・バイナリ 637562e91903・設定 17b0be22fe40・取り込み a46b11e4b9fd
