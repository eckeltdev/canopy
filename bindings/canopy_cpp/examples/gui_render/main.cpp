#include <cstddef>
#include <cstdint>
#include <fstream>
#include <iostream>
#include <string>
#include <vector>

#include "canopy_cpp/dsl.hpp"
#include "canopy_cpp/host.hpp"

// The end-to-end Canopy C++ story, all the way to PIXELS: a GUI is authored with the builder DSL
// (normal std::string/vector/unique_ptr routed through the freestanding frt runtime), its op
// bytes are applied through the REAL Canopy engine (canopy-abi), and the engine's lite layout +
// software rasterizer turn the retained tree into an RGBA8 framebuffer we save as a PPM image.
//
// Build the engine staticlib first: `cargo build -p canopy-abi`. Run: `./canopy_cpp_gui_example`
// writes canopy_cpp_gui.ppm in the working directory.
namespace {

    constexpr std::uint32_t view_w = 480;
    constexpr std::uint32_t view_h = 320;

    // Author the GUI tree with the DSL. Every node is inline-styled (the geometry + colors the
    // lite engine reads): a dark screen holding a rounded card with a title, a two-button row, and
    // a status bar. `wire::prop_*` are the well-known style ids; colors are #rrggbb, sizes are px.
    void build_gui(canopy::build_context& ctx) {
        using namespace canopy; // DSL factories (div/row/button/text/style/...) — a .cpp, not a header
        namespace wire = canopy::wire;

        mount(
            ctx,
            div( // the screen
                style(wire::prop_width, "480"), style(wire::prop_height, "320"),
                style(wire::prop_bg, "#1e1e2e"), style(wire::prop_padding, "32"),
                style(wire::prop_direction, "column"),
                div( // the card
                    style(wire::prop_width, "416"), style(wire::prop_height, "256"),
                    style(wire::prop_bg, "#313244"), style(wire::prop_radius, "16"),
                    style(wire::prop_padding, "24"), style(wire::prop_direction, "column"),
                    style(wire::prop_gap, "18"), style(wire::prop_fg, "#cdd6f4"),
                    text("Canopy - C++ on frt"),
                    row( // the button row
                        style(wire::prop_height, "56"), style(wire::prop_direction, "row"),
                        style(wire::prop_gap, "16"),
                        button(style(wire::prop_width, "180"), style(wire::prop_height, "56"),
                               style(wire::prop_bg, "#89b4fa"), style(wire::prop_radius, "10"),
                               style(wire::prop_padding, "20"), style(wire::prop_fg, "#11111b"),
                               on_click([] {}), "Run"),
                        button(style(wire::prop_width, "180"), style(wire::prop_height, "56"),
                               style(wire::prop_bg, "#f38ba8"), style(wire::prop_radius, "10"),
                               style(wire::prop_padding, "20"), style(wire::prop_fg, "#11111b"),
                               on_click([] {}), "Stop")),
                    div( // the status bar
                        style(wire::prop_width, "368"), style(wire::prop_height, "48"),
                        style(wire::prop_bg, "#45475a"), style(wire::prop_radius, "8"),
                        style(wire::prop_padding, "16"), style(wire::prop_fg, "#a6e3a1"),
                        text("status: ready")))));
    }

    // Encode a row-major RGBA8 framebuffer as a binary PPM (P6); the alpha byte is dropped.
    void write_ppm(const std::string& path, const std::vector<std::uint8_t>& rgba,
                   std::uint32_t width, std::uint32_t height) {
        std::ofstream out(path, std::ios::binary);
        out << "P6\n" << width << ' ' << height << "\n255\n";
        for (std::size_t idx = 0; idx + 4 <= rgba.size(); idx += 4) {
            out.put(static_cast<char>(rgba[idx]));
            out.put(static_cast<char>(rgba[idx + 1]));
            out.put(static_cast<char>(rgba[idx + 2]));
        }
    }

} // namespace

int main() {
    const std::string path = "canopy_cpp_gui.ppm";

    // 1. Author the UI with the C++ DSL (its std containers allocate through frt's seam).
    canopy::build_context ctx;
    build_gui(ctx);

    // 2. Apply the authored op-stream through the REAL Canopy engine.
    canopy::host engine;
    engine.apply(ctx.take_batch(0));

    // 3. Render the engine's retained tree to an RGBA8 framebuffer (lite layout + soft raster).
    const std::vector<std::uint8_t> rgba = engine.render_rgba(view_w, view_h);
    if (rgba.empty()) {
        std::cerr << "render failed (empty framebuffer)\n";
        return 1;
    }

    // 4. Save it as an image you can open.
    write_ppm(path, rgba, view_w, view_h);
    std::cout << "canopy C++ DSL on frt -> real engine -> " << view_w << 'x' << view_h
              << " RGBA -> " << path << " (" << engine.node_count() << " nodes)\n";
    return 0;
}
