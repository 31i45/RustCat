# RustCat

A running cat on your taskbar that sprints with your CPU load.

![RustCat running on the taskbar](demo1.gif)

[![CI](https://github.com/31i45/RustCat/actions/workflows/ci.yml/badge.svg)](https://github.com/31i45/RustCat/actions/workflows/ci.yml)
[![License](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Windows%2010%2B-0078D6.svg)](https://github.com/31i45/RustCat)
[![Rust](https://img.shields.io/badge/Rust-stable-orange.svg)](https://www.rust-lang.org)
[![Stars](https://img.shields.io/github/stars/31i45/RustCat?style=social)](https://github.com/31i45/RustCat/stargazers)

RustCat is a featherweight Windows tray application that keeps a tiny cat running
in your notification area. The cat's speed follows your CPU load in real time —
idle, it strolls; under load, it sprints.

It is a Rust rewrite of the core of [RunCat365](https://github.com/kyome22/RunCat365)
(C#/.NET), reduced to the single thing that makes RunCat delightful:
**CPU sampling → speed formula → frame animation**. Nothing else.

## Features

- **Live CPU animation** — 5-frame cat animation driven by a Windows PDH
  performance counter (fallback chain: `Processor Information` → `Processor`,
  `% Processor Utility` → `% Processor Time` on pre-26100 builds), smoothed
  over a rolling window.
- **Temperature tint** — the cat's tint shifts from its theme colour toward red as
  the CPU heats up: tinting starts at 60 °C and saturates at 90 °C across six
  levels, with 2 °C hysteresis so a reading hovering on a boundary does not
  flicker. The reading is the CPU package temperature, read through PawnIO (a
  signed kernel driver that hosts sandboxed modules) with a vendor-specific
  backend: MSR `0x1A2`/`0x1B1` on Intel, SMN `0x00059800` on AMD Zen — and
  smoothed over the same 5-sample window. Tint and theme are
  orthogonal: each theme owns its own six-colour palette, and the level only
  selects an entry. Where PawnIO is absent, or RustCat is not elevated, the icon
  keeps the plain theme colour instead of faking a value. The temperature
  feature needs PawnIO plus administrator rights — see
  [Temperature and administrator rights](#temperature-and-administrator-rights).
- **Hover flyout** — hovering the tray icon shows a compact panel with **CPU**,
  **memory**, **disk** and **temperature** activity (the temperature row is in °C
  and reads `—` when PawnIO is unavailable or RustCat is not elevated);
  per-monitor DPI aware, auto-hides when the cursor leaves. Deliberately
  excludes GPU (no cross-vendor user-mode temperature channel) and network (no
  frame of reference at a glance).

  ![Flyout detail](demo2.gif)
- **Theme aware** — black cat on light taskbars, white cat on dark ones while it
  is cool; the temperature tint shifts either base colour once the CPU heats up.
  Follows the system theme live (responds to `WM_SETTINGCHANGE`).
- **Run at startup** — left-click the tray icon to toggle; confirmed by a
  checked entry in the right-click menu and a permanent second line in the
  tooltip. Stored in the `HKCU` `Run` key, no settings file required.
- **Featherweight** — a single runtime dependency (the [`windows`](https://crates.io/crates/windows)
  crate), ~0.20 MiB release binary, pure Win32 message loop. No runtime, no
  scheduler, no polling: everything is timer- and message-driven. PawnIO is an
  optional external prerequisite for temperature, not a Rust dependency, so the
  one-dependency count stands.

## How it works

```
PDH sample (1 s) → rolling average → speed = max(1.0, load / 5.0)
                  → frame interval = 500 / speed  (ms)
```

The busier your CPU, the faster the cat runs — and when the system is idle the
animation deliberately slows down to ~2 fps, exactly like the original RunCat.

A second branch reads the CPU package temperature through PawnIO — a signed
driver loaded dynamically via `LoadLibraryW`, whose absence is not an error —
and smooths it over the same rolling window. The backend is chosen from the
CPUID vendor string: Intel reads MSR `0x1B1` (TjMax from `0x1A2` minus the
digital readout), AMD Zen reads SMN `0x00059800` (CUR_TEMP × 0.125 °C, with the
49 °C offset that `RANGE_SEL`/`TJ_SEL` signal, and the Tdie correction for the
first two Ryzen generations). That value maps to one of six tint levels — 60 °C
starts the shift, 90 °C is full red — and the level only picks a colour from the
current theme's palette; it never affects the frame rate.

The five cat frames are baked into the executable at build time as raw BGRA
pixels (no PNG decoding at runtime), and the `.exe` file icon is generated as a
linker resource with zero external tooling.

## Build

Requirements: Windows 10 or later, Rust (MSVC toolchain).

```sh
git clone https://github.com/31i45/RustCat.git
cd RustCat
cargo build --release
```

The executable is at `target/release/rustcat.exe`.

## Usage

| Action              | Result                                          |
| ------------------- | ----------------------------------------------- |
| Hover the tray icon | Flyout: CPU / memory / disk / temperature             |
| Left-click          | Toggle *Run at startup* (tooltip second line updates) |
| Right-click         | Menu: **自启** (✓ = enabled), **退出**           |

Exit fully cleans up: tray icon, timers, icons and mutex — no leftovers.

Temperature requires running as administrator; see
[Temperature and administrator rights](#temperature-and-administrator-rights).

## Temperature and administrator rights

Only the temperature tint and the flyout's temperature row need extra setup;
everything else works out of the box.

- **PawnIO must be installed.** The CPU package temperature is exposed through
  model-specific registers that user-mode code cannot access, so reading it
  needs a kernel driver. RustCat uses [PawnIO](https://pawnio.eu), a free,
  digitally signed, LGPL-licensed driver that hosts sandboxed modules; download
  and install it from that site. RustCat never installs, starts or updates the
  driver — it only talks to an installation that already exists.
- **RustCat must run as administrator.** The driver's device object is accessible
  to the Administrators group only, so an unelevated process cannot open it even
  with PawnIO installed.

If either condition is unmet, the flyout's temperature row reads `—` and the
icon keeps the plain theme colour. CPU, memory, disk, run-at-startup and theme
following are unaffected: no error dialog, no retry, no fabricated reading.

Coverage: Intel Nehalem and later (MSR `0x1A2`/`0x1B1`) and AMD Zen (families
17h/19h/1Ah, SMN `0x00059800`). Older AMD parts (10h–16h) and Intel parts
without a package thermal MSR report no reading.

**Prefer not to run as administrator?** Use
[v1.0.0](https://github.com/31i45/RustCat/releases/tag/v1.0.0) instead — the
last release without temperature tracking. The cat, the hover flyout (CPU,
memory, disk), run-at-startup and theme following are all there; only the
temperature row and the heat tint are missing.

## Design notes

- One dependency, one purpose. No GPU or network meters; no settings
  UI; no persistence beyond the `Run` key.
- Single instance enforced by a named mutex.
- `panic = "abort"`, LTO, `opt-level = "z"` — a static binary with no
  unwinding machinery.
- Build-time assertions guard the icon assets against silent drift; the
  guards live in the unit-test suite, so CI runs them.
- `VS_VERSIONINFO` is hand-assembled in `build.rs` with zero external tooling
  (structure verified via PE dump; property-sheet reading is a known open item).
- Temperature tinting rewrites only RGB bytes and leaves alpha untouched, so the
  baked silhouettes stay exact. Level 0 reuses the build-time baked icons and
  creates no extra GDI objects; levels 1–5 are generated lazily per
  `(theme, level)` pair, cached as HICONs and destroyed on exit. PawnIOLib is
  loaded dynamically with `LoadLibraryW` and both vendor modules are embedded
  (LGPL, see `assets/pawnio/COPYING`), so a machine without PawnIO — or a
  non-elevated run — simply has no temperature: the level stays 0 and the icon
  keeps its theme colour — degradation over fabrication. Pre-Zen AMD (family
  10h–16h) and Intel parts without `0x1B1` are in the same bucket: no reading
  rather than a guessed one; AMD is gated on the CPUID family so the SMN channel
  is never trusted on chips where `0x00059800` has no meaning.
- Temperature is the only feature that asks for elevation: its data lives in
  MSRs and SMNs that only ring 0 can read, and PawnIO's device object ACL admits
  Administrators alone. The driver is loaded at runtime and stays optional —
  its absence is a supported state, not a failure the app works around.
- The 1.1.x ACPI path is gone on purpose. Its PDH counter appeared to work but
  did not move under sustained full load (a firmware placeholder), so a real
  reading behind elevation was chosen over a convenient fake one.

## Credits

Built as a faithful Rust rewrite of [RunCat365](https://github.com/kyome22/RunCat365)
by [kyome](https://github.com/kyome22). The original concept and the cat
artwork belong to kyome (MIT License).

## License

[MIT](LICENSE)
