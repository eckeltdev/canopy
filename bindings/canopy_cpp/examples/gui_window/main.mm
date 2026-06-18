// A LIVE, CLICKABLE Canopy window — the C++/frt authoring stack driving a real native window.
//
// The UI is authored with the canopy_cpp DSL (normal std on the freestanding frt runtime), applied
// to the real Canopy engine, and the engine's lite layout + software rasterizer produce an RGBA8
// framebuffer that this file blits into an AppKit NSView each frame. Mouse clicks are hit-tested by
// the engine, the matching C++ on_click closure is fired through host::pump, the reactive runtime
// flushes a surgical SetText, and the window redraws — so the buttons actually count.
//
// winit/softbuffer are Rust crates and can't reach the C++ handler closures, so the window here is
// AppKit (Objective-C++, links -framework Cocoa) — zero extra dependencies, the C++ side owns the
// loop and uses the existing host::render_rgba / host::pointer / host::pump directly.
//
// A second piece of state animates with NO user input: an `NSTimer` in main() ticks `counter_app`
// ~12x/s, each tick `set`s a `ticks` signal under the runtime scope and flushes one surgical
// SetText (exactly the reactive sequence proven in tests/reactive_test.cpp — there is no pump for a
// timer tick), so the "ticks" readout climbs on its own while the +/- counter stays click-driven.
//
// Build: ./build.sh   (needs `cargo build -p canopy-abi` for libcanopy_abi.a; build.sh does it).
// Run:   ./canopy_cpp_window            -> opens the window
//        ./canopy_cpp_window --selftest -> headless: writes frame_before/after.ppm around a click
#import <Cocoa/Cocoa.h>

#include <cstddef>
#include <cstdint>
#include <fstream>
#include <string>
#include <vector>

#include "canopy.h"

#include "canopy_cpp/dsl.hpp"
#include "canopy_cpp/host.hpp"
#include "canopy_cpp/reactive.hpp"
#include "canopy_cpp/signal.hpp"

namespace {

    constexpr std::uint32_t kViewW = 480;
    constexpr std::uint32_t kViewH = 420;        // taller than the counter-only card to fit the readout
    constexpr std::uint16_t kClick = 1;          // EventKind CLICK (canopy_protocol)
    constexpr double kTickHz = 12.0;             // animation rate for the timer-driven readout

    // The C++ Canopy application: an interactive counter card. State lives in a signal; the count
    // text is a reactive binding, so a button's on_click only has to `set` the signal and the
    // runtime emits a targeted SetText on flush — no rebuild.
    struct counter_app {
        canopy::signal<int> count{0};
        canopy::signal<int> ticks{0}; // timer-driven, climbs with no user input (tenths of a second)
        canopy::build_context ctx;
        canopy::host engine;
        std::uint32_t seq = 0;

        counter_app() {
            using namespace canopy; // DSL factories — a .mm, not a header
            namespace wire = canopy::wire;

            // Inline-styled so the lite engine has geometry (for layout AND hit-testing). The
            // counter row is `[ - ] [ count ] [ + ]`; the two buttons carry CLICK handlers.
            mount(
                ctx,
                div( // screen
                    style(wire::prop_width, "480"), style(wire::prop_height, "420"),
                    style(wire::prop_bg, "#1e1e2e"), style(wire::prop_padding, "24"),
                    style(wire::prop_direction, "column"),
                    div( // card
                        style(wire::prop_width, "432"), style(wire::prop_height, "372"),
                        style(wire::prop_bg, "#313244"), style(wire::prop_radius, "16"),
                        style(wire::prop_padding, "20"), style(wire::prop_direction, "column"),
                        style(wire::prop_gap, "16"), style(wire::prop_fg, "#cdd6f4"),
                        text("Canopy counter - C++ on frt"),
                        row( // counter row: [-] [count] [+]
                            style(wire::prop_width, "376"), style(wire::prop_height, "72"),
                            style(wire::prop_direction, "row"), style(wire::prop_gap, "16"),
                            button(style(wire::prop_width, "80"), style(wire::prop_height, "72"),
                                   style(wire::prop_bg, "#f38ba8"), style(wire::prop_radius, "12"),
                                   style(wire::prop_fg, "#11111b"),
                                   style(wire::prop_align, "center"), style(wire::prop_justify, "center"),
                                   on_click([this] { count.set(count.get() - 1); }), "-"),
                            div( // count readout (reactive text), centered both axes
                                style(wire::prop_width, "184"), style(wire::prop_height, "72"),
                                style(wire::prop_bg, "#45475a"), style(wire::prop_radius, "12"),
                                style(wire::prop_align, "center"), style(wire::prop_justify, "center"),
                                style(wire::prop_fg, "#f9e2af"),
                                text([this] { return std::to_string(count.get()); })),
                            button(style(wire::prop_width, "80"), style(wire::prop_height, "72"),
                                   style(wire::prop_bg, "#a6e3a1"), style(wire::prop_radius, "12"),
                                   style(wire::prop_fg, "#11111b"),
                                   style(wire::prop_align, "center"), style(wire::prop_justify, "center"),
                                   on_click([this] { count.set(count.get() + 1); }), "+")),
                        text("click + / - to count"),
                        text("auto-ticks (no input):"),
                        row( // animated readout: a timer ticks this with no user input
                            style(wire::prop_width, "376"), style(wire::prop_height, "56"),
                            style(wire::prop_direction, "row"), style(wire::prop_gap, "16"),
                            style(wire::prop_fg, "#cdd6f4"),
                            div( // animated value readout (reactive text, timer-driven), centered
                                style(wire::prop_width, "376"), style(wire::prop_height, "56"),
                                style(wire::prop_bg, "#585b70"), style(wire::prop_radius, "12"),
                                style(wire::prop_align, "center"), style(wire::prop_justify, "center"),
                                style(wire::prop_fg, "#89dceb"),
                                text([this] { return std::to_string(ticks.get()); }))))));

            engine.apply(ctx.take_batch(seq++));
            engine.resize(static_cast<float>(kViewW), static_cast<float>(kViewH));
        }

