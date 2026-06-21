//! The **display-list wire format** — the dual of the op-stream.
//!
//! The op-stream (`canopy-protocol`) flows guest → host to build the tree. This format flows
//! host → consumer to drive a renderer: it serializes a laid-out [`DisplayList`] (the
//! renderer-agnostic geometric primitives — filled/rounded rects, borders, gradients, shadows,
//! text runs, and the clip stack) into a flat byte buffer a non-Rust consumer decodes into its
//! own GPU / 2D-accelerator draw calls. A Rust consumer never needs this — it has the
//! [`DisplayList`] directly (`canopy_layout_taffy::layout`) — but it is how the bare-metal C/C++
//! tier reaches the same geometry without taking the engine's software-rasterized pixels.
//!
//! It is exposed over the C ABI as `canopy_host_build_display_list` and documented for non-Rust
//! authors in `crates/canopy-abi/include/canopy_displaylist.h` (a parity test pins the two).
//!
//! ## Frame
//! ```text
//! version:u16  width:u32  height:u32  count:u32   item*count
//! ```
//! All multi-byte integers are little-endian; `f32` is its IEEE-754 bit pattern as a
//! little-endian u32; `Color` is 4 bytes `r,g,b,a`; `Rect` is 4 f32 `x,y,w,h`; `Point` is 2 f32.
//! Each item is a tag byte then its fields in declaration order (see the tag constants below).

use canopy_traits::{
    Color, DisplayItem, DisplayList, Glyph, GradientDirection, GradientStop, GradientStops, Point,
    Rect, ShapedGlyphs, Size,
};

/// Wire-format version, bumped on any incompatible change to the layout below.
pub const DL_VERSION: u16 = 1;

// Item tag bytes (host -> consumer).
/// A filled (optionally rounded) rectangle: `rect color radius`.
pub const DL_RECT: u8 = 0x01;
/// A shaped-glyph run: `color glyph_count (id:u32 x:f32 y:f32)*`.
pub const DL_GLYPHS: u8 = 0x02;
/// An unshaped (baked-font) text run: `origin color size box_w align text_len:u32 utf8`.
pub const DL_TEXT: u8 = 0x03;
/// A stroked border frame: `rect color width radius`.
pub const DL_BORDER: u8 = 0x04;
/// A linear-gradient fill: `rect direction:u8 stop_count:u8 (color pos:f32)*`.
pub const DL_GRADIENT: u8 = 0x05;
/// An outset drop shadow: `rect color blur offset`.
pub const DL_SHADOW: u8 = 0x06;
/// Push a clip region: `rect radius`. Everything until the matching PopClip is masked to it.
pub const DL_PUSH_CLIP: u8 = 0x07;
/// Pop the most recent clip region: no fields.
pub const DL_POP_CLIP: u8 = 0x08;

/// `GradientDirection::Vertical` on the wire.
pub const DL_DIR_VERTICAL: u8 = 0;
/// `GradientDirection::Horizontal` on the wire.
pub const DL_DIR_HORIZONTAL: u8 = 1;

