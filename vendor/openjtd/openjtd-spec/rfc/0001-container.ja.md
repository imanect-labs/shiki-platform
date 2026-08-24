# RFC 0001: JTD Container Inventory

Status: draft

Observed: 2026-06-18

## Summary

初期 local JTD samples は Compound File Binary (CFB) documents である。

M1 は container inventory だけを実装する。stream payloads は解釈しない。

container layer はまず `cfb` crate を使う。これは OLE/CFB files に対する rhwp の dependency choice と一致する。standard reader が reject する malformed FAT data を CFB file が持つ場合、rjtd は rhwp の `LenientCfbReader` approach を model にした narrow lenient reader に fallback する。

## Command

```sh
cd rjtd
cargo run -p rjtd-cli -- streams ../rjtd-testdata/local-samples/a5.jtd
```

output format は tab-separated:

```text
<entry-kind>    <byte-size>    <path>
```

`entry-kind` は `storage` または `stream`。

stream names 内の ASCII control characters は表示用に escape される。例: `\x04JSRV_SegmentInformation`。

## Lenient FAT Fallback

一部の local `.jtd` samples には duplicate sector pointers のような FAT inconsistencies が含まれる。rhwp は同等の HWP files を、別 dependency 追加ではなく direct `LenientCfbReader` implementation で処理する。

rjtd はその pattern に従う。

- standard `cfb` parsing を最初に試す。
- lenient parsing は、standard parsing が CFB らしい file で失敗した後だけ使う。
- stream inventory と stream reads は同じ fallback を共有する。
- successful lenient open 後も、missing streams は `not found` errors のままにする。

Current local sweep:

| Command | Local samples checked | Result |
| --- | ---: | --- |
| `rjtd info` | 61 | 61 opened |
| `rjtd cat` | 61 | 61 opened; 2 use embedded `SsmgV.01` fragments instead of named `/DocumentText` |

## Local Samples

以下の sample names は、初期 container inventory work で使った local files を指す。
観察済み stream layouts を比較するための examples である。

| Sample | Entry count | Notes |
| --- | ---: | --- |
| `a5.jtd` | 32 | `LineMark`, `PageMark`, `PaperMark`, `PageLayoutStyleHeader` を持つ |
| `46.jtd` | 31 | `LineMark`, `PageMark`, `PaperMark` を持つ |
| `b6.jtd` | 31 | `LineMark`, `PageMark`, `PaperMark` を持つ |
| `shinsyo.jtd` | 28 | initial inventory では `LineMark`, `PageMark`, `PaperMark` を持たない |
| `a6.jtd` | 28 | initial inventory では `LineMark`, `PageMark`, `PaperMark` を持たない |

## Common Top-Level Streams

これらの top-level streams は five local samples すべてに現れる。

- `/\x04JSRV_SegmentInformation`
- `/\x04JSRV_SummaryInformation`
- `/\x05SummaryInformation`
- `/AutoTextInfo`
- `/DocumentEditStyles`
- `/DocumentPeripheralThree`
- `/DocumentPeripheralTwo`
- `/DocumentText`
- `/DocumentTextPositionTables`
- `/DocumentViewStyles`
- `/Font`
- `/Footnote`
- `/Header`
- `/MarkTag`
- `/PageLayoutStyle`
- `/ReferenceInfo`
- `/RelatedDocuments`
- `/TextLayoutStyle`
- `/ThinkingTemplate`

## Common Storage Tree

five samples はすべて次の macro storage tree を含む。

```text
/DocumentMacro
/DocumentMacro/\x04JSRV_SegmentInformation
/DocumentMacro/Macros
/DocumentMacro/Macros/\x04JSRV_SegmentInformation
/DocumentMacro/Macros/BaseStorage0
/DocumentMacro/Macros/BaseStorage0/\x04JSRV_SegmentInformation
/DocumentMacro/Macros/BaseStorage0/InfoStream
/DocumentMacro/Macros/BaseStorage0/MacrosStream
/DocumentMacro/Macros/BaseStorage0/MacrosStreamStyle3
```

## Early Hypotheses

- `/DocumentText` は圧倒的に大きな common stream なので、primary body text candidate である。
- `/DocumentTextPositionTables` は text positions を index または map している可能性が高い。
- `/DocumentViewStyles`、`/DocumentEditStyles`、`/TextLayoutStyle`、`/PageLayoutStyle`、`/Font` は style/model candidates である。
- `JSRV_*` streams と `SummaryInformation` streams は、理解される前でも保存すべきである。

## Next Questions

- `/DocumentText` が compressed、encoded、segmented、encrypted のいずれかを判定する。
- `/DocumentText` size と content を、known text を持つ trivial documents と比較する。
- page size variants が `a5`、`a6`、`b6`、`46` の違いを説明するか確認する。
- line/page mark streams が optional layout caches なのか required model data なのか判断する。
- named `/DocumentText` ではなく embedded `SsmgV.01` fragments を使う samples の proper object/stream boundaries を特定する。