        // Deliver a click in canopy pixel space. Returns true if it changed the UI (redraw needed).
        bool click(double pos_x, double pos_y) {
            engine.pointer(static_cast<float>(pos_x), static_cast<float>(pos_y), 0, kClick);
            if (engine.pump(ctx) <= 0) {
                return false; // missed every handler
            }
            // The handler ran under pump's runtime scope and marked the count binding dirty; flush
            // it (under the runtime) into one SetText, then apply that surgical batch to the host.
            {
                const canopy::active_runtime_scope scope(&ctx.runtime());
                ctx.flush();
            }
            const std::vector<std::uint8_t> update = ctx.take_batch(seq++);
            engine.apply(update);
            return true;
        }

        // Advance the timer-driven animation by one frame. There is no pump for a timer tick (no
        // event), so we drive the proven reactive sequence directly: under the runtime scope, `set`
        // the signal (marking the bound readout dirty) and `flush` it into one surgical SetText,
        // then apply that batch — exactly the manual path in tests/reactive_test.cpp.
        void tick() {
            {
                const canopy::active_runtime_scope scope(&ctx.runtime());
                ticks.set(ticks.get() + 1);
                ctx.flush();
            }
            engine.apply(ctx.take_batch(seq++));
        }

        std::vector<std::uint8_t> frame() { return engine.render_rgba(kViewW, kViewH); }
    };

    void write_ppm(const std::string& path, const std::vector<std::uint8_t>& rgba) {
        std::ofstream out(path, std::ios::binary);
        out << "P6\n" << kViewW << ' ' << kViewH << "\n255\n";
        for (std::size_t idx = 0; idx + 4 <= rgba.size(); idx += 4) {
            out.put(static_cast<char>(rgba[idx]));
            out.put(static_cast<char>(rgba[idx + 1]));
            out.put(static_cast<char>(rgba[idx + 2]));
        }
    }

    // Headless proof of BOTH loops: the click loop (render, click "+", render) AND the timer-driven
    // animation loop (several tick()s climbing the readout with no user input).
    int run_selftest() {
        counter_app app;
        write_ppm("frame_before.ppm", app.frame());
        const bool hit = app.click(380.0, 112.0); // center of the "+" button (see the layout above)
        write_ppm("frame_after.ppm", app.frame());

        // Drive the animation: N ticks must advance the readout signal 0 -> N with no input.
        constexpr int kTicks = 5;
        for (int step = 0; step < kTicks; ++step) {
            app.tick();
        }

        const bool click_ok = hit && app.count.get() == 1;
        const bool anim_ok = app.ticks.get() == kTicks;
        std::printf("selftest: click hit a handler = %s; count now = %d; ticks now = %d (want %d)\n",
                    hit ? "yes" : "no", app.count.get(), app.ticks.get(), kTicks);
        return (click_ok && anim_ok) ? 0 : 1;
    }

} // namespace

// --- the AppKit window -----------------------------------------------------------------------

@interface CanopyView : NSView {
    counter_app* app_;                 // owned elsewhere (main); borrowed here
    std::vector<std::uint8_t>* frame_; // kept alive across drawRect for the NSBitmapImageRep
}
- (instancetype)initWithFrame:(NSRect)frame app:(counter_app*)app;
@end

