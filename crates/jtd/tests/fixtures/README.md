# テストフィクスチャ（トラックJTD）

忠実度ハーネス（`../fidelity_it.rs`）が使う実ファイルと、その比較対象。

## 同梱しているもの

いずれも厚生労働省ホームページの配布物で、同省の
[利用規約](https://www.mhlw.go.jp/chosakuken/index.html)により
**公共データ利用規約（第1.0版）／PDL1.0** が適用される。
[PDL1.0](https://www.digital.go.jp/resources/open_data/public_data_license_v1.0) は
CC BY 4.0 と互換で、国は利用者が CC BY 4.0 に従って利用することを許諾している。

| ファイル | 出典 |
| --- | --- |
| `f1.jtd` | 厚生労働省「研究計画書（新規申請用）」<br>https://www.mhlw.go.jp/wp/kenkyu/koubo04/dl/f1.jtd （配布ページ: `.../koubo04/kh08.html`） |
| `f1.reference.txt` | 同省が同じ様式で配布している text 版<br>https://www.mhlw.go.jp/wp/kenkyu/koubo04/dl/f1.txt<br>**加工内容: 文字コードを CP932 から UTF-8 へ変換（内容は変えていない）** |
| `betu.jtd` | 厚生労働省「第一種使用規程承認申請手続等について（別紙様式）」<br>https://www.mhlw.go.jp/general/seido/kousei/i-kenkyu/seibutu/dl/betu.jtd<br>（[e-Gov データポータル](https://data.e-gov.go.jp/data/dataset/mhlw_20140917_0897)にも登録されているが、そちらのページはライセンス欄を持たないので出典は実ファイルの配布元を挙げる） |

`*.golden.txt` は**我々の抽出結果のスナップショット**で、退行を止めるためだけのもの。
配布物ではないので上の規約の対象外。意図した改善で内容が変わったら中身を確認して更新する
（`cargo run -p shiki-jtd --example jtd-dump -- --text <出力> <入力.jtd>`）。

## 同梱していないもの

日本コンクリート工学会「和文原稿作成テンプレート」（`tpwin_jp.jtd`）は再配布条件が
未確認のため同梱しない。`scripts/fetch-jtd-fixtures.sh` が `external/` へ取得する
（`.gitignore` 済み）。取得に失敗したらスクリプトは**失敗する**。

`external/` を使うのは `scripts/jtd-fidelity.sh`（視覚比較）だけで、**cargo test は
これに依存しない**。ネットワークの有無で CI の結果が変わらないようにしてある。
視覚比較のスクリプトは `external/` が無ければ**失敗する**（取得を促す）。

## オラクルの使い分け

- **`f1.reference.txt`（一次・独立）** — 我々の実装と無関係に同省が作った text 版。
  ただし**手作りの別版**なので、空白の入れ方や表のセルの並べ方は一致しない。
  よって正規化したうえで
  **recall（公式テキストの何割を順序どおり回収できたか）と precision（我々の出力の
  何割が公式テキストで裏を取れるか）の両方**を見る。recall だけだと、レコードの
  ペイロードやバイナリ領域が本文へ漏れる退行を 1 文字も検出できない。
- **`*.golden.txt`（二次・自前）** — 我々自身の出力の固定。改善も退行もここに出る。
- **配布 PDF（`f1.pdf`）** — 罫線位置とページ割りの真値。リポジトリには置かず、
  `scripts/jtd-fidelity.sh` が取得して視覚比較の参照側に置く。
  本格的に使うのは JTD.3 / JTD.4。

`betu.jtd` には同省が配布する text 版も PDF 版も無いため、独立オラクルが無い
（ゴールデン固定のみ）。
