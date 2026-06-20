#include <cstddef>
#include <cstdint>
#include <fstream>
#include <iostream>
#include <string>
#include <vector>

#include "canopy_cpp/dsl.hpp"
#include "canopy_cpp/host.hpp"

// "A basic app, styled with CSS, as a freestanding lib." The UI STRUCTURE is authored in C++ on
// the frt runtime with the DSL — but it carries only IDENTITY (id(...) / cls(...)), no inline
// styles. All styling lives in a CSS-lite stylesheet handed to the engine via host::set_stylesheet;
// the host runs the real lite selector cascade (type / id / class / compound, with specificity) and
// folds the matched declarations onto each node before laying out + software-rasterizing to pixels.
// The retained tree stays identity-only (the cascade is non-destructive). Build the engine
// staticlib first: `cargo build -p canopy-abi`. Writes canopy_cpp_css.ppm +
// canopy_cpp_css_hover.ppm.
namespace {

    constexpr std::uint32_t view_w = 480;
    constexpr std::uint32_t view_h = 320;

    // The stylesheet exercises the full lite selector engine. Every box's geometry AND color comes
    // from here — the C++ below sets no inline styles, only ids and classes. `color` inherits to
    // text. Note the selector variety (type / id / class / compound + :hover) and the three color
    // spellings the engine normalizes to #rrggbb: a named keyword, `#rgb`, and `rgb(r, g, b)`.
    constexpr const char* stylesheet =
        // Type selector: EVERY <button> shares geometry, a 2px border, and flex-grow — no class
        // needed. `flex-grow` lets the two buttons split the row; `border-*` is a new lite prop.
        "button { height: 56; radius: 10; flex-grow: 1; color: black;"
        "         border-width: 2; border-color: #222 }"
        // Id selectors (authored with id(...)) for the singleton frame + card. `navy` is a named
        // color; `rgb(49, 50, 68)` and `margin` (centring the card in the frame's padding) are new.
        "#screen { width: 480; height: 320; background: navy; padding: 32; direction: column }"
        "#card   { width: 400; height: 240; margin: 8; background: rgb(49, 50, 68); radius: 16;"
        "          padding: 24; direction: column; gap: 12; color: #cdd6f4 }"
        // Class selectors for the layout rows; `min-width` is a new sizing prop.
        ".bar    { width: 352; height: 56; direction: row; gap: 16 }"
        ".status { width: 352; height: 44; background: #45475a; radius: 8; padding: 14;"
        "          color: #a6e3a1; min-width: 320 }"
        // Compound selectors: a rule that fires only when the node is a <button> AND carries the
        // class. Specificity (type+class=11) beats the bare `button` rule, so these win the color.
        "button.primary       { background: #89b4fa }"
        "button.primary:hover { background: #b4caff }" // lighter on hover (pointer over the button)
        "button.danger        { background: #f38ba8 }"
        "button.danger:hover  { background: #f8b0c4 }";

    // Author the tree with identity only — ids on the singletons, classes on the rest, no inline
    // styles. The buttons carry ONLY their variant class; the shared `button` type rule does the
    // rest. Buttons auto-center their labels.
    void build_app(canopy::build_context& ctx) {
        using namespace canopy; // DSL factories — a .cpp, not a header
        mount(ctx,
              div(id("screen"), div(id("card"), text("Canopy - styled with CSS"),
                                    div(cls("bar"), button(cls("primary"), on_click([] {}), "Run"),
                                        button(cls("danger"), on_click([] {}), "Stop")),
                                    div(cls("status"), text("status: ready")))));
    }

    // Encode a row-major RGBA8 framebuffer as a binary PPM (P6); the alpha byte is dropped.
    void write_ppm(const std::string& path, const std::vector<std::uint8_t>& rgba) {
        std::ofstream out(path, std::ios::binary);
        out << "P6\n" << view_w << ' ' << view_h << "\n255\n";
        for (std::size_t idx = 0; idx + 4 <= rgba.size(); idx += 4) {
            out.put(static_cast<char>(rgba[idx]));
            out.put(static_cast<char>(rgba[idx + 1]));
            out.put(static_cast<char>(rgba[idx + 2]));
        }
    }

} // namespace

int main() {
    // 1. Author the structure (identity only) with the C++ DSL on frt.
    canopy::build_context ctx;
    build_app(ctx);

    // 2. Hand the engine the stylesheet, then apply the identity-only op-stream.
    canopy::host engine;
    engine.set_stylesheet(stylesheet);
    engine.apply(ctx.take_batch(0));
    engine.resize(static_cast<float>(view_w),
                  static_cast<float>(view_h)); // viewport for hover hit-test

    // 3. Render: the host runs the selector cascade, lays out, and rasterizes to RGBA8.
    const std::vector<std::uint8_t> base = engine.render_rgba(view_w, view_h);
    if (base.empty()) {
        std::cerr << "render failed (empty framebuffer)\n";
        return 1;
    }
    write_ppm("canopy_cpp_css.ppm", base);

    // 4. Move the pointer over the "Run" button (its center) and re-render: the
    //    `button.primary:hover` rule lightens it. Proves `:hover` end to end.
    engine.hover(148.0F, 124.0F);
    write_ppm("canopy_cpp_css_hover.ppm", engine.render_rgba(view_w, view_h));

    std::cout << "canopy C++ (identity only) + CSS stylesheet -> canopy_cpp_css.ppm"
              << " + canopy_cpp_css_hover.ppm (Run hovered) (" << engine.node_count()
              << " nodes)\n";
    return 0;
}
