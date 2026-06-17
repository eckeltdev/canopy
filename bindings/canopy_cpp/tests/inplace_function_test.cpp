#include <iostream>
#include <utility>

#include "canopy_cpp/inplace_function.hpp"

// Locks the manual-lifetime contract of inplace_function: copy yields two live callables,
// move empties the source, self-assignment is a no-op (the this==&other guards), and an empty
// instance stays empty through copy/move. These paths run in production (a click_handler is
// copy-constructed into add_listener by value, then move-assigned into the handler table), so
// a regression in the vtable/reset logic would otherwise pass the suite silently.
namespace {

    using callable = canopy::inplace_function<void()>;

    bool copy_yields_two_live_callables() {
        int count = 0;
        callable first{[&count] { count += 1; }};
        // The copy IS the subject under test, not an avoidable one.
        // NOLINTNEXTLINE(performance-unnecessary-copy-initialization)
        callable second = first;
        first();
        second();
        if (count != 2) {
            std::cerr << "FAIL: copy not independent-live (count=" << count << ")\n";
            return false;
        }
        return true;
    }

    bool move_leaves_source_empty() {
        int count = 0;
        callable source{[&count] { count += 1; }};
        callable sink = std::move(source);
        if (static_cast<bool>(source)) {
            std::cerr << "FAIL: moved-from source not empty\n";
            return false;
        }
        if (!static_cast<bool>(sink)) {
            std::cerr << "FAIL: move sink empty\n";
            return false;
        }
        sink();
        if (count != 1) {
            std::cerr << "FAIL: moved callable did not run (count=" << count << ")\n";
            return false;
        }
        return true;
    }

    bool self_assignment_preserves_the_callable() {
        int count = 0;
        callable handler{[&count] { count += 1; }};
        callable* alias = &handler;
        handler = *alias;            // self copy-assign (guarded)
        handler = std::move(*alias); // self move-assign (guarded)
        if (!static_cast<bool>(handler)) {
            std::cerr << "FAIL: self-assignment emptied the handler\n";
            return false;
        }
        handler();
        if (count != 1) {
            std::cerr << "FAIL: self-assigned callable did not run (count=" << count << ")\n";
            return false;
        }
        return true;
    }

    bool empty_stays_empty_through_copy_and_move() {
        callable empty;
        if (static_cast<bool>(empty)) {
            std::cerr << "FAIL: default-constructed not empty\n";
            return false;
        }
        callable copied = empty;
        callable moved = std::move(empty);
        if (static_cast<bool>(copied) || static_cast<bool>(moved)) {
            std::cerr << "FAIL: copy/move of empty became non-empty\n";
            return false;
        }
        return true;
    }

} // namespace

int main() {
    const bool all_passed = copy_yields_two_live_callables() && move_leaves_source_empty() &&
                            self_assignment_preserves_the_callable() &&
                            empty_stays_empty_through_copy_and_move();
    if (all_passed) {
        std::cerr << "ok: all inplace_function tests passed\n";
        return 0;
    }
    return 1;
}
