# API reference

Authoritative headers (read these for exact contracts):
- `crates/canopy-abi/include/canopy.h` — the C ABI (functions).
- `crates/canopy-abi/include/canopy_protocol.h` — the op-stream wire format (ops, ids).
- `bindings/canopy_cpp/include/canopy_cpp/host.hpp` + `dsl.hpp` — the C++ wrapper + DSL.
- `crates/canopy-ui/src/lib.rs` — the Rust `Ui` surface (see `authoring.md`).

## The C ABI (`canopy.h`)

Opaque `CanopyHost*`; create once, free once. Op bytes are borrowed only for the call.
Return codes are `CANOPY_OK` (0) or negative `CANOPY_ERR_*` (`NULL_HOST`, `NULL_DATA`,
`TOO_LARGE`, `DECODE`).

```c
CanopyHost *canopy_host_new(void);
void        canopy_host_free(CanopyHost *host);                                  // exactly once

int32_t canopy_host_apply(CanopyHost *host, const uint8_t *ptr, size_t len);     // apply an op batch (<= 1 MiB)
size_t  canopy_host_node_count(const CanopyHost *host);                          // live nodes (excl. root)

int32_t canopy_host_set_stylesheet(CanopyHost *host, const uint8_t *css, size_t len); // install lite CSS (UTF-8)
int32_t canopy_host_resize(CanopyHost *host, float width, float height);              // viewport for hit-test

// Render to RGBA8. Two-call sizing: pass cap=0/out=NULL to get *out_len = w*h*4, then call again.
int32_t canopy_host_render_rgba(const CanopyHost *host, uint32_t width, uint32_t height,
                                uint8_t *out, size_t cap, size_t *out_len);       // dims capped at 8192

int32_t canopy_host_hover(CanopyHost *host, float x, float y);                    // update :hover node
int32_t canopy_host_pointer(CanopyHost *host, float x, float y, uint8_t button, uint16_t event); // hit-test + queue event
int32_t canopy_host_poll_events(CanopyHost *host, uint8_t *out, size_t cap, size_t *out_len);     // drain DispatchEvent batch

// Debug oracle: pre-order DFS text dump of the retained tree (same two-call sizing as render).
int32_t canopy_host_debug_snapshot(const CanopyHost *host, uint8_t *out, size_t cap, size_t *out_len);
```

Note: there is **no** `canopy_host_set_focus`/`set_active` in the C ABI yet — `:focus`/
`:active` are driven from the Rust host API (`CanopyHost::set_focus`/`set_active`) only;
the extern surface is a documented follow-up.

## C++ `host` (RAII over the above; `host.hpp`)

`host()` · `apply(batch)` · `resize(w,h)` · `set_stylesheet(css)` · `render_rgba(w,h)->vector<uint8_t>`
· `hover(x,y)->bool` (changed?) · `pointer(x,y,button,event)->int` (queued?) · `pump(ctx)->int`
(events fired into the parked closures) · `node_count()->size_t`. Move-only; frees via the C
ABI. Link `libcanopy_abi.a`.

## Op-stream wire format (`canopy_protocol.h`)

Frame: `BeginBatch(version, seq) op* EndBatch`, little-endian, strings interned once.

| Op | Tag | Fields |
|---|---|---|
| BeginBatch / EndBatch | 0x01 / 0x02 | version,seq / — |
| CreateElement | 0x10 | node, tag |
| CreateText | 0x11 | node, str |
| RemoveNode | 0x12 | node |
| InsertBefore | 0x13 | parent, node, anchor |
| SetText | 0x14 | node, str |
| SetAttribute | 0x15 | node, attr, str |
| SetInlineStyle | 0x16 | node, prop, str |
| SetClass / RemoveClass | 0x17 / 0x18 | node, str |
| AddListener / RemoveListener | 0x19 / 0x1A | node, event, handler |
| InternString | 0x1B | str, bytes |
| SetTagName | 0x1C | node, str |
| DispatchEvent (host→guest) | 0x80 | handler(u32), node(u64), payload |

Handle widths: `NodeId` u64, `HandlerId` u32, `StrId` u32, `ElementTag`/`PropId`/`EventKind`/`AttrId` u16.
Reserved: `NODE_ROOT=0`, `NODE_NULL=0xFFFF…FF` (InsertBefore append), `ATTR_ID=1` (the CSS id).
Well-known host ids: `EL_COLUMN=1`, `EL_ROW=2`, `EL_BUTTON=3`, `EL_INPUT=4`; `EVENT_CLICK=1`;
PropIds `1..75` (BG, FG, … through the grid props — the full list is in `canopy-paint` and
mirrored in the header).

**Stateful emitter rules (any encoder MUST follow):** `NodeId` author-minted, monotonic
from 1; `HandlerId` author-minted, monotonic from 0; intern each unique string once and
reuse its `StrId`; the intern table + node counter persist across batches. You rarely write
this by hand — `canopy-core`'s `Emitter`, `Ui`, and the C++ DSL all do it for you.

## Keeping the contract intact

If you add or change a PropId / op tag / widget id, mirror it across **all four** sites and
run the parity test:
- `crates/canopy-paint/src/lib.rs` (PropId source of truth)
- `crates/canopy-abi/include/canopy_protocol.h`
- `bindings/canopy_cpp/include/canopy_cpp/protocol.hpp`
- `crates/canopy-abi/tests/protocol_header.rs` → `cargo test -p canopy-abi --test protocol_header`
