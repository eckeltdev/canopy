//! Canopy software renderer: a CPU rasterizer that paints a [`DisplayList`] into an
//! RGBA8888 buffer.
//!
//! It implements the same [`canopy_traits::Renderer`] trait the GPU backend will,
//! so the rest of the host is renderer-agnostic. Bringing it up first has two
//! payoffs: it validates the `Renderer` seam without a GPU or a window (so the whole
//! pipeline stays unit-testable), and it *is* the Tier-2 / bare-metal renderer — the
//! same path that later swaps in `tiny-skia` / `vello_cpu` for antialiased quality.
//!
//! It fills rectangles with **coverage-based antialiasing** — fractional edges and
//! **true rounded corners** when a [`DisplayItem::Rect`] carries a positive `radius`
//! (see [`Buffer::fill_round_rect`]) — strokes antialiased [`DisplayItem::Border`]
//! frames, fills real interpolated [`DisplayItem::Gradient`] boxes
//! ([`Buffer::fill_gradient`]), feathers soft outset [`DisplayItem::Shadow`] drops
//! ([`Buffer::fill_shadow`]), and blits a baked 8x8 bitmap font ([`canopy_text_baked`])
//! for [`DisplayItem::Text`] runs. All antialiasing is computed in pure integer/`core`
//! float math (a Newton-iteration [`isqrt_f32`], no `f32::sqrt`), keeping the crate
//! `no_std`. Shaped [`DisplayItem::Glyphs`] runs (the capable-tier path) are not yet
//! rasterized here.
#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::vec;
use alloc::vec::Vec;

use canopy_text_baked::{glyph, CELL_H, CELL_W};
use canopy_traits::{
    Color, DisplayItem, DisplayList, GradientDirection, GradientStop, GradientStops, HostError,
    Point, Rect, Renderer, Size,
};

/// Composite `src` (straight-alpha RGBA) **over** the destination pixel `dst`
/// (`[r, g, b, a]`) in place, the classic Porter–Duff "source over".
///
/// Each color channel becomes `src·a + dst·(1 − a)` with `a = src.a/255`, evaluated
/// in pure integer math as `(src·a + dst·(255 − a) + 127) / 255` — the `+127` gives
/// round-to-nearest so a half-alpha blend lands on the true midpoint instead of
/// drifting dark. This is the same rounding the text compositor uses, kept here so a
/// reduced-alpha fill fades over the background rather than overwriting it. No float,
/// no `f32::round`, no `unsafe` — `no_std`-clean.
///
/// The destination alpha is composited the same way against a notionally opaque
/// source coverage (`255`), so stacking translucent fills keeps the buffer's alpha
/// channel sensible (it trends toward opaque), which matters when the surface is
/// later alpha-composited onto another (e.g. a plugin panel into the frame).
fn blend_over(dst: &mut [u8], src: Color) {
    let a = u32::from(src.a);
    let inv = 255 - a;
    let mix = |s: u8, d: u8| -> u8 { ((u32::from(s) * a + u32::from(d) * inv + 127) / 255) as u8 };
    dst[0] = mix(src.r, dst[0]);
    dst[1] = mix(src.g, dst[1]);
    dst[2] = mix(src.b, dst[2]);
    // Resulting coverage: a + dst·(1 − a), i.e. `mix(255, dst.a)`.
    dst[3] = ((255 * a + u32::from(dst[3]) * inv + 127) / 255) as u8;
}

/// Composite `src` over `dst` with an extra **coverage** factor `cov ∈ [0, 255]`
/// that scales the source's own alpha — the antialiasing primitive.
///
/// A boundary pixel only partially covered by a shape passes its fractional coverage
/// here (e.g. an edge at `x = 10.4` gives pixel 10 a coverage of `0.4·255 ≈ 102`); the
/// effective source alpha becomes `src.a · cov / 255`, so the fill fades in over the
/// background along the edge instead of snapping on. `cov == 255` reduces exactly to
/// [`blend_over`] (and, for an opaque `src`, to a straight overwrite), so fully-covered
/// interior pixels are byte-for-byte unchanged from the non-AA path. Pure integer math,
/// round-to-nearest, `no_std`-clean.
fn blend_cov(dst: &mut [u8], src: Color, cov: u8) {
    if cov == 0 {
        return;
    }
    // Effective alpha = src.a · cov / 255 (round-to-nearest), then a normal src-over.
    let eff_a = ((u32::from(src.a) * u32::from(cov) + 127) / 255) as u8;
    blend_over(
        dst,
        Color {
            r: src.r,
            g: src.g,
            b: src.b,
            a: eff_a,
        },
    );
}

/// Straight-alpha per-channel linear interpolation between colors `a` and `b` at
/// `t ∈ [0, 1]` (`t = 0` → `a`, `t = 1` → `b`), round-to-nearest, `no_std`-clean.
/// Each channel (including alpha) is mixed independently: `a + (b − a)·t`.
fn lerp_color(a: Color, b: Color, t: f32) -> Color {
    let t = t.clamp(0.0, 1.0);
    let mix = |lo: u8, hi: u8| -> u8 {
        let lo = lo as f32;
        let hi = hi as f32;
        (lo + (hi - lo) * t + 0.5) as u8
    };
    Color {
        r: mix(a.r, b.r),
        g: mix(a.g, b.g),
        b: mix(a.b, b.b),
        a: mix(a.a, b.a),
    }
}

/// The interpolated gradient color at normalized axis position `t ∈ [0, 1]` for the
/// stop list `stops`, which must be non-empty. Finds the bracketing pair by their
/// `.position` and lerps between them ([`lerp_color`]); `t` before the first stop
/// clamps to that stop's color, after the last clamps to the last's (CSS "hard" ends).
/// Stops are assumed in non-decreasing `position` order, as the producer emits them.
fn gradient_color_at(stops: &[GradientStop], t: f32) -> Color {
    // Empty handled by the caller; defensively return transparent.
    let Some(first) = stops.first() else {
        return Color::default();
    };
    if t <= first.position {
        return first.color;
    }
    let last = stops[stops.len() - 1];
    if t >= last.position {
        return last.color;
    }
    // Find the segment [lo, hi] with lo.position <= t < hi.position.
    let mut i = 1;
    while i < stops.len() {
        let hi = stops[i];
        if t < hi.position {
            let lo = stops[i - 1];
            let span = hi.position - lo.position;
            // Coincident stops (a hard color stop): take the upper color.
            let local = if span > 0.0 {
                (t - lo.position) / span
            } else {
                1.0
            };
            return lerp_color(lo.color, hi.color, local);
        }
        i += 1;
    }
    last.color
}

/// Ceiling of a non-negative `f32` as a `usize`, `no_std`-clean (no `f32::ceil`, a
/// `std`-only intrinsic). Negatives saturate to `0`. Used to find the last pixel a
/// fractional rect edge touches, so a rect ending at `x = 10.4` still paints pixel 10.
fn ceil_to_usize(v: f32) -> usize {
    if v <= 0.0 {
        return 0;
    }
    let t = v as usize; // truncates toward zero
    if v > t as f32 {
        t + 1
    } else {
        t
    }
}

