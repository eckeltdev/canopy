# Canopy lite-CSS — feature coverage

The lite tier (`canopy-style-css` + `canopy-layout-taffy` + `canopy-render-soft`, all
`#![no_std] + alloc`, driven by the host cascade in `canopy-abi` and the in-process
`canopy-ui`) is a small but real CSS engine for the freestanding/embedded path. This is
the authoritative map of what it does and does not support. The crate-level rustdoc on
`lib.rs` is the detailed reference; this file is the at-a-glance summary.

## Selectors

| Feature | Status |
|---|---|
| type / id / class / `*` universal | ✅ |
| compound (`button.primary#go`) | ✅ |
| descendant (` `) and child (`>`) combinators | ✅ host path¹ |
| attribute `[a]` `[a="v"]` `[a^=]` `[a$=]` `[a*=]` | ✅ |
| `:hover` / `:focus` / `:active` | ✅ (state plumbing in the host²) |
| `:disabled` / `:checked` | ✅ (attribute-presence) |
| `:first-child` `:last-child` `:only-child` `:empty` `:nth-child(An+B)` `:nth-last-child` | ✅ host path¹ |
| `:not()` / `:is()` / `:where()` (single-compound args, correct specificity incl. `:where`=0) | ✅ |
| specificity ordering (id=100, class/attr/pseudo=10, type=1; source-order ties) | ✅ |
| sibling combinators `+` `~`, `:nth-of-type`, `:focus-within`, `::before/::after`, attribute `~=`/`\|=` | ❌ deferred |

¹ Combinators + structural pseudos need the retained tree; they resolve on the host
(`canopy-abi`) path. The in-process `canopy-ui` path retains no tree edges, so it
supports the own-identity subset (its methods document this).
² `:hover`/`:focus`/`:active` use a host state set (`set_hover`/`set_focus`/`set_active`).
The C-ABI extern surface to drive focus/active from a non-Rust host is **not yet wired**
(Rust host API only) — see Deferred.

## Properties

| Group | Supported |
|---|---|
| box model | `margin`/`padding`/`inset` shorthands **and** per-side longhands, `margin: auto`, negative margins, `box-sizing`, `min/max-width/height`, `aspect-ratio` |
| flex | `direction`, `align`, `justify`, `align-self`, `flex` shorthand, `flex-grow/-shrink/-basis/-wrap`, `gap`/`row-gap`/`column-gap` |
| grid | `display: grid`, `grid-template-columns/-rows` (`px`/`fr`/`%`/`auto`/`minmax`/`repeat`), `grid-column/-row` (`a/b`, `span n`, line), `grid-auto-flow`, `justify-items` |
| position | `position: static/relative/absolute`, `top/right/bottom/left` (inset), `z-index` |
| display | `display: flex/grid/none`, `visibility: hidden`, `overflow: hidden/clip` (real clipping) |
| paint | `background`, `background-image: linear-gradient(...)`, `box-shadow`, `color`, `opacity`, `radius`/per-corner radius, `border` shorthand + per-side widths/colors + `border-style`, `outline-width/-color/-offset` |
| text | `font-size`, `font-weight`, `line-height`, `text-align`, `text-decoration` (underline/line-through) |
| transform | `translate-x`/`translate-y` only |

## Values

| Feature | Status |
|---|---|
| colors: named, `#rgb`/`#rgba`/`#rrggbb`/`#rrggbbaa`, `rgb()`/`rgba()`, `transparent` | ✅ → normalized to `#rrggbb`/`#rrggbbaa` |
| lengths: `px`, bare number, negative, `%` (passed to Taffy) | ✅ |
| relative units: `rem`, `em`, `vw`, `vh` → px | ✅ |
| `calc()` / `min()` / `max()` / `clamp()` over absolute lengths | ✅ |
| custom properties `--x` + `var(--x, fallback)` (inherited) | ✅ host path¹ |
| `!important` | ⚠️ stripped (does not break the decl); precedence not yet honored |
| `inherit` / `initial` / `unset` | ⚠️ recognized; drop the declaration (no full semantics) |
| `%` inside `calc()`, `hsl()`, `ch`/`fr`-as-length | ❌ deferred |

## Cascade / rendering

- Real **inheritance** of `color`/font traits/`visibility` and custom properties (host).
- Non-destructive cascade: matched rules fold in as inline styles; the retained tree is
  unchanged (parity-stable), author inline wins over selectors.
- **`@media`** width/height queries (OR of ANDs of `min/max-width/height`).
- Anti-aliased rounded corners / borders, real linear-gradient ramps, soft box-shadows,
  `overflow` clipping — all on the no_std software rasterizer.

## Deferred (with rationale)

| Item | Why deferred |
|---|---|
| **transitions / animations / `@keyframes`** | needs a frame clock + time-driven restyle loop; the runtime already offers signal-driven animation via `Ui::bind_style`, so this overlaps existing facilities. |
| **transforms beyond translate** (rotate/scale/skew) | needs an affine rasterizer (the software fill is axis-aligned); medium value for a native tier. |
| **`background-image: url(...)` / `<img>`** | needs a vendor-neutral image-source type + a decoder; a large, self-contained subsystem. |
| **radial/conic gradients, per-stop positions, multi-shadow, inset shadow, dashed/dotted borders** | rendering polish on top of the shipped primitives. |
| **C-ABI focus/active extern surface** | `:focus`/`:active` work via the Rust host API; the extern fns + `canopy.h` + C++ wrapper to drive them from a non-Rust host are a follow-up. |
| **sibling combinators, `:nth-of-type`, pseudo-elements, container queries** | lower-frequency selector features. |
| **full `!important` / `inherit` / `initial` semantics** | recognized but not fully cascaded. |

These are tracked as honest follow-ups, not silent gaps — each is small-to-medium and
isolated from the shipped engine.