@implementation CanopyView

- (instancetype)initWithFrame:(NSRect)frame app:(counter_app*)app {
    self = [super initWithFrame:frame];
    if (self != nil) {
        app_ = app;
        frame_ = new std::vector<std::uint8_t>();
    }
    return self;
}

// Top-left origin, matching Canopy's pixel space, so mouse y and the blit agree.
- (BOOL)isFlipped {
    return YES;
}

- (void)drawRect:(NSRect)dirtyRect {
    (void)dirtyRect;
    *frame_ = app_->frame();
    unsigned char* bytes = frame_->data();
    NSBitmapImageRep* rep =
        [[NSBitmapImageRep alloc] initWithBitmapDataPlanes:&bytes
                                                pixelsWide:kViewW
                                                pixelsHigh:kViewH
                                             bitsPerSample:8
                                           samplesPerPixel:4
                                                  hasAlpha:YES
                                                  isPlanar:NO
                                            colorSpaceName:NSDeviceRGBColorSpace
                                               bytesPerRow:kViewW * 4
                                              bitsPerPixel:32];
    // respectFlipped:YES draws the top-left-origin framebuffer top-down in this isFlipped view,
    // so the image can't come out upside-down regardless of the view's coordinate system.
    [rep drawInRect:self.bounds
           fromRect:NSZeroRect
          operation:NSCompositingOperationCopy
           fraction:1.0
     respectFlipped:YES
              hints:nil];
}

- (void)mouseDown:(NSEvent*)event {
    const NSPoint local = [self convertPoint:event.locationInWindow fromView:nil];
    // The content view is a fixed kViewW x kViewH, so local coords ARE canopy pixels (flipped Y).
    if (app_->click(local.x, local.y)) {
        [self setNeedsDisplay:YES];
    }
}

@end

int main(int argc, const char* argv[]) {
    if (argc > 1 && std::string(argv[1]) == "--selftest") {
        return run_selftest();
    }

    // --shot <out.png>: render the REAL AppKit view (drawRect -> NSBitmapImageRep -> flipped draw)
    // offscreen to a PNG. Verifies the blit path (orientation + color) without a visible window.
    if (argc > 2 && std::string(argv[1]) == "--shot") {
        @autoreleasepool {
            [NSApplication sharedApplication];
            auto* app = new counter_app();
            const NSRect frame = NSMakeRect(0, 0, kViewW, kViewH);
            NSWindow* window = [[NSWindow alloc] initWithContentRect:frame
                                                          styleMask:NSWindowStyleMaskBorderless
                                                            backing:NSBackingStoreBuffered
                                                              defer:NO];
            CanopyView* view = [[CanopyView alloc] initWithFrame:frame app:app];
            [window setContentView:view];
            NSBitmapImageRep* rep = [view bitmapImageRepForCachingDisplayInRect:view.bounds];
            [view cacheDisplayInRect:view.bounds toBitmapImageRep:rep];
            NSData* png = [rep representationUsingType:NSBitmapImageFileTypePNG properties:@{}];
            [png writeToFile:[NSString stringWithUTF8String:argv[2]] atomically:YES];
        }
        return 0;
    }

    @autoreleasepool {
        [NSApplication sharedApplication];
        [NSApp setActivationPolicy:NSApplicationActivationPolicyRegular];

        auto* app = new counter_app(); // lives for the process; the window borrows it
        const NSRect frame = NSMakeRect(0, 0, kViewW, kViewH);
        NSWindow* window =
            [[NSWindow alloc] initWithContentRect:frame
                                        styleMask:(NSWindowStyleMaskTitled | NSWindowStyleMaskClosable)
                                          backing:NSBackingStoreBuffered
                                            defer:NO];
        [window setTitle:@"Canopy - C++ on frt"];
        CanopyView* view = [[CanopyView alloc] initWithFrame:frame app:app];
        [window setContentView:view];
        [window center];
        [window makeKeyAndOrderFront:nil];
        [NSApp activateIgnoringOtherApps:YES];

        // Continuous animation with NO user input: a repeating timer ticks the app ~kTickHz times a
        // second. Each tick mutates the `ticks` signal and flushes one surgical SetText (see
        // counter_app::tick), then we mark the view dirty so the readout redraws on its own.
        [NSTimer scheduledTimerWithTimeInterval:1.0 / kTickHz
                                        repeats:YES
                                          block:^(NSTimer* timer) {
                                            (void)timer;
                                            app->tick();
                                            [view setNeedsDisplay:YES];
                                          }];

        [NSApp run];
    }
    return 0;
}
