#pragma once

#include <array>
#include <cstddef>
#include <memory>
#include <type_traits>
#include <utility>

// A fixed-capacity, heap-free, exception-free type-erased callable — the freestanding
// replacement for std::function. Captures live INLINE in a byte buffer (no global `new`),
// so a callable stored here never touches the heap and compiles under -fno-exceptions
// -fno-rtti. A callable larger than `Capacity` is a COMPILE error (static_assert), never a
// silent heap fallback. This is where Canopy event handlers (on_click closures) live.
namespace canopy {

    template <class Signature, std::size_t Capacity = 32,
              std::size_t Align = alignof(std::max_align_t)>
    class inplace_function;

    template <class R, class... Args, std::size_t Capacity, std::size_t Align>
    class inplace_function<R(Args...), Capacity, Align> {
    public:
        inplace_function() noexcept = default;

        // Construct from any compatible callable, copied into the inline buffer. Rejected at
        // compile time if it does not fit — no heap fallback, ever. The callable must be
        // CONST-invocable (operator() const) because the stored object is invoked through a
        // const path; this rejects a `mutable` lambda cleanly AT this constructor rather than
        // deep inside the invoke trampoline. Stateful handlers keep their state in signals,
        // not in mutable captures.
        template <class Fn, class Decayed = std::decay_t<Fn>>
            requires(!std::is_same_v<Decayed, inplace_function> &&
                     std::is_invocable_r_v<R, const Decayed&, Args...>)
        inplace_function(
            Fn&& func) { // NOLINT(google-explicit-constructor) — function-like by design
            static_assert(sizeof(Decayed) <= Capacity,
                          "callable too large for this inplace_function capacity");
            static_assert(alignof(Decayed) <= Align,
                          "callable over-aligned for this inplace_function alignment");
            std::construct_at(static_cast<Decayed*>(storage()), std::forward<Fn>(func));
            // NOLINTNEXTLINE(cppcoreguidelines-prefer-member-initializer): follows construct_at
            vtable_ = &vtable_for<Decayed>;
        }

        inplace_function(const inplace_function& other) {
            if (other.vtable_ != nullptr) {
                other.vtable_->copy(other.storage(), storage());
                vtable_ = other.vtable_;
            }
        }

        inplace_function(inplace_function&& other) noexcept {
            if (other.vtable_ != nullptr) {
                other.vtable_->move(other.storage(), storage());
                vtable_ = other.vtable_;
                other.reset();
            }
        }

        auto operator=(const inplace_function& other) -> inplace_function& {
            if (this != &other) {
                reset();
                if (other.vtable_ != nullptr) {
                    other.vtable_->copy(other.storage(), storage());
                    vtable_ = other.vtable_;
                }
            }
            return *this;
        }

        auto operator=(inplace_function&& other) noexcept -> inplace_function& {
            if (this != &other) {
                reset();
                if (other.vtable_ != nullptr) {
                    other.vtable_->move(other.storage(), storage());
                    vtable_ = other.vtable_;
                    other.reset();
                }
            }
            return *this;
        }

        ~inplace_function() {
            reset();
        }

        // Invoke the stored callable. Calling an empty inplace_function is undefined (check
        // with `bool` first) — there is no exception to throw on the freestanding target.
        auto operator()(Args... args) const -> R {
            return vtable_->invoke(storage(), std::forward<Args>(args)...);
        }

        explicit operator bool() const noexcept {
            return vtable_ != nullptr;
        }

    private:
        struct vtable {
            R (*invoke)(const void*, Args...);
            void (*move)(void* src, void* dst);
            void (*copy)(const void* src, void* dst);
            void (*destroy)(void*) noexcept;
        };

        template <class Fn>
        static constexpr vtable vtable_for = {
            [](const void* self, Args... args) -> R {
                return (*static_cast<const Fn*>(self))(std::forward<Args>(args)...);
            },
            [](void* src, void* dst) {
                std::construct_at(static_cast<Fn*>(dst), std::move(*static_cast<Fn*>(src)));
            },
            [](const void* src, void* dst) {
                std::construct_at(static_cast<Fn*>(dst), *static_cast<const Fn*>(src));
            },
            [](void* self) noexcept { std::destroy_at(static_cast<Fn*>(self)); },
        };

        [[nodiscard]] auto storage() noexcept -> void* {
            return static_cast<void*>(buffer_.data());
        }
        [[nodiscard]] auto storage() const noexcept -> const void* {
            return static_cast<const void*>(buffer_.data());
        }

        void reset() noexcept {
            if (vtable_ != nullptr) {
                vtable_->destroy(storage());
                vtable_ = nullptr;
            }
        }

        alignas(Align) std::array<std::byte, Capacity> buffer_{};
        const vtable* vtable_ = nullptr;
    };

} // namespace canopy
