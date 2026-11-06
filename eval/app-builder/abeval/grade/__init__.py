"""採点（GAP-30 §5）。静的（`static`）・動的（`dynamic`）・照合子（`checks`）・採点器の自己テスト用の壊し方（`mutate`）。

製品の規則は写さない。静的な 1 層目は HEAD の検証器そのもの（ir_lint）、出力の形と出口はカタログ（`ir_lint --catalog`）
と検証の出力（`node_ports`）から読む。
"""

# 採点器の版（採点の規則を変えたら上げる。結果には ir_lint のバイナリの sha と一緒に残す）。
# 2: S2d に想定外の状態・想定外の skipped を足し、「入力が届かない」を stub の run だけにした。基盤の失敗で流せ
#    なかった run_case があれば採点しない。見出しは S2s・S2d（Mode B は S4 も）が決まったときだけ真。
GRADER_VERSION = 2
