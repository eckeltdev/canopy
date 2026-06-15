//! Plugin-panel compositing: render a plugin's retained tree into a sub-region of a
//! host frame buffer.
//!
//! The point is **isolation by compositing**. An untrusted plugin builds its own
//! [`canopy_dom::Dom`] in its own coordinate space (the orchestrator typically gets
//! one from [`canopy_transport_wasmtime::PluginHost::dom`], but this crate takes a
//! plain `&Dom` and so never links wasmtime). The host lays that tree out for the
//! *panel's* viewport, rasterizes it into a throwaway [`SoftwareRenderer`], and blits
//! the finished sub-image into the host buffer at the panel's origin. Because the
//! plugin only ever sees a viewport sized to its panel and never the host's buffer,
//! it cannot address — or paint over — anything outside the region it was given.
//!
//! Two entry points:
//! - [`render_panel`] composites a plugin tree into an existing host [`Buffer`],
//!   clipped to the panel rect.
//! - [`render_panel_to_buffer`] rasterizes a plugin tree to a fresh, standalone
//!   [`Buffer`] (useful for tests, snapshots, or off-screen panels).
//!
//! This is a `std` host-side leaf crate: it orchestrates the `no_std` layout and
//! software-renderer crates but is itself free to use `std`.

use canopy_dom::Dom;
use canopy_layout_taffy::layout;
use canopy_render_soft::{Buffer, SoftwareRenderer};
use canopy_traits::{Color, Point, Rect, Renderer, Size};

/// Rasterize `plugin_dom` into a fresh [`Buffer`] of `region_size`, on a `clear`
/// background.
///
/// The plugin tree is laid out for a viewport of exactly `region_size` — its own
/// origin is `(0, 0)`, independent of wherever the host later places it — and painted
/// by a [`SoftwareRenderer`]. The returned buffer is the panel's pixels, ready to blit
/// or inspect. Use this for standalone/off-screen panels; [`render_panel`] wraps it to
/// composite into a larger host buffer.
pub fn render_panel_to_buffer(region_size: Size, clear: Color, plugin_dom: &Dom) -> Buffer {
    let w = region_size.w.max(0.0) as usize;
    let h = region_size.h.max(0.0) as usize;
    let mut renderer = SoftwareRenderer::new(w, h, clear);
    let (scene, _layout) = layout(plugin_dom, region_size);
    // The software renderer cannot fail (it only fills an owned buffer), so the
    // `Result` is infallible here; surfacing it would only add noise to the API.
    let _ = renderer.render(&scene);
    // Move the rendered buffer out of the renderer.
    let src = renderer.buffer();
    let mut out = Buffer::new(w, h);
    copy_into(&mut out, src, 0, 0);
    out
}

/// Composite `plugin_dom` into `target` at `region.origin`, clipped to `region`.
///
/// `plugin_dom` is laid out and rasterized for a viewport of `region.size` (see
/// [`render_panel_to_buffer`]), then blitted into `target` so that the panel's pixel
/// `(0, 0)` lands at `region.origin`. Any panel pixel that would fall outside the
/// panel rect — or outside `target` — is dropped, so the plugin's paint is strictly
/// confined to its region.
///
/// The host's existing pixels outside the region are left untouched. The panel's own
/// clear color is transparent black, so the plugin's backgrounds (if any) show through;
/// pixels the plugin leaves clear overwrite the host with that transparent black, which
/// is the panel's defined backdrop.
pub fn render_panel(target: &mut Buffer, region: Rect, plugin_dom: &Dom) {
    let panel = render_panel_to_buffer(region.size, Color::default(), plugin_dom);
    let ox = region.origin.x.max(0.0) as usize;
    let oy = region.origin.y.max(0.0) as usize;
    copy_into(target, &panel, ox, oy);
}

