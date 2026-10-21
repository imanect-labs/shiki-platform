# Architecture

OpenJTD currently builds its JTD engine through the `rjtd` Rust toolset. `rjtd`
follows the rhwp-style layered document engine architecture.

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

- Container code discovers and opens logical streams.
- Stream code handles byte-level stream access.
- Record code decodes typed and unknown record boundaries.
- Model code owns semantic document structures.
- Export code consumes only the document model.

Exporters must not read raw container, stream, or record data directly.

## Unknown Preservation

Reverse engineering is incremental. Any data that is not understood yet must be carried forward as one of the unknown model shapes instead of being discarded.

- `UnknownRecord`
- `UnknownBlock`
- `UnknownStyle`
- `UnknownObject`
