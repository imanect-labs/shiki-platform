# RFC 0004: Initial Document Model Export

Status: draft

Observed: 2026-06-18

## Summary

`rjtd export` は現在、必要な layer boundary に従う。

```text
Document File
  -> Container
  -> DocumentParser
  -> Document Model
  -> Exporter
```

Exporter は raw JTD/JTT/JTTC bytes を直接読まない。

## Implemented Commands

```sh
cargo run -p rjtd-cli -- export ../rjtd-testdata/local-samples/a5.jtd --format json
cargo run -p rjtd-cli -- export ../rjtd-testdata/local-samples/a5.jtd --format md
cargo run -p rjtd-cli -- export ../rjtd-testdata/local-samples/a5.jtd --format text
cargo run -p rjtd-cli -- export ../rjtd-testdata/local-samples/setsuden_05.jttc --format json
```

`html` は planned format として認識されるが、未実装である。

## Initial Model Mapping

current model bridge は意図的に minimal である。

- extracted text は LF line endings に normalize される。
- non-empty lines は `Paragraph` blocks になる。
- each paragraph は one `TextRun` を含む。
- raw `/DocumentText` stream、inner `.jttc` `/DocumentText`、または embedded fragment payload は `Document` model に保存される。
- observed style/layout streams は record semantics が decode される前に named `UnknownStyle` model entries として保存される。
- style references は現在 `null`。
- unknown styles、objects、blocks は future preservation work のために model API の一部として残る。

この mapping は final JTD model ではない。project early stage で exporter boundary を実体化するために存在する。

## JSON Shape

initial JSON export は compact で model-oriented である。

```json
{
  "metadata": { "title": null },
  "blocks": [
    {
      "type": "paragraph",
      "style": null,
      "inlines": [
        { "type": "text", "text": "銀河鉄道の夜", "style": null }
      ]
    }
  ],
  "unknownStyles": [],
  "unknownObjects": [],
  "textCountRanges": [],
  "textControlBoundaries": [],
  "textBoundaryCandidates": [],
  "textParagraphBoundaryCandidates": [],
  "rawStreams": [
    { "name": "/DocumentText", "size": 240104 }
  ]
}
```

unknown payloads が model に存在する場合、JSON exporter が silently discard しないよう hex strings として出力する。

raw streams は JSON では name と size で要約される。bytes は later round-trip と structured parsing work のために in-memory model に保存される。

observed style/layout streams は現在 `unknownStyles` に stream path、raw payload、observed family、big-endian header fields、neutral record boundary candidates 付きで出力される。`a5.jtd` では次の style streams が保存される。

```text
/DocumentEditStyles
/DocumentViewStyles
/TextLayoutStyle
/PageLayoutStyle
/PageLayoutStyleHeader
```

app-core compatibility JSON も、これらを decoded paragraph styles として見せずに source として報告する。`getDocumentInfo` は `styleStreamCount`、`styleCandidateCount`、`styleCandidateNames`、`styleStreams`、`getStyleList` は `sourceStreamCount`、`getStyleDetail(0)` は `decoded:false` と `sourceStreams` を含む。

labeled `/TextLayoutStyle` records については、app-core style APIs が stable candidate IDs、observed names、`decoded:false`、source stream/offset/code metadata を持つ rhwp-shaped style candidates を expose する。これらの candidates はまだ paragraph や text-run style references ではない。

user が app-core `applyStyle` 経由でこれらの candidates を適用すると、rjtd は candidate ID を in-memory paragraph `StyleRef` として保存する。`getStyleAt` と model JSON はその fallback style reference を観測できる。これは editable model state であり、original JTD body/style streams を書き戻せることの証明ではない。

同じ observation data は CLI diagnostics からも利用できる。`rjtd style-records <file>` は preserved style stream families、header candidates、record layouts、offsets、codes、payload lengths、labels を出力する。`rjtd style-candidates <file>` は cross-sample correlation 用に labeled `/TextLayoutStyle` subset を stable per-document candidate rows として出力する。`rjtd text-layout-style-records <file>` は unlabeled records、payload lengths、digests、BE16 fields、short previews、labels、labeled records の candidate IDs を含む all `/TextLayoutStyle` records を出力する。`rjtd document-view-style-groups <file>` は `/DocumentViewStyles` group record payload lengths、digests、short previews を出力する。`rjtd text-position-style-context <file>` は `TCntV.01` tail fields と text/page style candidate IDs・record indexes・`/DocumentViewStyles` group records を比較し、`rjtd text-position-style-summary <file>` はこれらの hits を tail field ごとに aggregate し、`rjtd text-position-count-tail-field-roles <file>` は selected deltas で tail fields と adjacent field pairs を document-text unit/text hits と比較する。これらの hits は decoded model references ではなく evidence として扱う。

