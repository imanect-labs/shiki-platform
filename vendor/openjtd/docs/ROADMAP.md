# OpenJTD Roadmap

This roadmap tracks the path from the current `rjtd` Rust toolset toward the
OpenJTD editor and engine.

## M1: Container Explorer

Status: implemented.

Command:

```text
rjtd streams sample.jtd
```

Goals:

- Analyze CFB container structure.
- List streams in a JTD sample.
- Use a rhwp-style lenient fallback for malformed FAT CFB files.

## M2: Text Extraction

Status: implemented as an initial heuristic for observed `.jtd`, `.jtt`, and `.jttc` samples.

Command:

```text
rjtd cat sample.jtd
```

Goals:

- Extract text from JTD/JTT `/DocumentText`.
- Open observed JTTC `/JSCompDocument` `JustCompressedDocument` payloads and read inner `/DocumentText`.
- Recover observed embedded `SsmgV.01`/`TextV.01` fragments when no named `/DocumentText` stream exists.

## M3: Document Model

Status: minimal model export implemented.

Command:

```text
rjtd export sample.jtd --format json
```

Goals:

- Build `Paragraph`.
- Build `TextRun`.
- Build `Style`.

## M4: Markdown Export

Status: minimal model-based export implemented.

Command:

```text
rjtd export sample.jtd --format md
```

Goal:

- Export the document model to Markdown.

## M5: Public Specification

Repository:

```text
openjtd-spec
```

Goal:

- Record reverse engineering results as RFC-style specification documents.