/// Quantize a coverage fraction in `[0, 1]` to a `[0, 255]` alpha scale,
/// round-to-nearest.
fn cov_to_u8(cov: f32) -> u8 {
    (cov.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
}

/// Non-negative square root of `v`, computed without `f32::sqrt` (a `std`-only
/// intrinsic) so the crate stays `no_std`. Seeds with the integer square root of the
/// truncated value (`u32::isqrt`, a `core` const fn) and refines with a few Newton
/// steps `g ← (g + v/g) / 2`, which converges to well under a pixel of error for the
/// modest distances the shadow ramp feeds it. `v <= 0` returns `0`.
fn isqrt_f32(v: f32) -> f32 {
    if v <= 0.0 {
        return 0.0;
    }
    // Integer-sqrt seed (good to ±1), then Newton-refine for the fractional part.
    let mut g = (v as u32).isqrt() as f32;
    if g <= 0.0 {
        g = v.min(1.0); // v in (0, 1): seed below the true root so Newton climbs up
    }
    // Four Newton iterations are ample for distances up to a few hundred px.
    let mut i = 0;
    while i < 4 {
        g = 0.5 * (g + v / g);
        i += 1;
    }
    g
}

/// Fractional coverage of pixel column/row `i` (whose span is `[i, i+1)`) by the half
/// line `[lo, hi)`, in `[0, 1]`. A pixel fully inside returns `1.0`; one the edge cuts
/// returns the covered fraction (so an edge at `10.4` leaves pixel 10 `0.6` covered on
/// its right side / `0.4` on the carved side, depending on which bound moves); one fully
/// outside returns `0.0`. With **integer** `lo`/`hi` every pixel is `0.0` or `1.0`, so
/// integer-aligned rects rasterize exactly as before — the AA only bites fractional edges.
fn axis_coverage(i: usize, lo: f32, hi: f32) -> f32 {
    let a = (i as f32).max(lo);
    let b = ((i + 1) as f32).min(hi);
    (b - a).clamp(0.0, 1.0)
}

/// Coverage of pixel `(x, y)` by the rounded rect `rect`/`r`, estimated by 4×4
/// supersampling: how many of 16 evenly spaced sub-sample points fall inside, as a
/// fraction in `[0, 1]`. Used only on boundary (corner-band) pixels — interior pixels
/// take the fast full-coverage path. Each sub-sample uses the same squared-distance
/// test as [`point_in_round_rect`], so no `sqrt` (a `std`-only intrinsic) is needed —
/// keeping this `no_std`.
fn round_rect_coverage(x: usize, y: usize, rect: Rect, r: f32) -> u8 {
    let mut hits = 0u32;
    // Sub-sample centers at 1/8, 3/8, 5/8, 7/8 of the pixel on each axis.
    let mut sy = 0;
    while sy < 4 {
        let py = y as f32 + (sy as f32 * 2.0 + 1.0) / 8.0;
        let mut sx = 0;
        while sx < 4 {
            let px = x as f32 + (sx as f32 * 2.0 + 1.0) / 8.0;
            if point_in_round_rect(px, py, rect, r) {
                hits += 1;
            }
            sx += 1;
        }
        sy += 1;
    }
    // 16 hits => 255 (fully opaque); scale linearly otherwise.
    ((hits * 255 + 8) / 16) as u8
}

/// Whether pixel-center `(cx, cy)` is in the stroke ring: inside the outer rounded rect
/// (`outer`/`outer_r`) yet outside the inner one (`inner`/`inner_r`).
fn point_in_ring(cx: f32, cy: f32, outer: Rect, outer_r: f32, inner: Rect, inner_r: f32) -> bool {
    point_in_round_rect(cx, cy, outer, outer_r) && !point_in_round_rect(cx, cy, inner, inner_r)
}

/// Coverage of pixel `(x, y)` by the stroke ring, by 4×4 supersampling, as `[0, 255]`.
/// Each sub-sample tests [`point_in_ring`], so the antialiasing covers **both** the
/// outer and the inner boundary (and the rounded corners of each) with no `sqrt`.
#[allow(clippy::too_many_arguments)]
fn ring_coverage(x: usize, y: usize, outer: Rect, outer_r: f32, inner: Rect, inner_r: f32) -> u8 {
    let mut hits = 0u32;
    let mut sy = 0;
    while sy < 4 {
        let py = y as f32 + (sy as f32 * 2.0 + 1.0) / 8.0;
        let mut sx = 0;
        while sx < 4 {
            let px = x as f32 + (sx as f32 * 2.0 + 1.0) / 8.0;
            if point_in_ring(px, py, outer, outer_r, inner, inner_r) {
                hits += 1;
            }
            sx += 1;
        }
        sy += 1;
    }
    ((hits * 255 + 8) / 16) as u8
}

/// Whether point `(px, py)` (pixel-center, absolute logical coords) lies inside the
/// rounded rect `rect` with corner radius `r`. The straight edges and interior are
/// inside; only the four corner quarter-circles carve pixels away. Used by the rounded
/// fill and stroke. `r` is clamped to half the shorter side; `r <= 0` is a plain rect.
fn point_in_round_rect(px: f32, py: f32, rect: Rect, r: f32) -> bool {
    if px < rect.origin.x
        || px >= rect.origin.x + rect.size.w
        || py < rect.origin.y
        || py >= rect.origin.y + rect.size.h
    {
        return false;
    }
    let r = r.min(0.5 * rect.size.w.min(rect.size.h)).max(0.0);
    if r <= 0.0 {
        return true;
    }
    let left = rect.origin.x + r;
    let right = rect.origin.x + rect.size.w - r;
    let top = rect.origin.y + r;
    let bottom = rect.origin.y + rect.size.h - r;
    // Nearest arc center per axis; an unclamped axis contributes 0 distance, so straight
    // edge strips stay inside and only true corners apply the quarter-circle cutoff.
    let cx = px.clamp(left, right);
    let cy = py.clamp(top, bottom);
    let dx = px - cx;
    let dy = py - cy;
    dx * dx + dy * dy <= r * r
}

/// An RGBA8888 pixel buffer, row-major, 4 bytes per pixel.
pub struct Buffer {
    width: usize,
    height: usize,
    data: Vec<u8>,
}

impl Buffer {
    /// Allocate a transparent-black buffer.
    pub fn new(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            data: vec![0; width * height * 4],
        }
    }

    /// Width in pixels.
    pub fn width(&self) -> usize {
        self.width
    }

    /// Height in pixels.
    pub fn height(&self) -> usize {
        self.height
    }

    /// The raw RGBA bytes.
    pub fn data(&self) -> &[u8] {
        &self.data
    }

    /// Read one pixel as `[r, g, b, a]`. Out-of-bounds reads return zeros.
    pub fn pixel(&self, x: usize, y: usize) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        let i = (y * self.width + x) * 4;
        [
            self.data[i],
            self.data[i + 1],
            self.data[i + 2],
            self.data[i + 3],
        ]
    }

    /// Overwrite one pixel with `[r, g, b, a]` (a **straight store**, no blending),
    /// clipped to the buffer.
    ///
    /// This is the write counterpart to [`pixel`](Buffer::pixel): a caller that has
    /// already done its own compositing (it read the destination, blended, and holds
    /// the final value) writes the result back here so [`fill_rect`](Buffer::fill_rect)'s
    /// alpha blend does not double-apply. The text compositor in `canopy-render-text`
    /// uses exactly this pairing.
    pub fn set_pixel(&mut self, x: usize, y: usize, rgba: [u8; 4]) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = (y * self.width + x) * 4;
        self.data[i..i + 4].copy_from_slice(&rgba);
    }

    /// Fill the whole buffer with one color.
    pub fn clear(&mut self, c: Color) {
        for px in self.data.chunks_exact_mut(4) {
            px.copy_from_slice(&[c.r, c.g, c.b, c.a]);
        }
    }

    /// Fill `rect` with `c`, **src-over alpha-blended** over whatever is already in
    /// the buffer, clipped to the buffer, with **antialiased edges**. `f32 as usize`
    /// saturates negatives to 0, so off-screen origins clip cleanly.
    ///
    /// Edges no longer truncate straight to integer pixels: an edge at `x = 10.4`
    /// partially covers pixel 10. Each pixel's coverage is the fraction of its
    /// `1×1` cell that lies inside the rect (the product of its horizontal and vertical
    /// [`axis_coverage`]), and the fill is composited at that coverage (see
    /// [`blend_cov`]). Fully-inside pixels have coverage `1.0`, so for an opaque `c`
    /// they remain a byte-exact overwrite and for a translucent `c` a plain src-over —
    /// exactly the pre-AA behavior. Fully-outside pixels (coverage `0`) are untouched.
    /// Only the boundary ring pays the fractional blend. An **integer-aligned** rect has
    /// no fractional edges, so it rasterizes identically to the old truncating fill.
    pub fn fill_rect(&mut self, rect: Rect, c: Color) {
        let lx = rect.origin.x;
        let ty = rect.origin.y;
        let rx = rect.origin.x + rect.size.w;
        let by = rect.origin.y + rect.size.h;
        if rx <= lx || by <= ty {
            return;
        }
        // Pixel span: include any pixel the rect touches at all (floor of the low edge,
        // ceil of the high edge), saturating off-screen, so fractional edge pixels are
        // visited and given partial coverage.
        let x0 = (lx.max(0.0) as usize).min(self.width);
        let y0 = (ty.max(0.0) as usize).min(self.height);
        let x1 = (ceil_to_usize(rx)).min(self.width);
        let y1 = (ceil_to_usize(by)).min(self.height);
        let opaque = c.a == 255;
        for y in y0..y1 {
            let cov_y = axis_coverage(y, ty, by);
            if cov_y <= 0.0 {
                continue;
            }
            for x in x0..x1 {
                let cov = cov_y * axis_coverage(x, lx, rx);
                if cov <= 0.0 {
                    continue;
                }
                let i = (y * self.width + x) * 4;
                let px = &mut self.data[i..i + 4];
                if cov >= 1.0 {
                    // Fully covered: the original fast path, byte-for-byte.
                    if opaque {
                        px.copy_from_slice(&[c.r, c.g, c.b, c.a]);
                    } else {
                        blend_over(px, c);
                    }
                } else {
                    // Boundary pixel: composite at fractional coverage.
                    blend_cov(px, c, cov_to_u8(cov));
                }
            }
        }
    }

    /// Fill `rect` with `c`, **rounding the four corners** to `radius` logical px,
    /// **antialiased**, clipped to the buffer.
    ///
    /// Straight edges and the interior fill via the same fractional-edge coverage as
    /// [`fill_rect`](Buffer::fill_rect) (the product of the horizontal and vertical
    /// [`axis_coverage`]). The four corners are smoothed by **4×4 supersampling** of
    /// each boundary pixel ([`round_rect_coverage`]): a pixel straddling a corner arc
    /// gets the fraction of its 16 sub-samples that fall inside the rounded shape, so
    /// the arc reads as a smooth curve rather than a staircase. Each sub-sample uses a
    /// squared-distance test, so there is **no `f32::sqrt`** (a `std`-only intrinsic) —
    /// keeping this `no_std`.
    ///
    /// Only corner-band pixels pay the supersample; a pixel fully inside a corner's
    /// quarter-circle (all 16 sub-samples inside) and every straight-region pixel take
    /// the cheap full-coverage path, so the interior is byte-for-byte the non-AA fill.
    ///
    /// `radius` is clamped to half the rect's shorter side, so an oversized radius
    /// produces a pill/stadium (or a circle for a square rect) rather than overflowing.
    /// A non-positive radius falls through to a square [`fill_rect`](Buffer::fill_rect),
    /// so callers can pass the display item's radius unconditionally.
    ///
    /// Like [`fill_rect`](Buffer::fill_rect), the body is **src-over alpha-blended**:
    /// an opaque `c` overwrites fully-covered pixels while a translucent `c` blends, so
    /// a faded rounded card composites correctly. Carved-away corner pixels (coverage 0)
    /// keep whatever was behind them.
    pub fn fill_round_rect(&mut self, rect: Rect, c: Color, radius: f32) {
        // Clamp to half the shorter side; a non-positive radius is just a square.
        let max_r = 0.5 * rect.size.w.min(rect.size.h);
        let r = radius.min(max_r);
        if r <= 0.0 {
            self.fill_rect(rect, c);
            return;
        }
        let opaque = c.a == 255;

        let lx = rect.origin.x;
        let ty = rect.origin.y;
        let rx = rect.origin.x + rect.size.w;
        let by = rect.origin.y + rect.size.h;

        // Pixel-space bounds, floor/ceil so fractional edges are visited (saturating
        // off-screen). Matches [`fill_rect`]'s AA span.
        let x0 = (lx.max(0.0) as usize).min(self.width);
        let y0 = (ty.max(0.0) as usize).min(self.height);
        let x1 = ceil_to_usize(rx).min(self.width);
        let y1 = ceil_to_usize(by).min(self.height);

        // Corner bands, in absolute logical coords: a pixel whose cell can touch an arc
        // lies where the center is within `r` of an edge. We supersample any pixel whose
        // span `[i, i+1]` overlaps `[edge, edge±r]`.
        let left = lx + r;
        let right = rx - r;
        let top = ty + r;
        let bottom = by - r;

        for y in y0..y1 {
            let cov_y = axis_coverage(y, ty, by);
            if cov_y <= 0.0 {
                continue;
            }
            // Is this row inside a corner band (its cell can reach a top/bottom arc)?
            let row_corner = (y as f32) < top || ((y + 1) as f32) > bottom;
            for x in x0..x1 {
                let col_corner = (x as f32) < left || ((x + 1) as f32) > right;
                let cov = if row_corner && col_corner {
                    // True corner pixel: supersample the rounded boundary.
                    round_rect_coverage(x, y, rect, r)
                } else {
                    // Straight edge / interior: rectangular fractional coverage.
                    cov_to_u8(cov_y * axis_coverage(x, lx, rx))
                };
                if cov == 0 {
                    continue;
                }
                let i = (y * self.width + x) * 4;
                let px = &mut self.data[i..i + 4];
                if cov == 255 {
                    if opaque {
                        px.copy_from_slice(&[c.r, c.g, c.b, c.a]);
                    } else {
                        blend_over(px, c);
                    }
                } else {
                    blend_cov(px, c, cov);
                }
            }
        }
    }

    /// Stroke a `width`-thick frame **inside** `rect` in `c`, src-over blended,
    /// clipped to the buffer.
    ///
    /// The CPU tier's [`DisplayItem::Border`] rasterization: paints the ring of pixels
    /// that lie inside the outer rounded rect (`rect`, corner radius `radius`) but outside
    /// the inner one (`rect` inset by `width`, radius shrunk by `width`). A non-positive
    /// `width` paints nothing; `width` and `radius` are each clamped to half the rect's
    /// shorter side. With `radius == 0.0` the ring is the four square edge bands; with a
    /// positive radius the corners are carved to match the rounded fill (and the GPU
    /// tier's rounded stroke), so a `border-radius` box frames correctly.
    ///
    /// The ring edges are **antialiased**. A pixel is classified by sampling its four
    /// cell corners against the ring; if they all agree it is fully in the ring (opaque)
    /// or fully out (untouched) — the cheap path that keeps the interior band byte-exact.
    /// A pixel the outer or inner boundary crosses (corners disagree) is **4×4
    /// supersampled** ([`ring_coverage`]) and composited at the resulting fractional
    /// coverage, so both edges of the frame — and their rounded corners — read smooth.
    pub fn stroke_rect(&mut self, rect: Rect, c: Color, width: f32, radius: f32) {
        if width <= 0.0 {
            return;
        }
        let half = 0.5 * rect.size.w.min(rect.size.h);
        let w = width.min(half);
        if w <= 0.0 {
            return;
        }
        let outer_r = radius.max(0.0).min(half);
        // Inner edge of the band: the border-box inset by `w`, with the radius shrunk
        // to keep the ring an even `w` thick around the curve.
        let inner = Rect {
            origin: Point {
                x: rect.origin.x + w,
                y: rect.origin.y + w,
            },
            size: Size {
                w: (rect.size.w - 2.0 * w).max(0.0),
                h: (rect.size.h - 2.0 * w).max(0.0),
            },
        };
        let inner_r = (outer_r - w).max(0.0);
        let opaque = c.a == 255;

        let x0 = (rect.origin.x as usize).min(self.width);
        let y0 = (rect.origin.y as usize).min(self.height);
        let x1 = ceil_to_usize(rect.origin.x + rect.size.w).min(self.width);
        let y1 = ceil_to_usize(rect.origin.y + rect.size.h).min(self.height);

        for y in y0..y1 {
            for x in x0..x1 {
                let fx = x as f32;
                let fy = y as f32;
                // Membership at the four cell corners: a boundary (the outer or inner
                // edge) crosses this pixel iff they disagree.
                let c00 = point_in_ring(fx, fy, rect, outer_r, inner, inner_r);
                let c10 = point_in_ring(fx + 1.0, fy, rect, outer_r, inner, inner_r);
                let c01 = point_in_ring(fx, fy + 1.0, rect, outer_r, inner, inner_r);
                let c11 = point_in_ring(fx + 1.0, fy + 1.0, rect, outer_r, inner, inner_r);
                let cov = if c00 == c10 && c00 == c01 && c00 == c11 {
                    // All corners agree: deep in the band (opaque) or fully outside it.
                    if c00 {
                        255
                    } else {
                        0
                    }
                } else {
                    // An edge crosses this cell: estimate fractional coverage.
                    ring_coverage(x, y, rect, outer_r, inner, inner_r)
                };
                if cov == 0 {
                    continue;
                }
                let idx = (y * self.width + x) * 4;
                let px = &mut self.data[idx..idx + 4];
                if cov == 255 {
                    if opaque {
                        px.copy_from_slice(&[c.r, c.g, c.b, c.a]);
                    } else {
                        blend_over(px, c);
                    }
                } else {
                    blend_cov(px, c, cov);
                }
            }
        }
    }

    /// Fill `rect` with a **linear gradient** that interpolates `stops` along
    /// `direction`, src-over blended over the buffer, clipped to the buffer.
    ///
    /// For [`GradientDirection::Vertical`] the axis runs top→bottom (the first stop at
    /// the top edge, the last at the bottom); for [`GradientDirection::Horizontal`] it
    /// runs left→right. Each pixel's position `t ∈ [0, 1]` along the axis selects a
    /// color by [`gradient_color_at`] (straight-alpha lerp between the bracketing
    /// stops), which is then composited per [`blend_over`] — so a translucent stop set
    /// fades over the background instead of overwriting it.
    ///
    /// An empty stop set paints nothing; a single stop fills solid with that color.
    /// `t` is taken at the pixel center, normalized over the rect's logical extent.
    pub fn fill_gradient(
        &mut self,
        rect: Rect,
        stops: &GradientStops,
        direction: GradientDirection,
    ) {
        let s = stops.as_slice();
        if s.is_empty() {
            return;
        }
        let lx = rect.origin.x;
        let ty = rect.origin.y;
        let rx = rect.origin.x + rect.size.w;
        let by = rect.origin.y + rect.size.h;
        if rx <= lx || by <= ty {
            return;
        }
        let x0 = (lx.max(0.0) as usize).min(self.width);
        let y0 = (ty.max(0.0) as usize).min(self.height);
        let x1 = ceil_to_usize(rx).min(self.width);
        let y1 = ceil_to_usize(by).min(self.height);

        // Axis extent (guard a zero-extent rect, though the early-out above covers it).
        let extent = match direction {
            GradientDirection::Vertical => (by - ty).max(1e-6),
            GradientDirection::Horizontal => (rx - lx).max(1e-6),
        };

        for y in y0..y1 {
            // For a vertical gradient the whole row shares one `t`/color — compute once.
            let row_color = if matches!(direction, GradientDirection::Vertical) {
                let t = ((y as f32 + 0.5) - ty) / extent;
                Some(gradient_color_at(s, t))
            } else {
                None
            };
            for x in x0..x1 {
                let color = match row_color {
                    Some(c) => c,
                    None => {
                        let t = ((x as f32 + 0.5) - lx) / extent;
                        gradient_color_at(s, t)
                    }
                };
                let i = (y * self.width + x) * 4;
                blend_over(&mut self.data[i..i + 4], color);
            }
        }
    }

    /// Paint a soft **outset drop shadow** for the box `rect`, translated by `offset`
    /// and feathered by `blur` logical px, in `color`, src-over blended under whatever
    /// is already in the buffer (it is emitted before the box). Clipped to the buffer.
    ///
    /// The shadow is the element's border-box, offset, with a `blur`-wide linear alpha
    /// ramp on every side: full `color.a` at the (offset) box edge falling to `0` at
    /// `blur` distance outside. The falloff uses the distance outside the offset core
    /// rect; the diagonal distance near a corner uses an integer-domain `isqrt` of the
    /// squared distance, so there is **no `f32::sqrt`** (a `std`-only intrinsic) —
    /// keeping this `no_std`. This mirrors the Stylo CPU tier's distance-ramp shadow.
    pub fn fill_shadow(&mut self, rect: Rect, color: Color, blur: f32, offset: Point) {
        if color.a == 0 {
            return;
        }
        let blur = blur.max(0.0);
        // Offset core rect (the box, translated): inside it the shadow is at full alpha.
        let cx0 = rect.origin.x + offset.x;
        let cy0 = rect.origin.y + offset.y;
        let cx1 = cx0 + rect.size.w;
        let cy1 = cy0 + rect.size.h;
        if cx1 <= cx0 || cy1 <= cy0 {
            return;
        }

        // Outer bounds: the core inflated by `blur`, clipped to the buffer.
        let x0 = ((cx0 - blur).max(0.0) as usize).min(self.width);
        let y0 = ((cy0 - blur).max(0.0) as usize).min(self.height);
        let x1 = ceil_to_usize(cx1 + blur).min(self.width);
        let y1 = ceil_to_usize(cy1 + blur).min(self.height);

        let base_a = u32::from(color.a);
        for y in y0..y1 {
            let fy = y as f32 + 0.5;
            // Distance OUTSIDE the core on the y axis (0 within the core's vertical span).
            let dy = (cy0 - fy).max(fy - cy1).max(0.0);
            for x in x0..x1 {
                let fx = x as f32 + 0.5;
                let dx = (cx0 - fx).max(fx - cx1).max(0.0);
                // Euclidean distance outside the core (no sqrt: integer isqrt of d²).
                let dist = if dx == 0.0 {
                    dy
                } else if dy == 0.0 {
                    dx
                } else {
                    let d2 = dx * dx + dy * dy;
                    isqrt_f32(d2)
                };
                let falloff = if blur <= 0.0 {
                    if dist <= 0.0 {
                        1.0
                    } else {
                        0.0
                    }
                } else {
                    (1.0 - dist / blur).clamp(0.0, 1.0)
                };
                if falloff <= 0.0 {
                    continue;
                }
                let a = ((base_a as f32 * falloff) + 0.5) as u32;
                if a == 0 {
                    continue;
                }
                let i = (y * self.width + x) * 4;
                blend_over(
                    &mut self.data[i..i + 4],
                    Color {
                        r: color.r,
                        g: color.g,
                        b: color.b,
                        a: a.min(255) as u8,
                    },
                );
            }
        }
    }

    /// Paint one opaque pixel, clipped to the buffer.
    fn put_pixel(&mut self, x: usize, y: usize, c: Color) {
        if x >= self.width || y >= self.height {
            return;
        }
        let i = (y * self.width + x) * 4;
        self.data[i..i + 4].copy_from_slice(&[c.r, c.g, c.b, c.a]);
    }

    /// Blit `text` as a baked-bitmap-font run in `color`, starting at `origin` and
    /// advancing one cell per character.
    ///
    /// Only "ink" bits are painted; non-ink pixels are left untouched so the element
    /// background shows through. `size` is the target cell height in pixels; the
    /// font is 8px tall, so the integer scale is `max(1, (size / 8).floor())`. Each
    /// source pixel is drawn as a `scale`×`scale` block.
    ///
    /// The run is **vertically centered within the line height**: see
    /// [`blit_text_in_box`](Buffer::blit_text_in_box) for the centering math. This
    /// convenience wrapper applies no horizontal box clip (the run is bounded only by
    /// the buffer edges) — the renderer's `DisplayItem::Text` path calls
    /// [`blit_text_in_box`](Buffer::blit_text_in_box) directly to clip to the box.
    pub fn blit_text(&mut self, origin: Point, text: &str, color: Color, size: f32) {
        // No box clip: bound only by the buffer (an effectively infinite right edge).
        self.blit_text_in_box(origin, text, color, size, f32::INFINITY);
    }

    /// Blit `text` like [`blit_text`](Buffer::blit_text), but **clipped to a box's
    /// right edge** and **vertically centered within the line height**.
    ///
    /// ## Vertical centering
    /// The baked glyph occupies only `CELL_H * scale` pixels, while the layout asked
    /// for a `size`-px-tall line. When `size` exceeds that baked pixel height (the
    /// usual case for any `size` that is not an exact multiple of 8 — e.g. `size = 20`
    /// is scale 2 -> 16 baked px, leaving 4px of slack) the run is offset down by
    /// `(size - CELL_H * scale) / 2` so the glyphs sit in the vertical middle of the
    /// line box instead of pinned to its top.
    ///
    /// Limitation: `DisplayItem::Text` carries no `box_h`, only the line `size`, so
    /// the renderer can only center the glyphs *within the baked line height* (`size`),
    /// not within a taller layout box. Centering a short line inside a tall box would
    /// need the box height on the seam; here we center within the geometry we have.
    ///
    /// ## Horizontal box clipping
    /// `box_right` is the run box's absolute right edge in pixels (origin x + box
    /// width). Any glyph pixel at or past `box_right` is dropped, so a run longer than
    /// its container stops at the box edge rather than spilling past it. This is a box
    /// clip, distinct from the buffer-bounds clip in [`put_pixel`](Buffer::put_pixel):
    /// it bounds the run to its container even when the buffer is wider. Pass
    /// `f32::INFINITY` for no box clip.
    pub fn blit_text_in_box(
        &mut self,
        origin: Point,
        text: &str,
        color: Color,
        size: f32,
        box_right: f32,
    ) {
        let scale = ((size / CELL_H as f32) as usize).max(1);
        let advance = CELL_W as usize * scale;
        let ox = origin.x as usize;
        // Vertical center within the line height: push the baked glyph (CELL_H * scale
        // px tall) down by half the leftover slack so it sits mid-line, not top-pinned.
        let glyph_h = CELL_H as usize * scale;
        let slack = (size as usize).saturating_sub(glyph_h);
        let oy = origin.y as usize + slack / 2;
        // Box-width clip: never paint a pixel at/after the box's right edge. Saturating
        // `f32 as usize` keeps a negative/empty box from wrapping to a huge bound; an
        // infinite (no-clip) edge saturates to `usize::MAX`, so nothing is clipped.
        let clip_x = if box_right >= usize::MAX as f32 {
            usize::MAX
        } else {
            box_right.max(0.0) as usize
        };
        for (col, ch) in text.chars().enumerate() {
            let bitmap = glyph(ch);
            let cell_x = ox + col * advance;
            for (row, bits) in bitmap.iter().enumerate() {
                for bit in 0..CELL_W as usize {
                    // bit 7 (0x80) is the leftmost pixel.
                    if bits & (0x80 >> bit) != 0 {
                        let px0 = cell_x + bit * scale;
                        let py0 = oy + row * scale;
                        for dy in 0..scale {
                            for dx in 0..scale {
                                let px = px0 + dx;
                                // Stop the run at the box's right edge.
                                if px >= clip_x {
                                    continue;
                                }
                                self.put_pixel(px, py0 + dy, color);
                            }
                        }
                    }
                }
            }
        }
    }

    /// Encode as a binary PPM (P6) — a tiny, viewable artifact with no dependencies.
    pub fn to_ppm(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(
            alloc::format!("P6\n{} {}\n255\n", self.width, self.height).as_bytes(),
        );
        for px in self.data.chunks_exact(4) {
            out.extend_from_slice(&px[0..3]);
        }
        out
    }
}