fn push_f32(out: &mut Vec<u8>, v: f32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn push_u32(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}
fn push_color(out: &mut Vec<u8>, c: Color) {
    out.extend_from_slice(&[c.r, c.g, c.b, c.a]);
}
fn push_rect(out: &mut Vec<u8>, r: Rect) {
    push_f32(out, r.origin.x);
    push_f32(out, r.origin.y);
    push_f32(out, r.size.w);
    push_f32(out, r.size.h);
}
fn push_point(out: &mut Vec<u8>, p: Point) {
    push_f32(out, p.x);
    push_f32(out, p.y);
}

/// Serialize one item; returns `false` for an unknown (`#[non_exhaustive]`) variant so the caller
/// can keep the frame's `count` honest by skipping it.
fn serialize_item(item: &DisplayItem, out: &mut Vec<u8>) -> bool {
    match item {
        DisplayItem::Rect {
            rect,
            color,
            radius,
        } => {
            out.push(DL_RECT);
            push_rect(out, *rect);
            push_color(out, *color);
            push_f32(out, *radius);
        }
        DisplayItem::Glyphs { glyphs, color } => {
            out.push(DL_GLYPHS);
            push_color(out, *color);
            push_u32(out, glyphs.glyphs.len() as u32);
            for g in &glyphs.glyphs {
                push_u32(out, g.id);
                push_f32(out, g.x);
                push_f32(out, g.y);
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
            out.push(DL_TEXT);
            push_point(out, *origin);
            push_color(out, *color);
            push_f32(out, *size);
            push_f32(out, *box_w);
            push_f32(out, *align);
            push_u32(out, text.len() as u32);
            out.extend_from_slice(text.as_bytes());
        }
        DisplayItem::Border {
            rect,
            color,
            width,
            radius,
        } => {
            out.push(DL_BORDER);
            push_rect(out, *rect);
            push_color(out, *color);
            push_f32(out, *width);
            push_f32(out, *radius);
        }
        DisplayItem::Gradient {
            rect,
            stops,
            direction,
        } => {
            out.push(DL_GRADIENT);
            push_rect(out, *rect);
            out.push(match direction {
                GradientDirection::Horizontal => DL_DIR_HORIZONTAL,
                _ => DL_DIR_VERTICAL,
            });
            let slice = stops.as_slice();
            out.push(slice.len() as u8);
            for s in slice {
                push_color(out, s.color);
                push_f32(out, s.position);
            }
        }
        DisplayItem::Shadow {
            rect,
            color,
            blur,
            offset,
        } => {
            out.push(DL_SHADOW);
            push_rect(out, *rect);
            push_color(out, *color);
            push_f32(out, *blur);
            push_point(out, *offset);
        }
        DisplayItem::PushClip { rect, radius } => {
            out.push(DL_PUSH_CLIP);
            push_rect(out, *rect);
            push_f32(out, *radius);
        }
        DisplayItem::PopClip => out.push(DL_POP_CLIP),
        _ => return false, // a future variant a consumer of this version cannot know
    }
    true
}

/// Serialize a laid-out scene into the display-list wire format (see the module docs).
#[must_use]
pub fn serialize(scene: &DisplayList, width: u32, height: u32) -> Vec<u8> {
    let mut body = Vec::new();
    let mut count: u32 = 0;
    for item in &scene.items {
        if serialize_item(item, &mut body) {
            count += 1;
        }
    }
    let mut out = Vec::with_capacity(14 + body.len());
    out.extend_from_slice(&DL_VERSION.to_le_bytes());
    push_u32(&mut out, width);
    push_u32(&mut out, height);
    push_u32(&mut out, count);
    out.extend_from_slice(&body);
    out
}

/// A cursor over the wire bytes used by [`deserialize`].
struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    fn u8(&mut self) -> Option<u8> {
        let b = *self.bytes.get(self.pos)?;
        self.pos += 1;
        Some(b)
    }
    fn u16(&mut self) -> Option<u16> {
        let s = self.bytes.get(self.pos..self.pos + 2)?;
        self.pos += 2;
        Some(u16::from_le_bytes([s[0], s[1]]))
    }
    fn u32(&mut self) -> Option<u32> {
        let s = self.bytes.get(self.pos..self.pos + 4)?;
        self.pos += 4;
        Some(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
    }
    fn f32(&mut self) -> Option<f32> {
        Some(f32::from_bits(self.u32()?))
    }
    fn color(&mut self) -> Option<Color> {
        let s = self.bytes.get(self.pos..self.pos + 4)?;
        self.pos += 4;
        Some(Color {
            r: s[0],
            g: s[1],
            b: s[2],
            a: s[3],
        })
    }
    fn rect(&mut self) -> Option<Rect> {
        Some(Rect {
            origin: Point {
                x: self.f32()?,
                y: self.f32()?,
            },
            size: Size {
                w: self.f32()?,
                h: self.f32()?,
            },
        })
    }
    fn point(&mut self) -> Option<Point> {
        Some(Point {
            x: self.f32()?,
            y: self.f32()?,
        })
    }
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let s = self.bytes.get(self.pos..self.pos + n)?;
        self.pos += n;
        Some(s)
    }
}

/// A decoded display-list frame: the viewport it was built for plus the items.
/// (`DisplayItem` is not `PartialEq`; compare by re-serializing — `serialize` is the inverse of
/// `deserialize`, so equal bytes mean equal scenes.)
#[derive(Clone, Debug)]
pub struct Scene {
    /// Wire-format version.
    pub version: u16,
    /// Viewport width the scene was laid out for.
    pub width: u32,
    /// Viewport height the scene was laid out for.
    pub height: u32,
    /// The display items, in back-to-front paint order.
    pub items: Vec<DisplayItem>,
}

/// Decode the wire bytes back into a [`Scene`] (round-trips [`serialize`]). Returns `None` on a
/// truncated/malformed buffer or an unknown item tag. Mainly for tests + a Rust consumer that
/// wants to validate a C-side encoder against the Rust one.
#[must_use]
pub fn deserialize(bytes: &[u8]) -> Option<Scene> {
    let mut r = Reader { bytes, pos: 0 };
    let version = r.u16()?;
    let width = r.u32()?;
    let height = r.u32()?;
    let count = r.u32()?;
    let mut items = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let tag = r.u8()?;
        let item = match tag {
            DL_RECT => DisplayItem::Rect {
                rect: r.rect()?,
                color: r.color()?,
                radius: r.f32()?,
            },
            DL_GLYPHS => {
                let color = r.color()?;
                let n = r.u32()?;
                let mut glyphs = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    glyphs.push(Glyph {
                        id: r.u32()?,
                        x: r.f32()?,
                        y: r.f32()?,
                    });
                }
                DisplayItem::Glyphs {
                    glyphs: ShapedGlyphs { glyphs },
                    color,
                }
            }
            DL_TEXT => {
                let origin = r.point()?;
                let color = r.color()?;
                let size = r.f32()?;
                let box_w = r.f32()?;
                let align = r.f32()?;
                let len = r.u32()? as usize;
                let text = core::str::from_utf8(r.take(len)?).ok()?.into();
                DisplayItem::Text {
                    origin,
                    text,
                    color,
                    size,
                    box_w,
                    align,
                }
            }
            DL_BORDER => DisplayItem::Border {
                rect: r.rect()?,
                color: r.color()?,
                width: r.f32()?,
                radius: r.f32()?,
            },
            DL_GRADIENT => {
                let rect = r.rect()?;
                let direction = match r.u8()? {
                    DL_DIR_HORIZONTAL => GradientDirection::Horizontal,
                    _ => GradientDirection::Vertical,
                };
                let n = r.u8()?;
                let mut stops = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    stops.push(GradientStop {
                        color: r.color()?,
                        position: r.f32()?,
                    });
                }
                DisplayItem::Gradient {
                    rect,
                    stops: GradientStops::from_slice(&stops),
                    direction,
                }
            }
            DL_SHADOW => DisplayItem::Shadow {
                rect: r.rect()?,
                color: r.color()?,
                blur: r.f32()?,
                offset: r.point()?,
            },
            DL_PUSH_CLIP => DisplayItem::PushClip {
                rect: r.rect()?,
                radius: r.f32()?,
            },
            DL_POP_CLIP => DisplayItem::PopClip,
            _ => return None,
        };
        items.push(item);
    }
    Some(Scene {
        version,
        width,
        height,
        items,
    })
}
