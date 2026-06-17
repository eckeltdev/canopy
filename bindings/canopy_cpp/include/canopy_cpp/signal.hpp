#pragma once

#include <cstdint>
#include <utility>

// The fine-grained reactivity SURFACE. `signal<T>` is the state cell user components hold;
// the DSL reads it inside a reactive closure (e.g. `text([&]{ return label(); })`). In P1
// the contract — get / set / version — is frozen so components author against a stable API;
// the runtime BODY (subscription tracking on get, dirty propagation + targeted re-emit on
// set) lands in P4 without changing a single factory or component signature.
namespace canopy {

    template <class T> class signal {
    public:
        explicit signal(T value) : value_(std::move(value)) {}

        // Read the current value. P4 will additionally record a subscription when read inside
        // a running effect; today it is a plain read, so resolving a reactive slot ONCE during
        // the P1 mount walk is byte-identical to a static value.
        [[nodiscard]] auto get() const -> const T& {
            return value_;
        }

        // Replace the value and bump the version. P4 will additionally mark dependents dirty
        // and flush a targeted diff op; today it just updates state.
        void set(T value) {
            value_ = std::move(value);
            ++version_;
        }

        // Monotonic change counter — lets a reactive slot detect staleness before the full
        // effect graph exists.
        [[nodiscard]] auto version() const noexcept -> std::uint64_t {
            return version_;
        }

    private:
        T value_;
        std::uint64_t version_ = 0;
    };

} // namespace canopy