/// A [`Renderer`] that rasterizes into an owned [`Buffer`].
pub struct SoftwareRenderer {
    buffer: Buffer,
    clear: Color,
}

impl SoftwareRenderer {
    /// New renderer with a `clear` background color.
    pub fn new(width: usize, height: usize, clear: Color) -> Self {
        Self {
            buffer: Buffer::new(width, height),
            clear,
        }
    }

    /// The current frame buffer.
    pub fn buffer(&self) -> &Buffer {
        &self.buffer
    }

    /// Mutable access to the frame buffer — e.g. to composite another surface (a
    /// plugin panel) into the painted frame before presenting.
    pub fn buffer_mut(&mut self) -> &mut Buffer {
        &mut self.buffer
    }
}

impl Renderer for SoftwareRenderer {
    fn resize(&mut self, size: Size) {
        self.buffer = Buffer::new(size.w as usize, size.h as usize);
    }

    fn render(&mut self, scene: &DisplayList) -> Result<(), HostError> {
        self.buffer.clear(self.clear);
        for item in &scene.items {
            match item {
                DisplayItem::Rect {
                    rect,
                    color,
                    radius,
                } => {
                    // Square is the common case; only pay the per-pixel corner test
                    // when a positive radius is actually requested.
                    if *radius > 0.0 {
                        self.buffer.fill_round_rect(*rect, *color, *radius);
                    } else {
                        self.buffer.fill_rect(*rect, *color);
                    }
                }
                DisplayItem::Text {
                    origin,
                    text,
                    color,
                    size,
                    box_w,
                    align,
                } => {
                    // Center / right-align the baked run within its box using the
                    // run's OWN baked width (chars * advance at this size), offset by
                    // `(box_w - run_w) * align` clamped to >= 0. For the baked path the
                    // run usually equals the box, so the offset is ~0; `align == 0.0`
                    // is byte-for-byte the legacy left-aligned blit.
                    let scale = ((size / CELL_H as f32) as usize).max(1);
                    let advance = (CELL_W as usize * scale) as f32;
                    let run_w = text.chars().count() as f32 * advance;
                    // The run box's right edge is the *unshifted* origin x plus the box
                    // width; capture it before shadowing `origin` with the align shift.
                    // Glyph pixels at/past this are clipped so the run cannot spill out
                    // of its container — composes with the align shift and the vertical
                    // centering done inside `blit_text`.
                    let box_right = origin.x + box_w;
                    let origin = Point {
                        x: origin.x + ((box_w - run_w) * align).max(0.0),
                        y: origin.y,
                    };
                    self.buffer
                        .blit_text_in_box(origin, text, *color, *size, box_right)
                }
                DisplayItem::Border {
                    rect,
                    color,
                    width,
                    radius,
                } => {
                    // Degraded frame: stroke the four edge bands. The CPU tier has no
                    // rounded-corner stroke, so a positive radius is not carved (the
                    // corners stay square) — a faithful, never-panicking approximation.
                    self.buffer.stroke_rect(*rect, *color, *width, *radius);
                }
                DisplayItem::Gradient {
                    rect,
                    stops,
                    direction,
                } => {
                    // Real linear-gradient fill: interpolate the stops along the axis.
                    self.buffer.fill_gradient(*rect, stops, *direction);
                }
                DisplayItem::Shadow {
                    rect,
                    color,
                    blur,
                    offset,
                } => {
                    // Soft outset drop shadow: a feathered offset box, blended under the
                    // element (the shadow item is emitted before the box).
                    self.buffer.fill_shadow(*rect, *color, *blur, *offset);
                }
                // Shaped-glyph rasterization arrives with the capable-tier text backend.
                DisplayItem::Glyphs { .. } => {}
                // `DisplayItem` is `#[non_exhaustive]`: a future primitive added to
                // the seam reaches this out-of-tree-style arm. Skipping an unknown
                // item is the safe degradation — paint nothing rather than panic.
                _ => {}
            }
        }
        Ok(())
    }

