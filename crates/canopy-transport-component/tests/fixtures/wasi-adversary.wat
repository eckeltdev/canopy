;; An adversarial Canopy "guest" component that imports a capability the host does
;; NOT grant: `wasi:cli/environment` (read the process environment). The host
;; (`canopy-transport-component`) adds ONLY the `canopy:ui/host` interface to its
;; linker, so instantiating this component MUST fail with an unsatisfied-import error
;; — proving the guest cannot reach the OS through anything but the one granted
;; capability. It still exports the same `run: func()` the world expects, so the only
;; thing standing between it and execution is the missing authority.
(component
  (import "wasi:cli/environment@0.2.0" (instance
    (export "get-environment" (func (result (list (tuple string string)))))
  ))
  (core module $m
    (func (export "run"))
  )
  (core instance $i (instantiate $m))
  (func (export "run") (canon lift (core func $i "run")))
)