parsed `TextRun` values は source stream map から導ける場合に `/DocumentText` byte/UTF-16 source span を持つ。valid `TCntV.01` text-count entries は decoded-false `textCountRanges` として document model に保存される。JSON export と app-core `getDocumentInfo` は observed family、chosen start/end/span、declared start/end、tail fields、raw entry bytes、model text runs に対する byte/unit `documentTextOverlaps`、および `0x001c` や `0x000e` など observed delimiter candidates で分割した `/DocumentText` intervals に対する candidate `controlRangeOverlaps` を expose する。これらの overlaps は decoded-false top-level `textBoundaryCandidates` としても mirror され、application tools が recovered paragraph semantics と扱わずに possible paragraph-boundary evidence を inspect できる。nonzero-span、strict unit `0x001c`、dual-layout-exact subset は decoded-false `textParagraphBoundaryCandidates` として別に expose される。parser はまだこれらの ranges や candidates を paragraph styles、text-run styles、real paragraph boundaries、final layout geometry に attach しない。current 61 local samples では JSON export は全 samples で成功し、10 samples が non-empty `textCountRanges` を expose し、同じ 10 samples が少なくとも 1 件の `documentTextOverlaps` を expose し、`textParagraphBoundaryCandidates` は合計 10 rows を保存する。すべて `iwata_file` 由来である。

`/DocumentText` control boundary codes は decoded-false `textControlBoundaries` として別に保存される。named `/DocumentText` stream 由来の boundary では、JSON export と app-core `getDocumentInfo` が byte/UTF-16 unit source span も expose する。これらの boundary entries はまだ paragraph、line、style、object semantics には promote せず、後続の boundary/layout inference の evidence として保持する。

non-text visual/object stream evidence は decoded-false `objectStreamCandidates` として別に保存される。JSON export と app-core `getDocumentInfo` は candidate path、size、reason list、path-derived ownership candidate、image signature offsets、image payload spans、image payload object envelopes、SVG offsets、SO marker offsets、short payload prefix を expose する。model は complete detected image payload bytes を内部に保持し、JSON は payload metadata、undecoded header/trailer envelope metadata、conservative declared-length candidates、prefix numeric header fields、source-path candidates、short `payloadPrefixHex` preview を report する。`getValidationWarnings` はこれらを `JtdObjectStreamCandidateDiagnosticOnly` として report する。これらの entries は後続の image、shape、SVG、object、table recovery の evidence であり、まだ page paint operations や decoded Ichitaro object geometry ではない。

app-core navigation では、source-spanned text runs との隣接関係が安全に確認できる preserved `textControlBoundaries` を fallback paragraph character offsets に project できる。そのため `getControlTextPositions` は rhwp と同じ numeric-array return shape を保つ。nearest-control APIs は false table/picture/shape/equation/field/bookmark semantics を付けず、`type:"jtdControl"`、`decoded:false` diagnostics を返すことがある。

`getPageControlLayout` は同じ projection を page-scoped diagnostics に使う。fallback bounding boxes、`secIdx`、`paraIdx`、`controlIdx`、`charPos`、source、code、`decoded:false` を持つ control entries を返せるが、これは evidence-preserving placeholder であり decoded Ichitaro object geometry ではない。

app-core `getPageLayerTree` は fallback `pageBackground` paint op、fallback `textRun` paint ops、rhwp-shaped top-level `textSources` および per-op `source` spans を出力する。parsed `/DocumentText` spans が分かる場合、text entries は JTD byte/unit source ranges も含む。layer envelope は schema/resource table versions、output options、empty font resources、feature lists、fallback `textV2` diagnostics も含む。geometry はまだ rjtd の fallback text paginator によるもので、decoded Ichitaro page layout ではない。

