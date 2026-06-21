# Canopy — agent handoff

Paste this to a fresh agent/session to pick up work on Canopy.

---

You are picking up work on **Canopy**, a **JavaScript-runtime-free, capability-based native
UI runtime**. You author a UI with the web mental model — a declarative tree, CSS-like
styling, components, signal reactivity — but there is **no JavaScript runtime**: the app
reaches the UI only through a typed, capability-based **op-stream** of unforgeable node
handles. The core is `no_std` + `alloc`, so the same UI runs from desktop GPU down to bare
metal (a software rasterizer). There are **two authoring front-ends over one op-stream** — a
Rust `rsx!` macro and a freestanding **C++ DSL** — and the lite path already renders a
CSS-styled GUI to pixels on bare-metal aarch64 with no OS.

## The repo

- **Clone:** `git clone https://github.com/eckeltdev/canopy.git`, then work from the repo root.
- Branch: `master`.
- First read: [`README.md`](README.md) — overview, architecture, crate map, build commands.

## Set up the skill (do this first)

There is a ready-made **project skill** at `.claude/skills/canopy/` that teaches you to use
the library on any target. Claude Code auto-discovers skills under `.claude/skills/`, so:

- **Working from the repo:** nothing to install — open/run Claude Code from the repo root and
  the `canopy` skill is available. It triggers automatically on Canopy-related tasks (or
  invoke it explicitly as `/canopy`).
- **Working from elsewhere:** copy it user-level so it's available everywhere —
  ```sh
  cp -r .claude/skills/canopy ~/.claude/skills/   # run from the repo root
  ```

The skill is a 5-part reference:
| File | Covers |
|---|---|
| `.claude/skills/canopy/SKILL.md` | Mental model, a **path-picker** (Rust desktop / Rust MCU / C++ bare-metal / any-language C ABI), minimal apps, the universal `render → framebuffer` step, build/verify rules |
| `reference/architecture.md` | Op-stream, the capability boundary, the two style tiers, the render path, the crate map |
| `reference/authoring.md` | The Rust `rsx!`/`Ui` API and the C++ DSL side by side |
| `reference/styling.md` | The lite CSS engine (summary; points to FEATURES.md) |
| `reference/hardware.md` | Deploying to a real framebuffer; the frt platform seam; **Path E = bring-your-own GPU renderer** |
| `reference/api.md` | C ABI, C++ `host` wrapper, Rust `Ui`, op-protocol quick reference |

## Where to get information (priority order)

1. **The skill** (`.claude/skills/canopy/SKILL.md` + `reference/*.md`) — start here; it routes you by task.
2. **[`README.md`](README.md)** — project overview, crate map, portability tiers, build & test.
3. **[`crates/canopy-style-css/FEATURES.md`](crates/canopy-style-css/FEATURES.md)** — the authoritative lite-CSS coverage map (selectors, properties, values, and honestly-deferred items).
4. **[`docs/ARCHITECTURE.md`](docs/ARCHITECTURE.md)** — how `rsx!` lowers to ops, how the capability boundary is enforced, how the rendering tiers stack up.
5. **The wire contracts (authoritative headers):**
   - op-stream (guest → host): `crates/canopy-abi/include/canopy_protocol.h`
   - C ABI functions: `crates/canopy-abi/include/canopy.h`
   - display-list / bring-your-own-renderer (host → consumer): `crates/canopy-abi/include/canopy_displaylist.h`
6. **Worked examples:**
   - Rust `rsx!` desktop: `examples/lite/welcome/src/lib.rs`
   - C++ DSL → CSS → pixels: `bindings/canopy_cpp/examples/gui_css/main.cpp`
   - `no_std` bare-metal build-proof: `examples/lite/embedded/`

## Critical build constraints (don't trip on these)

- The workspace pins the **stable** toolchain (`rust-toolchain.toml`); it needs **rustc ≥ 1.87**
  (the GPU crates' MSRV). CI uses the latest stable.
- **Never run `cargo update`** — the lockfile is pinned; a transitive crate needs an even newer rustc.
- A bare `cargo test --workspace` fails on an older pinned stable (wgpu/naga). Test specific
  crates instead — e.g. `cargo test -p canopy-style-css` — or use a stable ≥ 1.87.
- `canopy-style-stylo` (capable-tier Stylo) and `canopy-wpt` are **excluded** standalone crates
  with their own lockfiles (nightly/Stylo); build them from their own directories.
- **C++ binding:** `cargo build -p canopy-abi` (→ `target/debug/libcanopy_abi.a`), then CMake
  under `bindings/canopy_cpp/`. C++ style is gated by **cpp-doctor** (types are `snake_case`) —
  use the `cpp-doctor-style-guide` skill and run `cpp-doctor check` until clean.
- **no_std proof:** `rustup target add aarch64-unknown-none && cargo build -p canopy-core --target aarch64-unknown-none`.

## First moves

```sh
git clone https://github.com/eckeltdev/canopy.git && cd canopy
# 1. Read the overview + the skill entry point:
#    README.md  and  .claude/skills/canopy/SKILL.md
# 2. Sanity build/test (avoid a bare --workspace on old stable):
cargo build -p canopy-abi
cargo test  -p canopy-style-css -p canopy-abi
# 3. See it render — Rust desktop (writes a PPM):
( cd examples/lite/welcome && cargo run --bin render /tmp/welcome.ppm )
# 4. See it render — C++ DSL → CSS → pixels:
cargo build -p canopy-abi
cmake -S bindings/canopy_cpp -B bindings/canopy_cpp/build
cmake --build bindings/canopy_cpp/build --target canopy_cpp_css_example
( cd bindings/canopy_cpp/build && ./canopy_cpp_css_example )   # writes canopy_cpp_css.ppm
```

When in doubt, let the `canopy` skill drive — it has the exact APIs, commands, and seams for
each target (desktop, bare-metal aarch64 on `frt`, Cortex-M `no_std`, or any language over the
C ABI), plus the bring-your-own-GPU-renderer path.
