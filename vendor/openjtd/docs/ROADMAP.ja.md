# OpenJTD Roadmap

この roadmap は、現在の `rjtd` Rust ツール群から OpenJTD editor と engine へ進む道筋を
管理する。

## M1: Container Explorer

Status: implemented.

Command:

```text
rjtd streams sample.jtd
```

Goals:

- CFB container structure を解析する。
- JTD sample 内の streams を列挙する。
- malformed FAT CFB files には rhwp 風の lenient fallback を使う。

## M2: Text Extraction

Status: 観察済み `.jtd`、`.jtt`、`.jttc` samples に対する初期 heuristic として実装済み。

Command:

```text
rjtd cat sample.jtd
```

Goals:

- JTD/JTT `/DocumentText` から text を抽出する。
- 観察済み JTTC `/JSCompDocument` `JustCompressedDocument` payloads を開き、inner `/DocumentText` を読む。
- named `/DocumentText` stream がない場合、観察済み embedded `SsmgV.01`/`TextV.01` fragments を復元する。

## M3: Document Model

Status: minimal model export implemented.

Command:

```text
rjtd export sample.jtd --format json
```

Goals:

- `Paragraph` を構築する。
- `TextRun` を構築する。
- `Style` を構築する。

## M4: Markdown Export

Status: minimal model-based export implemented.

Command:

```text
rjtd export sample.jtd --format md
```

Goal:

- document model を Markdown へ export する。

## M5: Public Specification

Repository:

```text
openjtd-spec
```

Goal:

- reverse engineering results を RFC-style specification documents として記録する。