`getCanvasKitReplayPlan` は同じ fallback projection を使う。rhwp-compatible `default` と `compat` modes を受け付け、unsupported mode を reject し、fallback `pageBackground`/`textRun` operations を direct `replayPlane:"background"`/`replayPlane:"flow"` items として report することで app render policy diagnostics が empty page のように見えないようにする。

current style stream summaries は neutral observation labels を意図的に使う。

- `family:"ssmg"` は `SsmgV.01` で始まる streams を表し、現時点では `TextLayoutStyle`、`PageLayoutStyle`、多くの `PageLayoutStyleHeader` payloads を含む。
- `family:"table"` は observed non-`SsmgV.01` style tables を表し、現時点では `DocumentEditStyles` と `DocumentViewStyles` を含む。
- `headerU32Be` と `headerU16Be` は final field names をまだ割り当てず、big-endian header candidates を expose する。
- `recordLayout:"ssmg-slots"` は `0x5555` または `0x4444` markers を持つ observed Ssmg slot records を、`offset`、numeric `code`、`codeHex`、`payloadLength`、存在する場合は conservative UTF-16BE `label` candidate で報告する。
- `recordLayout:"sequential"` は `u16 code + u16 payload_len + payload` に合う observed table records を報告する。`a5.jtd` では `/DocumentViewStyles` が現在 48 candidates を expose する。
- `recordLayout:"none"` は safe observed record boundary がまだ見つかっていないことを意味する。raw payload は引き続き保存される。

## Local Sample Result

`a5.jtd` では、blank lines を省くと JSON export は現在 490 paragraph blocks を生成する。

first block:

```text
銀河鉄道の夜				宮沢 賢治
```

table of contents は次のような recovered inline base text を含む。

```text
一、午后の授業
五、天気輪の柱
八、鳥を捕る人
九、ジョバンニの切符
```

観察済み `.jttc` template samples では、JSON export は `/JSCompDocument` を開き、inner CFB を decompress し、inner `/DocumentText` stream summary を保存する。

```json
{
  "blocks": [],
  "rawStreams": [
    { "name": "/DocumentText", "size": 564 }
  ]
}
```

これらの samples は extracted text が blank/control-heavy であるため、現在 zero non-empty paragraph blocks を生成する。

観察済み `cfb-embedded-document-text` samples では、JSON export は recovered source を `/EmbeddedDocumentText` として記録する。

```json
{
  "blocks": [
    { "type": "paragraph", "inlines": [{ "type": "text", "text": "参加者募集中団体名氏　名", "style": null }], "style": null }
  ],
  "rawStreams": [
    { "name": "/EmbeddedDocumentText", "size": 77380 }
  ]
}
```

この size は `ichitaro-20030706231249-success-001-success_data-fujimoto_file.jtd` 由来であり、他の embedded samples では異なる。

PDF regression coverage のため、`rjtd-export` には conditional local-sample smoke test がある。`rjtd-testdata/local-samples` が存在する場合、available `.jtd`、`.jtt`、`.jttc` samples をすべて parse し、各 document を PDF bytes に export したうえで PDF header、page marker、EOF marker、non-trivial size を確認する。これにより crate 内に redistributable fixtures がなくても sample-wide PDF export path を再現可能に保つ。

## Known Gaps

- Paragraphs は decoded JTD paragraph records ではなく extracted newlines から derive される。
- Style streams は raw named `UnknownStyle` data として保存されるが、style IDs は paragraphs や text runs にはまだ attach されていない。
- observed ruby base/phonetic annotation pair だけは structured `Inline::Ruby` model data として表現する。より広い ruby/style semantics はまだ decoded されていない。
- JTTC `JustCompressedDocument` support は observed single-member `-lh5-` profile に限定される。
- Embedded document text source boundaries は heuristic である。

## Next Steps

- `/DocumentText` の paragraph and inline record boundaries を decode する。
- preserved style streams を decode し、paragraphs と text runs に style references を attach する。
- 現在 observed している base/phonetic pair 以外の ruby/style controls を decode する。
- `.jttc` template/control-heavy `DocumentText` を blank paragraphs としてではなく model content として解釈する。
- embedded `/EmbeddedDocumentText` synthesis を proper object/stream ownership に置き換える。
- redistributable samples が利用可能になったら stable expected JSON fixtures を追加する。
