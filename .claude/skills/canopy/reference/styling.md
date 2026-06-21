# Styling with the lite CSS engine

The device/hardware path uses the LITE engine (`canopy-style-css`, `no_std`). It is a
small but **real** CSS engine — author the tree with identity (`class`/`id`/tag) and put
everything else in the stylesheet string handed to `set_stylesheet` (C++/C) or
`Ui::with_css` (Rust). The full, current coverage table is
**`crates/canopy-style-css/FEATURES.md`** — read it for the authoritative list. Summary:

## Selectors
- type / `#id` / `.class` / `*` / compound (`button.primary#go`)
- combinators: descendant (` `) and child (`>`)  *(host path; see note)*
- attributes: `[a]`, `[a="v"]`, `[a^=]`, `[a$=]`, `[a*=]`
- pseudo-classes: `:hover` `:focus` `:active` `:disabled` `:checked`; structural `:first-child`/`:last-child`/`:only-child`/`:empty`/`:nth-child(An+B)`/`:nth-last-child`; functional `:not()`/`:is()`/`:where()`
- specificity: id=100, class/attr/pseudo=10, type=1; ties by source order (`:where()`=0)

## Properties
- **box model:** `margin`/`padding`/`inset` (shorthand 1–4 values + per-side longhands), `margin:auto`, negatives, `box-sizing`, `min/max-width/height`, `aspect-ratio`, `width`/`height`
- **flex:** `direction`, `align`, `justify`, `align-self`, `flex`/`flex-grow`/`-shrink`/`-basis`/`-wrap`, `gap`/`row-gap`/`column-gap`
- **grid:** `display:grid`, `grid-template-columns/-rows` (`px`/`fr`/`%`/`auto`/`minmax`/`repeat`), `grid-column/-row` (`a/b`, `span n`), `grid-auto-flow`, `justify-items`
- **position / display:** `position` + `top/right/bottom/left` + `z-index`; `display:flex/grid/none`, `visibility:hidden`, `overflow:hidden/clip` (real clipping)
- **paint:** `background`, `background-image:linear-gradient(...)`, `box-shadow`, `color`, `opacity`, `radius` (+ per-corner), `border` shorthand / per-side / `border-style`, `outline-*`
- **text:** `font-size`, `font-weight`, `line-height`, `text-align`, `text-decoration`
- **transform:** `translate-x`/`translate-y`

## Values
- colors: named, `#rgb`/`#rgba`/`#rrggbb`/`#rrggbbaa`, `rgb()`/`rgba()`, `transparent` → normalized to `#rrggbb`/`#rrggbbaa`
- lengths: `px`, bare number, negatives, `%` (resolved by Taffy), and `rem`/`em`/`vw`/`vh` → px
- `calc()` / `min()` / `max()` / `clamp()` over absolute lengths
- **custom properties** `--token` + `var(--token, fallback)`, inherited
- `@media (min/max-width|height: …)` with `and`/comma
- real **inheritance** of `color`/font traits/`visibility`/custom properties

## Example (exercises the engine)

```css
#screen { --accent: #89b4fa; --text: #cdd6f4;
          background-image: linear-gradient(to bottom, #1e1e2e, #11111b);
          padding: 32; direction: column; align: center; justify: center }
#card   { width: 400; background: #313244; radius: 16; padding: 24; gap: 16;
          color: var(--text); box-shadow: 0 10 28 #00000088 }
.title  { font-size: 26; font-weight: bold; text-align: center }
.actions{ display: grid; grid-template-columns: repeat(2, 1fr); gap: 14 }
button  { height: 48; radius: 10; color: #11111b; border-color: rgba(0,0,0,0.25) }
button.primary       { background: var(--accent) }
button.primary:hover { background: #b4caff }
@media (max-width: 360px) { .actions { grid-template-columns: 1fr } }
```

## Two things to know

1. **Non-destructive cascade.** The host clones the tree, folds matched declarations in as
   inline styles (author inline wins), and renders that — the retained tree and
   `debug_snapshot` stay byte-stable. Parity tests depend on this; don't expect the
   authored tree to show resolved styles.
2. **In-process vs host path.** Combinators and structural pseudo-classes need the
   retained tree, so they resolve on the **host** path (C ABI / `canopy-abi`). The
   in-process Rust `canopy-ui` path retains no tree edges, so it supports the own-identity
   subset (its methods document this). The C++/device path goes through the host → full set.

## Deferred (in FEATURES.md, with rationale)
transitions/`@keyframes`, transforms beyond translate, `url()` images, the C-ABI extern
surface for `:focus`/`:active` (Rust host API works), sibling combinators, pseudo-elements,
full `!important`/`inherit` semantics. Each is small-to-medium and isolated.
