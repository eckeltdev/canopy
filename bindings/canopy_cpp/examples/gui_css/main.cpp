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
// the host runs the real lite cascade (selectors + specificity + inheritance + custom properties)
// and folds the matched declarations onto each node before laying out + software-rasterizing to
// pixels. The retained tree stays identity-only (the cascade is non-destructive). Build the engine
// staticlib first: `cargo build -p canopy-abi`. Writes canopy_cpp_css.ppm +
// canopy_cpp_css_hover.ppm.
namespace {

    constexpr std::uint32_t view_w = 480;
    constexpr std::uint32_t view_h = 360;

    // The stylesheet exercises the FULL lite CSS engine, all from this string — the C++ below sets
    // no inline styles, only ids and classes. It showcases: custom properties (`--accent`) reused
    // via var() and inherited down the tree; a `linear-gradient` background; a soft `box-shadow`; a
    // real CSS `grid` for the button row; `font-size`/`font-weight`/`text-align` (inherited to the
    // text); per-side `margin`; an `outline`; named/#hex/rgba() colors; and the selector variety
    // (type / id / class / compound + :hover) resolved with specificity and anti-aliased corners.
    constexpr const char* stylesheet =
        // Design tokens: custom properties declared on the frame inherit to every descendant, then
        // get pulled in with var(). Change one value here and the whole UI re-themes.
        "#screen { --accent: #89b4fa; --danger: #f38ba8; --text: #cdd6f4;"
        "          width: 480; height: 360; padding: 32; direction: column;"
        "          align: center; justify: center;"
        "          background-image: linear-gradient(to bottom, #1e1e2e, #11111b) }"
        // The card: a shadowed, rounded panel. box-shadow + var() + gap.
        "#card { width: 400; background: #313244; radius: 16; padding: 24; direction: column;"
        "        gap: 16; color: var(--text); box-shadow: 0 10 28 #00000088 }"
        // Inherited text traits: font-size/weight/align flow down to the text node inside.
        ".title { font-size: 26; font-weight: bold; text-align: center }"
        // A real CSS GRID: two equal columns for the action buttons.
        ".actions { display: grid; grid-template-columns: repeat(2, 1fr); gap: 14 }"
        // Type selector: every <button> shares geometry + a translucent border (rgba -> #rrggbbaa).
        "button { height: 48; radius: 10; color: #11111b; border-width: 2;"
        "         border-color: rgba(0, 0, 0, 0.25); text-align: center }"
        // Compound + var(): variant color from a token; :hover lightens (anti-aliased, end to end).
        "button.primary       { background: var(--accent) }"
        "button.primary:hover { background: #b4caff }"
        "button.danger        { background: var(--danger) }"
        "button.danger:hover  { background: #f8b0c4 }"
        // Status bar: a top margin (per-side shorthand) and an outline ring tinted by the token.
        ".status { background: #45475a; radius: 8; padding: 12; color: #a6e3a1; text-align: center;"
        "          margin: 6 0 0 0; outline-width: 2; outline-color: var(--accent);"
        "          outline-offset: 3 }";

    // Author the tree with identity only — ids on the singletons, classes on the rest. The two
    // buttons sit in a CSS grid; everything else is flex. No inline styles anywhere.
    void build_app(canopy::build_context& ctx) {
        using namespace canopy; // DSL factories — a .cpp, not a header
        mount(ctx, div(id("screen"),
                       div(id("card"), div(cls("title"), text("Canopy")),
                           div(cls("actions"), button(cls("primary"), on_click([] {}), "Open"),
                               button(cls("danger"), on_click([] {}), "Close")),
                           div(cls("status"), text("grid - gradient - shadow - var()")))));
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
    engine.resize(static_cast<float>(view_w), static_cast<float>(view_h)); // viewport for hover

    // 3. Render: the host runs the cascade (selectors, inheritance, var(), grid), lays out, and
    //    rasterizes to RGBA8.
    const std::vector<std::uint8_t> base = engine.render_rgba(view_w, view_h);
    if (base.empty()) {
        std::cerr << "render failed (empty framebuffer)\n";
        return 1;
    }
    write_ppm("canopy_cpp_css.ppm", base);

    // 4. Move the pointer over the "Open" button and re-render: the `button.primary:hover` rule
    //    lightens it. Proves :hover end to end on the gradient/grid/shadow scene.
    engine.hover(148.0F, 172.0F);
    write_ppm("canopy_cpp_css_hover.ppm", engine.render_rgba(view_w, view_h));

    std::cout << "canopy C++ (identity only) + CSS stylesheet -> canopy_cpp_css.ppm"
              << " + canopy_cpp_css_hover.ppm (Open hovered) (" << engine.node_count()
              << " nodes)\n";
    return 0;
}
