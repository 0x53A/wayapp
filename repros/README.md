# Regression tests

Run non-graphical tests in the Nix shell:

```sh
bash repros/run-native.sh
```

Run all unit, integration and example targets, including ignored native tests,
on a private headless Sway compositor:

```sh
bash repros/run-headless.sh
```

The default headless command also runs doctests; Cargo's `--all-targets` alone
does not. Both runners accept explicit Cargo arguments, for example:

```sh
bash repros/run-headless.sh test --locked --offline --all-targets --all-features -- --include-ignored --test-threads=4
bash repros/run-headless.sh test --release --jobs 2 --locked --offline --all-targets --all-features -- --include-ignored --test-threads=4
bash repros/run-native.sh test --locked --offline --doc --all-features
```

## Safety and tooling

Do not run virtual-input, capture or clipboard-publisher tests on a personal
desktop. Their shared helpers require the private-runtime marker, a direct
non-symlink directory with private permissions, and a local non-symlink Wayland
socket. They reject inherited display descriptors and serialize virtual input
within each test binary. Captured pixels stay in memory. These are accident
prevention checks, not a sandbox against deliberately spoofed environments.

The headless runner supplies Sway/grim through the optional Nix shell, clears
inherited display sockets, selects software Vulkan when the system Lavapipe ICD
is available, and stops its own compositor on exit. It prints the temporary
runtime directory containing diagnostics. Deliberately fatal client regressions
run in bounded subprocesses; their private compositor protocol errors are
expected and do not establish reconnect support.

Developer-only Nix runners are not runtime dependencies or requirements of the
focused production fixes. Additional standalone diagnostics and parked optional
changes live outside the repository in the task's reviewer tools.

## Retained coverage

- Display/clipboard, native role, GPU resource and viewport teardown; replacement
  views on retained native surfaces; Application Send and moving an initialized
  application between threads.
- One-shot texture uploads/frees on skipped or suspended frames, bounded live
  GPU allocations across skipped uploads (on backends with allocator reports), GPU callback
  command submission, safe resume, actual device resolution limits and recovery
  from compositor-valid oversized configure suggestions.
- Native GLES initialization and supported presentation modes; controlled
  acquisition failures and deferred repaint. Injected failures are not real
  driver faults.
- Current dimensions on zero/unspecified configure, initial input dimensions,
  default-SDK scale, matching buffer-scale commits, settled egui zoom, stationary
  pointer refresh, native widget hit testing and solid-color replacement pixels.
- Ordinary primary/secondary/middle buttons, cursor hiding and focus ownership,
  pointer-device loss without synthetic clicks, managed proxy retirement and
  caller-owned proxy survival. No synthetic multi-pointer survivor policy is
  claimed.
- Logical symbols/physical aliases, changed release identity, modifiers,
  shortcuts, Unicode and private clipboard transfer, native wheel/touchpad
  movement, native client repetition and cancellation on release/focus/device/
  surface loss.
- Controlled SDK server-repeat callbacks inspect raw text as well as TextEdit
  behavior: per-key composition, unrelated Num Lock, Shift/Control, keymaps and
  keys already held on enter. This is not on-wire protocol-v10 support.
- Caller-created live SDK keyboards retain upstream callback forwarding without
  adoption. Legal modifiers without keyboard focus are delivered without claiming
  focus; stale retired managed proxies cannot restore interactions.
- IME adapter cancellation and committed-text preservation, empty/null payloads
  and high-level commit/preedit routing between two editor contexts. These do
  not enable native IME integration or preedit active-range support.
- Atomic repaint deadline coalescing, reentrant callbacks and owner-dependent
  shutdown. Scheduler drop signals and detaches; an in-flight callback may finish
  later. Reader drop cancels idle waits without joining user callbacks.
- Terminal connection error delivery, native callbacks drained before timers on
  either wake token, reader acknowledgment only on native tokens, and transient
  flush backpressure with continued incoming readiness and local cancellation.
- CPU dependency-contract tests for egui texture deltas and scroll phases;
  bounded alias/focus state sequences and private-runtime safety probes.

`WaylandEvent::PointerRemoved` and the adapter cancellation method are deliberate
public additions: exhaustive event matches need updating. Ordinary pointer leave
still preserves dragging; device disappearance cancels without fabricating a
release that could click a widget. Other focused candidates need no public API
changes. Low-level native surfaces must outlive GPU resources; proxy clones do
not enforce that owner lifetime.

## Known limits

- Clipboard loading is synchronous and can wait indefinitely for an owner.
  The bounded slow-owner diagnostic measures this limitation; it does not fix
  responsiveness or provide cancellation. Its data-control publisher is test-only.
- Native text-input-v3 binding remains disabled. Transaction/serial handling,
  surrounding-text deletion, active-range conversion and zoomed native cursor
  rectangles need separate integration work. No guessed byte-width deletion or
  synthetic key workaround is implemented.
- Egui applies pending zoom inside its next pass, after input conversion. Settled
  zoom tests do not fix that first-transition ordering gap; no dummy UI pass or
  click replay is used. Stock egui exposes neither pending zoom nor an input hook
  between applying zoom and processing input. Fixing this needs upstream egui
  support; this library does not impose a fork or a workspace Cargo override.
- General public-handle/connection replacement misuse, manually released private
  device recovery, side-button extensions and synthetic survivor restoration are
  outside this proposal.
- Tests do not establish physical hotplug, full multiseat routing, every GPU or
  compositor, or a general concurrency/lifetime safety guarantee. Initially
  cached scale recognizes default `SurfaceData<()>`; custom data retains the
  previous scale-1 fallback until a forwarded scale event.
