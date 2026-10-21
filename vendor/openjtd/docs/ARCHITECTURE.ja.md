# Architecture

OpenJTD は現在、`rjtd` Rust ツール群を通じて JTD エンジンを構築している。`rjtd` は
rhwp 風の階層化された文書エンジンアーキテクチャに従う。

```text
Document File
      │
      ▼
Container Layer
      │
      ▼
Stream Layer
      │
      ▼
Record Layer
      │
      ▼
Document Model
      │
      ├──── Markdown Export
      ├──── HTML Export
      ├──── JSON Export
      └──── Future Renderer
```

## Layer Rules

- Container code は logical streams を発見して開く。
- Stream code は byte-level の stream access を扱う。
- Record code は typed record と unknown record boundaries を decode する。
- Model code は semantic document structures を所有する。
- Export code は document model だけを consume する。

Exporter は raw container、stream、record data を直接読んではならない。

## Unknown Preservation

リバースエンジニアリングは段階的に進む。まだ理解されていない data は破棄せず、unknown model shape のいずれかとして次の層へ運ぶ。

- `UnknownRecord`
- `UnknownBlock`
- `UnknownStyle`
- `UnknownObject`
