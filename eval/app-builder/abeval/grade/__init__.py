"""採点（GAP-30 §5）。静的（`static`）・動的（`dynamic`）・照合子（`checks`）・採点器の自己テスト用の壊し方（`mutate`）。

製品の規則は写さない。静的な 1 層目は HEAD の検証器そのもの（ir_lint）。出力の path の検査も ir_lint が製品のカタログ
（`output_fields`・`present_if`）で行い、出口は検証の出力（`node_ports`）から読む（`present_if` の「値がある」の判定だけは、
製品の Rust に関数が無いので ir_lint が web の `hasParamAt` を写している・GAP-112）。
"""

# 採点器の版（採点の規則を変えたら上げる。結果には ir_lint のバイナリの sha と一緒に残す）。
# 2: S2d に想定外の状態・想定外の skipped を足し、「入力が届かない」を stub の run だけにした。基盤の失敗で流せ
#    なかった run_case があれば採点しない。見出しは S2s・S2d（Mode B は S4 も）が決まったときだけ真。
# 3: きっかけが schedule・event の課題で、ir_lint の enable_blockers（有効化の前に止める理由）を S2s に数える。
# 4: ir_lint が `ir.unknown_version` を ignored_by_env から errors に戻した（S2s に数える・D-164 (f)）。
# 5: 出力の path の検査を ir_lint（`output_paths`・`ir_lint.output_path`）に移した（Python で判定しない・GAP-112）。
# 6: 実行の日に決まる値を持つ課題で、設定の中の日付の文字（決め打ち）を S2s に数える（date_literal・BUG-247）。
# 7: 日付の文字で、和暦や 2 桁の年に続く月日（「令和8年11月9日」の「11月9日」）も数える。
# 8: run_case の前に、前の run が fixture の置き場に作ったファイルを消す（Mode B で実モデルの run_case の後の
#    stub の配管が、同じ日付の名前のファイルに `name_taken` でぶつかって S2d を誤って真にしていた）。
# 9: llm_dependent の run_case を stub で流して LLM の手前で打ち切るときは、「入力が届かない」を見ない（LLM の後で
#    だけ入力を使う正しい形を誤って落としていた）。
# 10: emit_workflow の `ir` が JSON の文字列でも、読むとオブジェクトになれば IR として数える（製品が 7m〔vf〕から
#     受ける・GAP-119。Mode A の生成の分け方と Mode B の S6 の `abeval.no_ir`）。記録にある文字列の ir はどれも
#     読めないので、前の結果の数は変わらない。
GRADER_VERSION = 10