    fn present(&mut self) -> Result<(), HostError> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_traits::Point;

    #[test]
    fn fills_a_clipped_rect() {
        let mut b = Buffer::new(10, 10);
        b.fill_rect(
            Rect {
                origin: Point { x: 8.0, y: 8.0 },
                size: Size { w: 100.0, h: 100.0 },
            },
            Color {
                r: 1,
                g: 2,
                b: 3,
                a: 255,
            },
        );
        assert_eq!(b.pixel(9, 9), [1, 2, 3, 255]);
        assert_eq!(b.pixel(0, 0), [0, 0, 0, 0]);
        assert_eq!(b.pixel(50, 50), [0, 0, 0, 0]); // out of bounds -> zeros
    }

    #[test]
    fn half_alpha_fill_blends_over_opaque_background() {
        // THE alpha-compositing proof: a 50%-alpha white fill over a known opaque
        // background yields the channel-wise midpoint (src·a + dst·(1−a)), not a
        // straight overwrite. This is what makes a faded-in element actually fade.
        let mut b = Buffer::new(4, 4);
        let bg = Color {
            r: 0,
            g: 100,
            b: 200,
            a: 255,
        };
        b.clear(bg);
        // White at alpha 128 (~0.502).
        let src = Color {
            r: 255,
            g: 255,
            b: 255,
            a: 128,
        };
        b.fill_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 4.0, h: 4.0 },
            },
            src,
        );
        // mix(s, d) = (s*128 + d*127 + 127) / 255.
        // r: (255*128 + 0*127 + 127)/255 = 128
        // g: (255*128 + 100*127 + 127)/255 = 178
        // b: (255*128 + 200*127 + 127)/255 = 228
        // a: (255*128 + 255*127 + 127)/255 = 255
        assert_eq!(b.pixel(0, 0), [128, 178, 228, 255]);
        // Every covered pixel got the same blend.
        assert_eq!(b.pixel(3, 3), [128, 178, 228, 255]);
    }

    #[test]
    fn opaque_fill_still_overwrites_exactly() {
        // The fast path (a == 255) must remain a byte-exact overwrite, so opaque
        // scenes are unchanged by the new blend path.
        let mut b = Buffer::new(2, 2);
        b.clear(Color {
            r: 10,
            g: 20,
            b: 30,
            a: 255,
        });
        let src = Color {
            r: 200,
            g: 150,
            b: 100,
            a: 255,
        };
        b.fill_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 2.0, h: 2.0 },
            },
            src,
        );
        assert_eq!(
            b.pixel(0, 0),
            [200, 150, 100, 255],
            "opaque overwrites exactly"
        );
        assert_eq!(b.pixel(1, 1), [200, 150, 100, 255]);
    }

    #[test]
    fn zero_alpha_fill_leaves_the_background() {
        // A fully transparent fill is a no-op on the color channels (the limit of the
        // blend as a -> 0).
        let mut b = Buffer::new(2, 2);
        let bg = Color {
            r: 5,
            g: 6,
            b: 7,
            a: 255,
        };
        b.clear(bg);
        b.fill_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 2.0, h: 2.0 },
            },
            Color {
                r: 255,
                g: 255,
                b: 255,
                a: 0,
            },
        );
        assert_eq!(
            b.pixel(0, 0),
            [5, 6, 7, 255],
            "alpha 0 leaves the background"
        );
    }

    #[test]
    fn half_alpha_round_rect_blends_center_keeps_carved_corner() {
        // The rounded fill blends like the square one: the center is the half-mix,
        // while a carved-away corner is untouched (keeps the background).
        let mut b = Buffer::new(40, 40);
        let bg = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        b.clear(bg);
        let src = Color {
            r: 255,
            g: 255,
            b: 255,
            a: 128,
        };
        b.fill_round_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 40.0, h: 40.0 },
            },
            src,
            12.0,
        );
        // Center: white@128 over black -> (255*128 + 0*127 + 127)/255 = 128 per RGB.
        assert_eq!(
            b.pixel(20, 20),
            [128, 128, 128, 255],
            "center is the half-mix"
        );
        // Extreme corner is carved away: stays the black background.
        assert_eq!(
            b.pixel(0, 0),
            [0, 0, 0, 255],
            "carved corner keeps background"
        );
    }

    #[test]
    fn renders_text_ink_over_background() {
        // A known background so we can tell ink from untouched pixels.
        let bg = Color {
            r: 0x10,
            g: 0x20,
            b: 0x30,
            a: 255,
        };
        let ink = Color {
            r: 0xff,
            g: 0xd0,
            b: 0x40,
            a: 255,
        };
        let mut r = SoftwareRenderer::new(32, 16, bg);
        let scene = DisplayList {
            items: vec![DisplayItem::Text {
                origin: Point { x: 0.0, y: 0.0 },
                text: "A".into(),
                color: ink,
                size: 8.0, // scale 1: glyph maps 1:1 onto the cell
                // box equals the single-cell run, left-aligned: no offset, so this
                // stays the legacy 1:1 blit at the origin.
                box_w: 8.0,
                align: 0.0,
            }],
        };
        r.render(&scene).unwrap();
        let buf = r.buffer();

        // At scale 1 the 'A' apex is row 0 = 0x38 (0011_1000), so columns 2,3,4 are
        // ink while column 0 is clear.
        assert_eq!(buf.pixel(2, 0), [ink.r, ink.g, ink.b, ink.a], "ink pixel");

        // The top-left corner of the cell is a clear bit, so it keeps the cleared
        // background — text never paints non-ink pixels.
        assert_eq!(
            buf.pixel(0, 0),
            [bg.r, bg.g, bg.b, bg.a],
            "off-glyph pixel unchanged"
        );

        // Somewhere in the glyph there is at least one ink pixel equal to the color.
        let any_ink =
            (0..8).any(|y| (0..8).any(|x| buf.pixel(x, y) == [ink.r, ink.g, ink.b, ink.a]));
        assert!(any_ink, "expected at least one ink pixel");
    }

    /// THE text-align proof on the deterministic baked path: a single-cell run in a
    /// box much wider than the run, with `align = 0.5`, must have its ink land near
    /// the box center — not near x=0. The baked 'A' at scale 1 is one 8px cell; a
    /// `box_w` of 64 offsets it by `(64 - 8) * 0.5 = 28`, so the glyph cell occupies
    /// columns 28..36 (its leftmost lit bit is cell-column 0 -> absolute column 28),
    /// snug around the box midpoint (32), with the whole left region untouched
    /// background.
    #[test]
    fn text_align_center_centers_the_baked_run_in_its_box() {
        let bg = Color {
            r: 0x10,
            g: 0x20,
            b: 0x30,
            a: 255,
        };
        let ink = Color {
            r: 0xff,
            g: 0xff,
            b: 0xff,
            a: 255,
        };
        let box_w = 64.0_f32;
        let mut r = SoftwareRenderer::new(box_w as usize, 8, bg);
        let scene = DisplayList {
            items: vec![DisplayItem::Text {
                origin: Point { x: 0.0, y: 0.0 },
                text: "A".into(),
                color: ink,
                size: 8.0, // scale 1: one 8px cell
                box_w,
                align: 0.5, // centered
            }],
        };
        r.render(&scene).unwrap();
        let buf = r.buffer();

        let is_ink = |x: usize, y: usize| buf.pixel(x, y) == [ink.r, ink.g, ink.b, ink.a];
        // The leftmost inked column across the whole buffer.
        let leftmost_ink = (0..buf.width())
            .find(|&x| (0..buf.height()).any(|y| is_ink(x, y)))
            .expect("the run must have inked some column");

        // Offset = (64 - 8) * 0.5 = 28; the 'A' glyph's leftmost lit bit is
        // cell-column 0 (rows 0xC6/0xFE), so the leftmost ink is at absolute column
        // 28 — snug around box center 32 and far from the left edge: proof the run is
        // centered, not left-stuck.
        assert_eq!(leftmost_ink, 28, "leftmost ink sits near the box center");
        let center = (box_w as usize) / 2; // 32
        assert!(
            leftmost_ink.abs_diff(center) <= 4,
            "leftmost ink {leftmost_ink} should be within a few px of center {center}"
        );

        // The entire left quarter of the box is untouched background — the glyphs are
        // not stuck at the left edge.
        for x in 0..(box_w as usize / 4) {
            for y in 0..buf.height() {
                assert_eq!(
                    buf.pixel(x, y),
                    [bg.r, bg.g, bg.b, bg.a],
                    "left region must stay background, ink at ({x},{y})"
                );
            }
        }
    }

    /// THE vertical-centering proof: a baked run on a line *taller* than the baked
    /// glyph height must have its ink land in the vertical middle band of the line,
    /// not pinned to the top row. `size = 20` is scale 2 (glyph is 16 baked px), so
    /// there is `20 - 16 = 4` px of slack and the glyph is pushed down `4 / 2 = 2` px;
    /// the top two buffer rows stay background and the 'A' crossbar lands mid-buffer.
    #[test]
    fn text_is_vertically_centered_in_a_tall_line() {
        let bg = Color {
            r: 0x10,
            g: 0x20,
            b: 0x30,
            a: 255,
        };
        let ink = Color {
            r: 0xff,
            g: 0xff,
            b: 0xff,
            a: 255,
        };
        // A 20px-tall line (scale 2 -> 16 baked px, 4px slack). Buffer is the line
        // height so "middle band" is unambiguous; box is one glyph cell wide.
        let mut r = SoftwareRenderer::new(16, 20, bg);
        let scene = DisplayList {
            items: vec![DisplayItem::Text {
                origin: Point { x: 0.0, y: 0.0 },
                text: "A".into(),
                color: ink,
                size: 20.0,
                box_w: 16.0,
                align: 0.0,
            }],
        };
        r.render(&scene).unwrap();
        let buf = r.buffer();
        let is_ink = |x: usize, y: usize| buf.pixel(x, y) == [ink.r, ink.g, ink.b, ink.a];
        let row_has_ink = |y: usize| (0..buf.width()).any(|x| is_ink(x, y));

        // The top two rows are the centering slack: glyph row 0 is offset down to
        // buffer row 2, so rows 0 and 1 stay background (not top-pinned).
        assert!(!row_has_ink(0), "top row must be background, not ink");
        assert!(!row_has_ink(1), "second row must be background, not ink");

        // The vertical middle band of the line carries ink (the 'A' crossbar, row 4 of
        // the glyph -> buffer rows 10/11 at scale 2): proof the run sits mid-line.
        let mid = buf.height() / 2; // 10
        assert!(
            (mid.saturating_sub(1)..=mid + 1).any(row_has_ink),
            "expected ink in the vertical middle band around row {mid}"
        );

        // The topmost inked row is the slack offset (2), not 0.
        let topmost_ink = (0..buf.height())
            .find(|&y| row_has_ink(y))
            .expect("the run must have inked some row");
        assert_eq!(
            topmost_ink, 2,
            "ink starts at the centering offset, not the top"
        );
    }

    /// THE box-clip proof: a run wider than its box stops at the box's right edge —
    /// no glyph pixel is painted at or past it — even though the buffer is wider, so
    /// this is a true box clip, not the buffer-bounds clip. "AAAA" at scale 1 is a
    /// 32px run; a `box_w` of 16 admits only the first two cells, and columns 16.. of
    /// the wider buffer stay background.
    #[test]
    fn text_wider_than_its_box_is_clipped_at_the_box_edge() {
        let bg = Color {
            r: 0x10,
            g: 0x20,
            b: 0x30,
            a: 255,
        };
        let ink = Color {
            r: 0xff,
            g: 0xff,
            b: 0xff,
            a: 255,
        };
        let box_w = 16.0_f32; // two 8px cells
                              // Buffer (40px) is wider than the box, so any ink past col 16 would be a
                              // genuine box-clip failure, not a buffer overflow.
        let mut r = SoftwareRenderer::new(40, 8, bg);
        let scene = DisplayList {
            items: vec![DisplayItem::Text {
                origin: Point { x: 0.0, y: 0.0 },
                text: "AAAA".into(), // 4 cells = 32px, twice the box width
                color: ink,
                size: 8.0, // scale 1
                box_w,
                align: 0.0,
            }],
        };
        r.render(&scene).unwrap();
        let buf = r.buffer();
        let is_ink = |x: usize, y: usize| buf.pixel(x, y) == [ink.r, ink.g, ink.b, ink.a];

        // No ink at or past the box's right edge (column 16): the run is clipped.
        let right = box_w as usize; // 16
        for x in right..buf.width() {
            for y in 0..buf.height() {
                assert_eq!(
                    buf.pixel(x, y),
                    [bg.r, bg.g, bg.b, bg.a],
                    "no ink may spill past the box edge, found at ({x},{y})"
                );
            }
        }

        // The run did draw *inside* the box — proof we clipped the overflow, not the
        // whole run (the first 'A' apex inks columns 2..5 of cell 0).
        let any_ink_in_box = (0..right).any(|x| (0..buf.height()).any(|y| is_ink(x, y)));
        assert!(
            any_ink_in_box,
            "the part of the run inside the box must be drawn"
        );
    }

    /// THE rounded-rect proof: a filled rounded rect over a contrasting background
    /// must leave its corner pixels showing the *background* while the center is the
    /// *fill* — i.e. the corner quadrants are genuinely carved away, not just drawn
    /// square.
    #[test]
    fn round_rect_clears_corners_keeps_center() {
        let bg = Color {
            r: 0x10,
            g: 0x20,
            b: 0x30,
            a: 255,
        };
        let fill = Color {
            r: 0xf3,
            g: 0x8b,
            b: 0xa8,
            a: 255,
        };
        let mut b = Buffer::new(40, 40);
        b.clear(bg);
        b.fill_round_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 40.0, h: 40.0 },
            },
            fill,
            12.0,
        );

        // The four extreme corner pixels lie well outside the quarter-circles, so
        // they keep the background.
        let bg_px = [bg.r, bg.g, bg.b, bg.a];
        assert_eq!(b.pixel(0, 0), bg_px, "top-left corner kept background");
        assert_eq!(b.pixel(39, 0), bg_px, "top-right corner kept background");
        assert_eq!(b.pixel(0, 39), bg_px, "bottom-left corner kept background");
        assert_eq!(
            b.pixel(39, 39),
            bg_px,
            "bottom-right corner kept background"
        );

        // The center is solidly inside the rect, so it is the fill color.
        let fill_px = [fill.r, fill.g, fill.b, fill.a];
        assert_eq!(b.pixel(20, 20), fill_px, "center is the fill color");

        // A mid-edge pixel (no corner rounding on the straight edges) is also fill.
        assert_eq!(b.pixel(20, 0), fill_px, "top mid-edge is straight (fill)");
        assert_eq!(b.pixel(0, 20), fill_px, "left mid-edge is straight (fill)");
    }

    /// The same guarantee through the full `Renderer` path: a `DisplayItem::Rect`
    /// carrying a positive `radius` paints rounded, clearing the corner to the
    /// clear color while the center is the fill. Writes a viewable PPM artifact.
    #[test]
    fn renderer_paints_rounded_rect_from_display_item() {
        let clear = Color {
            r: 0x18,
            g: 0x18,
            b: 0x24,
            a: 255,
        };
        let fill = Color {
            r: 0x89,
            g: 0xb4,
            b: 0xfa,
            a: 255,
        };
        let mut r = SoftwareRenderer::new(64, 64, clear);
        let scene = DisplayList {
            items: vec![DisplayItem::Rect {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size: Size { w: 64.0, h: 64.0 },
                },
                color: fill,
                radius: 20.0,
            }],
        };
        r.render(&scene).unwrap();
        let buf = r.buffer();

        // Corner shows the clear color (carved away); center is the fill.
        assert_eq!(
            buf.pixel(0, 0),
            [clear.r, clear.g, clear.b, clear.a],
            "rounded corner shows the clear color"
        );
        assert_eq!(
            buf.pixel(32, 32),
            [fill.r, fill.g, fill.b, fill.a],
            "center is the fill color"
        );

        // Write a viewable PPM artifact next to the crate's target dir (matches the
        // render-text test's artifact convention).
        let ppm = buf.to_ppm();
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("target")
            .join("round_rect.ppm");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &ppm).unwrap();
    }

    /// An oversized radius clamps to half the shorter side: a square rect becomes a
    /// circle, so its bounding-box corners are background and its center is fill.
    #[test]
    fn oversized_radius_clamps_to_a_circle() {
        let bg = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let fill = Color {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        };
        let mut b = Buffer::new(20, 20);
        b.clear(bg);
        // A radius far larger than the box clamps to 10 (half of 20) -> a circle.
        b.fill_round_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 20.0, h: 20.0 },
            },
            fill,
            1000.0,
        );
        assert_eq!(
            b.pixel(0, 0),
            [bg.r, bg.g, bg.b, bg.a],
            "corner outside circle"
        );
        assert_eq!(
            b.pixel(10, 10),
            [fill.r, fill.g, fill.b, fill.a],
            "center inside circle"
        );
        // The mid-edge touches the circle, so it is filled.
        assert_eq!(
            b.pixel(10, 0),
            [fill.r, fill.g, fill.b, fill.a],
            "mid-edge on circle"
        );
    }

    /// THE degraded-border proof: a `DisplayItem::Border` strokes a frame — the
    /// edge bands take the border color while the interior keeps the background, so
    /// the box reads as framed, not filled.
    #[test]
    fn border_strokes_a_frame_keeps_interior() {
        let bg = Color {
            r: 0x10,
            g: 0x20,
            b: 0x30,
            a: 255,
        };
        let frame = Color {
            r: 0xff,
            g: 0x00,
            b: 0x00,
            a: 255,
        };
        let mut r = SoftwareRenderer::new(20, 20, bg);
        let scene = DisplayList {
            items: vec![DisplayItem::Border {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size: Size { w: 20.0, h: 20.0 },
                },
                color: frame,
                width: 3.0,
                radius: 0.0,
            }],
        };
        r.render(&scene).unwrap();
        let buf = r.buffer();
        let frame_px = [frame.r, frame.g, frame.b, frame.a];
        let bg_px = [bg.r, bg.g, bg.b, bg.a];
        // Edge pixels (within the 3px band) are the frame color.
        assert_eq!(buf.pixel(0, 10), frame_px, "left edge is the border");
        assert_eq!(buf.pixel(19, 10), frame_px, "right edge is the border");
        assert_eq!(buf.pixel(10, 0), frame_px, "top edge is the border");
        assert_eq!(buf.pixel(10, 19), frame_px, "bottom edge is the border");
        // The interior (well inside the 3px band) keeps the background — a frame,
        // not a fill.
        assert_eq!(buf.pixel(10, 10), bg_px, "interior stays background");
    }

    /// THE gradient proof (rewritten from the old `gradient_degrades_to_first_stop_solid_fill`,
    /// which asserted the now-removed flat first-stop degradation): a vertical
    /// `DisplayItem::Gradient` between two opaque stops paints a **real ramp**. A
    /// midpoint pixel is strictly between the two stop colors on every channel, the top
    /// row is near the first stop and the bottom row near the last, and the color
    /// increases monotonically down the axis.
    #[test]
    fn gradient_interpolates_between_its_stops() {
        use canopy_traits::{GradientDirection, GradientStop, GradientStops};
        let bg = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let first = Color {
            r: 0x12,
            g: 0x34,
            b: 0x56,
            a: 255,
        };
        let last = Color {
            r: 0xab,
            g: 0xcd,
            b: 0xef,
            a: 255,
        };
        let mut r = SoftwareRenderer::new(16, 16, bg);
        let scene = DisplayList {
            items: vec![DisplayItem::Gradient {
                rect: Rect {
                    origin: Point { x: 0.0, y: 0.0 },
                    size: Size { w: 16.0, h: 16.0 },
                },
                stops: GradientStops::from_slice(&[
                    GradientStop {
                        color: first,
                        position: 0.0,
                    },
                    GradientStop {
                        color: last,
                        position: 1.0,
                    },
                ]),
                direction: GradientDirection::Vertical,
            }],
        };
        r.render(&scene).unwrap();
        let buf = r.buffer();

        // The vertical midpoint (row 8, t ≈ 0.53) is strictly between the two stops on
        // every RGB channel — the heart of "it interpolates, not flat-fills".
        let mid = buf.pixel(0, 8);
        // first < last on every channel here, so a real interpolation lands strictly
        // between them.
        let lows = [first.r, first.g, first.b];
        let highs = [last.r, last.g, last.b];
        for (ch, &m) in mid.iter().take(3).enumerate() {
            let (a, b) = (lows[ch], highs[ch]);
            assert!(
                m > a && m < b,
                "channel {ch}: midpoint {m} must be strictly between {a} and {b}"
            );
        }

        // Ends land near their stops (within the per-row step of a 16px ramp).
        let top = buf.pixel(0, 0);
        let bottom = buf.pixel(0, 15);
        assert!(
            top[0].abs_diff(first.r) <= 8 && bottom[0].abs_diff(last.r) <= 8,
            "top {top:?} near first {first:?}, bottom {bottom:?} near last {last:?}"
        );

        // Monotonic increase top→bottom on the red channel (the ramp has a direction).
        assert!(
            top[0] < mid[0] && mid[0] < bottom[0],
            "red must rise down the vertical axis: {} < {} < {}",
            top[0],
            mid[0],
            bottom[0]
        );
        // A horizontally-offset pixel on the same row matches (vertical ⇒ row-constant).
        assert_eq!(
            buf.pixel(0, 8),
            buf.pixel(15, 8),
            "a vertical gradient is constant across each row"
        );
    }

    /// THE soft-shadow proof (rewritten from the old `shadow_is_a_no_op_on_cpu`, which
    /// asserted the shadow drew *nothing*): a `DisplayItem::Shadow` now paints a soft
    /// feathered drop. A pixel just outside the box in the offset direction is no longer
    /// the clear background, and it is *softer* (lower contribution) than the shadow's
    /// full color — the alpha ramps down with distance.
    #[test]
    fn shadow_paints_a_soft_feathered_drop() {
        // Opaque white background so a black shadow darkens it measurably.
        let bg = Color {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        };
        // A larger surface so the whole feathered halo fits and we can probe outside it.
        let mut r = SoftwareRenderer::new(32, 32, bg);
        let shadow = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 200,
        };
        let scene = DisplayList {
            items: vec![DisplayItem::Shadow {
                rect: Rect {
                    origin: Point { x: 8.0, y: 8.0 },
                    size: Size { w: 12.0, h: 12.0 },
                },
                color: shadow,
                blur: 4.0,
                offset: Point { x: 2.0, y: 2.0 },
            }],
        };
        r.render(&scene).unwrap();
        let buf = r.buffer();
        let bg_px = [bg.r, bg.g, bg.b, bg.a];

        // The offset core spans x∈[10,22), y∈[10,22) (box at (8,8)+offset (2,2)). A
        // pixel just *outside* the core's right edge, within the blur, is darkened but
        // not black — the soft halo, not the full-strength core.
        let just_outside = buf.pixel(23, 15); // 1px past the core's right edge (22)
        assert_ne!(
            just_outside, bg_px,
            "a pixel just outside the box (in the blur halo) must be shadowed, not clear"
        );
        // Full-strength core pixel: black@200 over white = 255·(55/255) ≈ 55 per channel.
        let core = buf.pixel(15, 15);
        // The halo pixel is *softer* — lighter (higher channel value) than the core, and
        // strictly between the core and the untouched white background.
        assert!(
            just_outside[0] > core[0],
            "halo {just_outside:?} must be lighter (softer) than the core {core:?}"
        );
        assert!(
            just_outside[0] < bg.r,
            "halo {just_outside:?} must still be darker than the clear background {bg_px:?}"
        );

        // The alpha ramp falls off with distance: stepping further out, the shadow keeps
        // fading toward the background (monotonically lighter).
        let nearer = buf.pixel(23, 15);
        let farther = buf.pixel(25, 15);
        assert!(
            farther[0] >= nearer[0],
            "the shadow must keep fading with distance: {} (near) -> {} (far)",
            nearer[0],
            farther[0]
        );

        // Far beyond the blur the surface is untouched (the halo is bounded).
        assert_eq!(
            buf.pixel(31, 31),
            bg_px,
            "well outside the blur the background is untouched"
        );
    }

    /// THE antialiasing proof: a boundary pixel of a rounded-rect corner reads a
    /// **partial, blended** value — neither the full fill nor the untouched background
    /// — because the arc carves it fractionally. (The pre-AA rasterizer made every
    /// pixel binary inside/outside, so this pixel would have been exactly one or the
    /// other.) A deep-interior pixel stays exactly the fill and a far-outside corner
    /// stays exactly the background — only the edge is fractional.
    #[test]
    fn round_rect_corner_pixel_is_antialiased() {
        let bg = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let fill = Color {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        };
        let mut b = Buffer::new(40, 40);
        b.clear(bg);
        b.fill_round_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 40.0, h: 40.0 },
            },
            fill,
            12.0,
        );

        // Scan the top-left corner's diagonal for a pixel that is neither pure fill nor
        // pure background — i.e. a partially-covered (antialiased) edge pixel.
        let bg_px = [bg.r, bg.g, bg.b, bg.a];
        let fill_px = [fill.r, fill.g, fill.b, fill.a];
        let mut found_blended = false;
        for d in 0..14 {
            let p = b.pixel(d, d);
            let blended = p != bg_px && p != fill_px;
            if blended {
                // A blend of opaque white over opaque black: every channel equal and
                // strictly inside (0, 255) — genuine fractional coverage.
                assert!(
                    p[0] > 0 && p[0] < 255,
                    "AA corner pixel {p:?} must be a partial blend, not clear or full"
                );
                found_blended = true;
                break;
            }
        }
        assert!(
            found_blended,
            "expected at least one antialiased (partially covered) pixel along the corner arc"
        );

        // Interior stays exactly the fill; the extreme corner stays exactly background.
        assert_eq!(b.pixel(20, 20), fill_px, "deep interior is exact fill");
        assert_eq!(b.pixel(0, 0), bg_px, "extreme corner is exact background");
    }

    /// AA also applies to a **fractional straight edge**: a rect whose right edge sits
    /// at x = 10.4 leaves pixel column 10 only ~40% covered, so it is a partial blend
    /// over the background, while the fully-covered column 9 is exact fill and the
    /// untouched column 11 is exact background. Proves the `fill_rect` edge AA, not just
    /// the corners.
    #[test]
    fn fill_rect_fractional_edge_is_antialiased() {
        let bg = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };
        let fill = Color {
            r: 255,
            g: 255,
            b: 255,
            a: 255,
        };
        let mut b = Buffer::new(16, 4);
        b.clear(bg);
        b.fill_rect(
            Rect {
                origin: Point { x: 0.0, y: 0.0 },
                size: Size { w: 10.4, h: 4.0 },
            },
            fill,
        );
        // Column 9 is fully inside -> exact fill.
        assert_eq!(
            b.pixel(9, 1),
            [255, 255, 255, 255],
            "interior column is exact fill"
        );
        // Column 10 is ~40% covered -> a partial blend (white@~0.4 over black ≈ 102).
        let edge = b.pixel(10, 1);
        assert!(
            edge[0] > 0 && edge[0] < 255,
            "fractional edge column {edge:?} must be a partial blend"
        );
        assert!(
            (90..=114).contains(&edge[0]),
            "edge coverage ≈ 0.4 -> channel ≈ 102, got {}",
            edge[0]
        );
        // Column 11 is fully outside -> untouched background.
        assert_eq!(
            b.pixel(11, 1),
            [0, 0, 0, 255],
            "past the edge stays background"
        );
    }
}