/// Blit every pixel of `src` into `dst` with its top-left at `(dst_x, dst_y)`,
/// clipped to `dst`'s bounds.
///
/// Out-of-bounds destination pixels are skipped, so a panel placed near an edge — or
/// larger than the space left for it — is cropped cleanly rather than wrapping or
/// panicking. Each pixel is written as a 1x1 fill, the only pixel-level write the
/// public [`Buffer`] API exposes.
fn copy_into(dst: &mut Buffer, src: &Buffer, dst_x: usize, dst_y: usize) {
    let dw = dst.width();
    let dh = dst.height();
    for sy in 0..src.height() {
        let py = dst_y + sy;
        if py >= dh {
            break;
        }
        for sx in 0..src.width() {
            let px = dst_x + sx;
            if px >= dw {
                break;
            }
            let [r, g, b, a] = src.pixel(sx, sy);
            dst.fill_rect(
                Rect {
                    origin: Point {
                        x: px as f32,
                        y: py as f32,
                    },
                    size: Size { w: 1.0, h: 1.0 },
                },
                Color { r, g, b, a },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use canopy_core::Emitter;
    use canopy_dom::ROOT;
    use canopy_layout_taffy::layout as taffy_layout;
    use canopy_paint::{BG, HEIGHT, WIDTH};
    use canopy_protocol::ElementTag;
    use canopy_traits::OpSink;

    const COLUMN: ElementTag = ElementTag::new(1);

    /// Build a plugin Dom: a column filled with green (`#a6e3a1`) sized to fill the
    /// panel, holding a text leaf.
    fn green_panel_dom() -> Dom {
        let mut e = Emitter::new();
        let col = e.create_element(COLUMN);
        e.append(ROOT, col);
        e.set_inline_style(col, BG, "#a6e3a1");
        e.set_inline_style(col, WIDTH, "120");
        e.set_inline_style(col, HEIGHT, "60");
        let label = e.create_text("plugin");
        e.append(col, label);

        let mut dom = Dom::new();
        dom.apply(&e.take_batch(0)).unwrap();
        dom
    }

    #[test]
    fn composites_plugin_into_region_and_clips_outside() {
        let green = Color {
            r: 0xa6,
            g: 0xe3,
            b: 0xa1,
            a: 255,
        };
        let black = Color {
            r: 0,
            g: 0,
            b: 0,
            a: 255,
        };

        // A 200x120 host buffer, cleared opaque black.
        let mut host = Buffer::new(200, 120);
        host.clear(black);

        let plugin = green_panel_dom();
        let region = Rect {
            origin: Point { x: 40.0, y: 30.0 },
            size: Size { w: 120.0, h: 60.0 },
        };
        render_panel(&mut host, region, &plugin);

        // A pixel inside the region shows the panel's green background. Host (100, 70)
        // is panel-local (60, 40): inside the panel and well clear of the top-left text
        // glyphs, so it lands on plain green background, not ink.
        assert_eq!(
            host.pixel(100, 70),
            [green.r, green.g, green.b, green.a],
            "inside the region must be the plugin's green background",
        );

        // A pixel outside the region is still the host's black — clipping holds.
        assert_eq!(
            host.pixel(10, 10),
            [black.r, black.g, black.b, black.a],
            "outside the region must be untouched host black",
        );
        // Just past the panel's right edge is also still black.
        assert_eq!(
            host.pixel(170, 60),
            [black.r, black.g, black.b, black.a],
            "to the right of the panel must be untouched host black",
        );
    }

    #[test]
    fn standalone_buffer_has_panel_background() {
        let plugin = green_panel_dom();
        let buf = render_panel_to_buffer(Size { w: 120.0, h: 60.0 }, Color::default(), &plugin);
        assert_eq!(buf.width(), 120);
        assert_eq!(buf.height(), 60);
        // Away from the text in the top-left, the panel is solid green.
        assert_eq!(buf.pixel(60, 40), [0xa6, 0xe3, 0xa1, 255]);
    }

    #[test]
    fn plugin_coordinates_are_local_to_the_panel() {
        // The plugin lays out at its own origin (0,0); compositing offsets it. Prove
        // the standalone layout puts the column at (0,0) regardless of host placement.
        let plugin = green_panel_dom();
        let (_scene, lay) = taffy_layout(&plugin, Size { w: 120.0, h: 60.0 });
        let col_rect = lay.rects.first().expect("a laid-out node").1;
        assert_eq!(
            col_rect.origin,
            Point { x: 0.0, y: 0.0 },
            "the plugin's root is local to its own viewport",
        );
    }
}
