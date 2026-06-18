#include <cstddef>
#include <cstdint>
#include <fstream>
#include <iostream>
#include <string>
#include <vector>

#include "canopy_cpp/dsl.hpp"
#include "canopy_cpp/host.hpp"

// "A basic app, styled with CSS, as a freestanding lib." The UI STRUCTURE is authored in C++ on
// the frt runtime with the DSL — but it carries only CLASS names (cls(...)), no inline styles.
// All styling lives in a CSS-lite stylesheet handed to the engine via host::set_stylesheet; the
// host cascades each node's classes to styles before laying out + software-rasterizing to pixels.
// The retained tree stays class-only (the cascade is non-destructive). Build the engine staticlib
// first: `cargo build -p canopy-abi`. Writes canopy_cpp_css.ppm.
namespace {

    constexpr std::uint32_t view_w = 480;
    constexpr std::uint32_t view_h = 320;

    // The stylesheet: `.class { property: value; ... }`. Every box's geometry AND color comes from
    // here — the C++ below sets no inline styles, only class names. `color` inherits to text.
    constexpr const char* stylesheet =
        ".screen  { width: 480; height: 320; background: #1e1e2e; padding: 32; direction: column }"
        ".card    { width: 416; height: 256; background: #313244; radius: 16; padding: 24;"
        "           direction: column; gap: 16; color: #cdd6f4 }"
        ".bar     { width: 376; height: 56; direction: row; gap: 16 }"
        ".btn     { width: 180; height: 56; radius: 10; color: #11111b }"
        ".primary { background: #89b4fa }"
        ".danger  { background: #f38ba8 }"
        ".status  { width: 368; height: 44; background: #45475a; radius: 8; padding: 14;"
        "           color: #a6e3a1 }";

    // Author the tree with class names only — no inline styles. Buttons auto-center their labels.
    void build_app(canopy::build_context& ctx) {
        using namespace canopy; // DSL factories — a .cpp, not a header
        mount(ctx, div(cls("screen"),
                       div(cls("card"), text("Canopy - styled with CSS"),
                           div(cls("bar"),
                               button(cls("btn"), cls("primary"), on_click([] {}), "Run"),
                               button(cls("btn"), cls("danger"), on_click([] {}), "Stop")),
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
    const std::string path = "canopy_cpp_css.ppm";

    // 1. Author the structure (classes only) with the C++ DSL on frt.
    canopy::build_context ctx;
    build_app(ctx);

    // 2. Hand the engine the stylesheet, then apply the class-only op-stream.
    canopy::host engine;
    engine.set_stylesheet(stylesheet);
    engine.apply(ctx.take_batch(0));

    // 3. Render: the host cascades classes -> styles, lays out, and rasterizes to RGBA8.
    const std::vector<std::uint8_t> rgba = engine.render_rgba(view_w, view_h);
    if (rgba.empty()) {
        std::cerr << "render failed (empty framebuffer)\n";
        return 1;
    }

    // 4. Save the image.
    write_ppm(path, rgba);
    std::cout << "canopy C++ (classes only) + CSS stylesheet -> real engine -> " << view_w << 'x'
              << view_h << " RGBA -> " << path << " (" << engine.node_count() << " nodes)\n";
    return 0;
}
