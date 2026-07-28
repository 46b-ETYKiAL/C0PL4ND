//! Binary entry point for `c0pl4nd` — the modern eframe/egui app (the canonical
//! binary). The legacy winit-driven terminal (`src/main.rs`) ships as
//! `c0pl4nd-legacy`; both build side by side.
//!
//! eframe owns the winit event loop — there is no second event loop here, and
//! no Win32 window subclass (window controls go through `ViewportCommand`).

// Release builds are a GUI subsystem app: no extra console window pops up
// alongside the window (a debug build keeps the console so tracing/wgpu logs
// are visible). Without this, launching the installed app spawns a second
// "terminal" window showing the wgpu/Vulkan INFO log spam.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]
// The egui chrome shell is pure-safe Rust: `deny(unsafe_code)` keeps ALL of this
// binary's own UI code unsafe-free. The SOLE exception is the audited
// `dll_hardening` platform-init module (it must call two Win32 search-order
// functions); it opts back in with a narrowly-scoped `#![allow(unsafe_code)]`.
// `deny` (not `forbid`) is required precisely so that one vetted module can
// override it — `forbid` is unconditional and would reject the submodule.
#![deny(unsafe_code)]

// mimalloc as the global allocator (see crates/app/Cargo.toml) — the
// declaration is safe; no `unsafe` needed.
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod diagnostics;
mod dll_hardening;
mod panic_hook;
#[path = "update/mod.rs"]
mod update;

// Additive Win32 caption subclass for the Windows 11 Snap Layouts flyout. It
// lives physically under `egui_app/` (a sibling of the lib-owned chrome modules)
// but is a BINARY-local module of the shipping `c0pl4nd` binary — declared here,
// compiled only into egui_main, never into the `#[path]`-included kittest lib
// harnesses (which have no real HWND). It quarantines the raw Win32 FFI behind its
// own `#![allow(unsafe_code)]`, so this binary's `#![deny(unsafe_code)]` holds.
#[path = "egui_app/win_chrome.rs"]
mod win_chrome;

// Additive system-tray icon (single-click minimize/restore + right-click menu).
// A BINARY-local module of the shipping `c0pl4nd` binary, exactly like
// `win_chrome`: it needs the real eframe HWND + the running winit event loop, so
// it lives physically under `egui_app/` but is declared here and never compiled
// into the `#[path]`-included kittest lib harnesses. Its raw Win32 FFI is
// quarantined behind its own `#![allow(unsafe_code)]`, so this binary's
// `#![deny(unsafe_code)]` holds.
#[path = "egui_app/tray.rs"]
mod tray;

// Quake mode: a DEFAULT-OFF global hotkey that drops the window down from the top
// of the monitor under the cursor and hides it again. A BINARY-local module for
// the same reason as `tray`/`win_chrome` — it needs the real eframe HWND and the
// running winit message loop (it reads `WM_HOTKEY` through its own
// `SetWindowSubclass` entry, chaining rather than competing with `win_chrome`'s).
// Its raw Win32 FFI is quarantined behind its own `#![allow(unsafe_code)]`.
#[path = "egui_app/quake.rs"]
mod quake;

// The egui shell lives in this crate's lib target so `tests/` links THIS
// compilation instead of `#[path]`-including a private second copy — which made
// llvm-cov attribute the kittest suites' coverage to an object the report never
// reads (see lib.rs). The re-import keeps `crate::reporting` &c. resolving for
// the binary's own modules: `panic_hook` reaches reporting this way.
// W1TN3SS opt-in reporting glue (Tier-1 crash spool + manual issue intake).
// Pure consumers of the pinned-tag `itasha-report-core` SDK; both default OFF.
use c0pl4nd::{cli_cwd, egui_app, user_error};

// `reporting`'s only consumer in this binary is `panic_hook::capture_panic_w1tn3ss`,
// which is itself `cfg(not(feature = "legacy-winit"))`. Carry the same gate here or
// the import is unused under `--all-features` (which turns `legacy-winit` on) and
// warns.
#[cfg(not(feature = "legacy-winit"))]
use c0pl4nd::reporting;

use eframe::egui;

use egui_app::gpu_diag;

fn main() -> eframe::Result<()> {
    // FIRST statement: harden the Windows DLL search order before any other DLL
    // could be implicitly loaded, defeating DLL-planting when launched from an
    // untrusted directory (e.g. Downloads). Safe wrapper; the Win32 `unsafe` FFI
    // lives in `dll_hardening` so this `#![forbid(unsafe_code)]` binary stays
    // unsafe-free. No-op off Windows.
    dll_hardening::harden_dll_search_order();

    // Install the unexpected-panic crash hook early (before the window /
    // event-loop): `panic = "abort"` otherwise kills the GUI with no diagnostic.
    // The hook writes a rotating crash log (and, on Windows, shows a MessageBox)
    // then chains to the default hook — it runs before the abort fires.
    panic_hook::install();

    // Create the process-wide KILL_ON_JOB_CLOSE job object (Windows) so no PTY
    // child shell — or anything it spawns — can outlive this process, even on a
    // hard `std::process::exit(0)` or a crash. Best-effort; the fast-close
    // `kill_child` path is the primary no-orphan guarantee, this is the backstop.
    egui_app::job_object::init();

    // Best-effort tracing. Mirror the legacy binary EXACTLY (F9-2): read the
    // `C0PL4ND_LOG` env var (NOT the default `RUST_LOG`) and default to `warn`.
    // The two binaries previously diverged — this one read `RUST_LOG` and
    // defaulted to the noisier `info` — so a user setting `C0PL4ND_LOG` saw it
    // honoured by the legacy binary but ignored by the canonical one, which also
    // logged at `info` in release. Both now share one contract: C0PL4ND_LOG/warn.
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("C0PL4ND_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .try_init();

    let args: Vec<String> = std::env::args().collect();

    // `c0pl4nd --version` — print and exit (no window).
    if args.iter().any(|a| a == "--version" || a == "-V") {
        println!("{} {}", c0pl4nd_core::PRODUCT_NAME, c0pl4nd_core::version());
        return Ok(());
    }

    // `c0pl4nd --diagnostics` (alias `--doctor`) — print a one-shot env/config
    // dump and exit BEFORE any window/GPU init. IME text routing is always
    // compiled into the egui app (it handles `egui::Event::Text`).
    if diagnostics::requested(&args) {
        diagnostics::run(true, panic_hook::crash_log_dir());
        return Ok(());
    }

    // `c0pl4nd update` — explicit, user-initiated update check (no window). Reads
    // the configured channel from the persisted config, defaulting to stable.
    if args.iter().any(|a| a == "update") {
        let channel = c0pl4nd_core::Config::default_path()
            .filter(|p| p.exists())
            .and_then(|p| {
                std::fs::read_to_string(&p)
                    .ok()
                    .and_then(|s| c0pl4nd_core::Config::from_toml(&s, &p).ok())
            })
            .map(|c| c.update.channel)
            .unwrap_or_else(|| "stable".to_string());
        // `run_update` is offline-graceful and only prints; surface any hard
        // error to stderr but still exit 0 (a failed check is not a crash).
        if let Err(e) = update::run_update(&channel) {
            tracing::warn!(target: "c0pl4nd::update", detail = ?e, "update subcommand failed");
            eprintln!("Couldn't check for updates: an unexpected problem occurred.");
        }
        return Ok(());
    }

    // `c0pl4nd --cwd <path>` (alias `-d <path>`, matching Windows Terminal) —
    // the directory the INITIAL shell starts in. This is what the Explorer
    // "Open C0PL4ND here" context-menu verb passes (`--cwd "%V"`), so the value
    // is UNTRUSTED: a path that does not exist, or that names a file, is
    // refused here with a clear message and a non-zero exit rather than
    // silently ignored (which reads as "the menu entry is broken") or panicked.
    //
    // The validated directory is handed to the one-shot `cli_cwd` store, which
    // the deferred first-pane spawn consumes and passes to
    // `PaneTerm::spawn_in_with_term`. It is deliberately NOT applied with
    // `std::env::set_current_dir`: `PtyProcess::spawn_program_in_with_term`
    // always sets the child's directory explicitly (requested cwd, else the
    // HOME fallback), so the child never inherits the process cwd and
    // `set_current_dir` would be a silent no-op. Threading it to the spawn seam
    // is the only wire that actually reaches the shell.
    match cli_cwd::parse_startup_cwd(&args) {
        Ok(Some(dir)) => cli_cwd::set_startup_cwd(&dir),
        Ok(None) => {}
        Err(e) => {
            // `show_startup_error` also prints to stderr, and shows a dialog on
            // Windows — where a release build is a GUI-subsystem app with no
            // console, so stderr alone would be invisible.
            panic_hook::show_startup_error("C0PL4ND couldn't start", &e.user_message());
            std::process::exit(2);
        }
    }

    // The window position + size are persisted natively by eframe via the
    // `persistence` feature + `NativeOptions.persist_window` below (ron state
    // stored under the stable `with_app_id` folder). We set only the FIRST-RUN
    // default size here; eframe restores the user's last size on later launches.
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1100.0, 720.0])
        .with_min_inner_size([520.0, 360.0])
        .with_app_id("com.itashacorp.c0pl4nd")
        .with_title("C0PL4ND")
        .with_decorations(false) // frameless — we draw our own titlebar
        .with_transparent(true) // required for rounded corners + acrylic blur
        // Ask winit/the OS to make the window ACTIVE (focused + foreground) at
        // creation so it does not open BEHIND other windows on first launch. This
        // is the polite first request; the app additionally sends
        // `ViewportCommand::Focus` and, on Windows 11 (foreground-lock), runs the
        // `win_foreground` AttachThreadInput nudge ONCE on the first frame as a
        // backstop (see `egui_app::win_foreground`).
        .with_active(true);
    // RESTORE the native minimize/maximize STYLE BITS (do NOT clear them at
    // creation). egui-winit maps the min/max "enabled buttons" to
    // WS_MINIMIZEBOX | WS_MAXIMIZEBOX on the (undecorated) window, and Windows
    // gates ALL of Aero Snap on those bits: WS_MAXIMIZEBOX gates
    // drag-to-top-maximize, drag-to-edge, Win+Left/Right/Up, Snap Assist AND the
    // Windows 11 Snap Layouts flyout; WS_MINIMIZEBOX gates Win+Down and the
    // taskbar minimize/restore animation. An earlier revision cleared both bits at
    // creation (`.with_minimize_button(false)` / `.with_maximize_button(false)`) to
    // stop DWM compositing a second native min/max set over the custom titlebar —
    // but that ALSO disabled every snap gesture (the reported "drag-to-top doesn't
    // maximize, no Snap Layouts"). winit 0.30.13 already hides the native
    // non-client frame via a `WM_NCCALCSIZE`-returns-0 handler on the undecorated
    // window, so keeping the style bits SET restores snap WITHOUT re-admitting a
    // doubled native frame (real-Win11 verification of the transparent-window case
    // is noted in `win_chrome`). The Win11 Snap Layouts FLYOUT additionally needs a
    // `WM_NCHITTEST` -> `HTMAXBUTTON` reply over the maximize button; that is added
    // by the additive `win_chrome` Win32 subclass (see the `win_chrome` module),
    // which layers on top of winit's frame and never touches WM_NCCALCSIZE.
    // (WS_SYSMENU's residual native close "x" is still stripped per-frame by
    // `egui_app::caption_close`; that is orthogonal to these snap bits.)
    // Runtime window + taskbar icon (the sigil). The exe's embedded icon
    // resource (build.rs) covers the Start-menu shortcut / Explorer /
    // Add-Remove-Programs; this covers the live window. Best-effort — a decode
    // failure leaves the platform default rather than blocking startup.
    if let Some(icon) = load_app_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    // Keep the window on top of all others when the user has enabled it (mirrors
    // SCR1B3's F-035). This is the STARTUP application so the flag takes effect
    // from the first frame; the settings toggle additionally live-applies it at
    // runtime via `ViewportCommand::WindowLevel`, so no relaunch is needed.
    if launch_always_on_top() {
        viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnTop);
    }

    let mut options = eframe::NativeOptions {
        viewport,
        // Persist native window position + size across restarts (#24/#25): pairs
        // with the eframe `persistence` feature + the stable `with_app_id` above.
        // eframe stores the geometry (and any `App`-saved state) as ron under the
        // app_id folder and restores it on the next launch; it also fires
        // `App::save()` on exit/interval once persistence is on. The settings
        // `egui::Window` (a resizable Window with a stable Id) likewise has its
        // size persisted automatically by this feature — no hand-rolled plumbing.
        persist_window: true,
        // Keep the wgpu backend (default via the `wgpu` feature); do NOT enable
        // glow — glyphon (Milestone 2) shares egui's wgpu device.
        ..Default::default()
    };
    prefer_backend_on_windows(
        &mut options,
        launch_transparency_enabled(),
        launch_backend_override(),
    );
    apply_gpu_preference(
        &mut options,
        launch_gpu_preference(),
        launch_transparency_enabled(),
    );
    // TRANSPARENCY ADAPTER SELECTION (the fix for "opaque black" on a hybrid-GPU
    // Optimus laptop). `power_preference` is only a HINT; the gpu-diag.log evidence
    // showed the discrete NVIDIA GPU advertises `PreMultiplied` but is opaqued by
    // the Optimus off-screen→display copy, while the display-driving Intel iGPU is
    // the one that actually composites see-through. eframe's `native_adapter_selector`
    // OVERRIDES the hint and is handed the real surface, so we DETERMINISTICALLY pick
    // the integrated / display-driving adapter (never the discrete GPU) and log every
    // adapter's `alpha_modes` + the mode egui-wgpu will configure to
    // `<config_dir>/gpu-diag.log`. Paired with default (non-forced) backends +
    // `LowPower`, this replicates why SCR1B3 is see-through on the same box. Only
    // installed when transparency is on; the opaque path keeps the discrete GPU.
    install_transparency_adapter_selector(&mut options, launch_transparency_enabled());

    // Present latency raises the swapchain frame-queue depth. A previous
    // `= Some(1)` "optimization" CORRUPTED the terminal grid: the draw raced
    // egui's font-atlas texture upload, so glyphs rendered from a not-yet-complete
    // atlas — heavily garbled on a fast discrete GPU (NVIDIA), and NEVER in an
    // offscreen/synchronous render (every headless snapshot is pixel-perfect,
    // which is what made it so hard to pin down). Empirically the garble scales
    // with latency: 1 = consistent, 2 (eframe/wgpu default) = intermittent, so we
    // set it HIGHER to give the upload more frames to land before a draw samples
    // it. 3 is still imperceptible for a terminal (< ~50 ms keystroke→glyph on a
    // 60 Hz panel); a garbled grid is not. Paired with the startup font-atlas
    // pre-warm (`prewarm_grid_atlas`), which keeps the atlas from GROWING mid-
    // render. Do NOT lower this. We also keep the default vsync present mode (NOT
    // Mailbox/AutoNoVsync): the app is event-driven, so an idle terminal repaints
    // ~0 fps and Mailbox would force wasteful continuous high-FPS rendering.
    //
    // EXCEPTION — translucent window: use latency 1, matching SCR1B3 exactly (its
    // whole config is the reference for the see-through path). The glyph-garble that
    // motivated latency 3 is a DX12/NVIDIA hazard; the translucent path runs on the
    // integrated GPU with default (non-DX12-forced) backends, where the hazard does
    // not apply, so we mirror SCR1B3's latency 1 to remove any remaining difference
    // from the known-good see-through configuration. The opaque path keeps latency 3.
    options.wgpu_options.desired_maximum_frame_latency =
        Some(if launch_transparency_enabled() { 1 } else { 3 });

    let result = eframe::run_native(
        "C0PL4ND",
        options,
        Box::new(|cc| {
            let app = egui_app::C0pl4ndApp::new(cc);
            // Windows 11 Snap Layouts: prime the additive `win_chrome` caption
            // subclass with the REAL eframe HWND (same handle `caption_close` /
            // `win_foreground` use), then drive its per-frame maximize-button-rect
            // publish + one-shot subclass install from a begin-pass hook. The hook
            // runs every pass on the winit/main thread; `win_chrome::tick` reads the
            // live `content_rect()` + `pixels_per_point()` so the published rect
            // tracks window resize and DPI. Additive over winit's frame; a no-op off
            // Windows and when `C0PL4ND_DISABLE_SNAP_CHROME` is set.
            #[cfg(windows)]
            {
                use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(handle) = cc.window_handle() {
                    if let RawWindowHandle::Win32(w) = handle.as_raw() {
                        win_chrome::prime_hwnd(w.hwnd.get());
                    }
                }
            }
            cc.egui_ctx.on_begin_pass(
                "win_chrome_snap_layouts",
                std::sync::Arc::new(|ui: &mut egui::Ui| win_chrome::tick(ui.ctx())),
            );
            // System-tray icon. Created HERE — after the window exists and on the
            // event-loop thread — so it can prime the real HWND (for
            // minimize/restore) and register its click/menu handlers. A single
            // LEFT click toggles minimize/restore; right click opens Show / Hide /
            // Quit. Best-effort: `tray::init` never panics or blocks startup (a
            // headless/session-0 shell simply gets no tray). The icon reuses the
            // embedded sigil PNG (`load_app_icon`, re-decoded once). No-op off
            // Windows. This is the tray's live creation call site — the wiring the
            // whole feature depends on.
            #[cfg(windows)]
            {
                use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(handle) = cc.window_handle() {
                    if let RawWindowHandle::Win32(w) = handle.as_raw() {
                        tray::prime_hwnd(w.hwnd.get());
                    }
                }
            }
            if let Some(icon) = load_app_icon() {
                tray::init(&cc.egui_ctx, icon.rgba, icon.width, icon.height);
            }
            // Quake mode (drop-down terminal). Primed + armed HERE for the same
            // reasons as the tray: the real HWND must exist and we must be on the
            // event-loop thread, because `RegisterHotKey` targets that window's
            // message queue and `quake` reads `WM_HOTKEY` off it via its own
            // subclass. DEFAULT OFF — `quake::init` registers NOTHING unless
            // `quake.enabled` is set in the config, and refuses an unparseable or
            // modifier-less combo rather than claiming an unintended global hotkey.
            // Applies at startup (a settings change takes effect on the next
            // launch); best-effort — a busy combo logs a warning and never blocks.
            #[cfg(windows)]
            {
                use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
                if let Ok(handle) = cc.window_handle() {
                    if let RawWindowHandle::Win32(w) = handle.as_raw() {
                        quake::prime_hwnd(w.hwnd.get());
                    }
                }
            }
            quake::init(&cc.egui_ctx, &launch_quake_config());
            // On-launch update check. Drives the SHARED in-app updater that powers
            // the persistent, dismissible NOTIFICATION BANNER (and the Settings →
            // Updates page): a found update surfaces a one-click "Update now" strip
            // that runs the WHOLE verified flow in place — download → SHA-256 +
            // minisign verify → silent `self-replace` → relaunch — never a browser
            // hand-off. Runs by default: the `notify` mode performs this on-launch
            // check (as does `auto`, which additionally auto-applies), plus the
            // legacy `check_on_launch` flag; `manual`/`off` suppress it. Throttled
            // by `check_interval_hours`. The check runs on a background thread
            // inside the updater, so startup never blocks; the banner reflects the
            // result on subsequent frames.
            let (should_check, mode) = launch_check_config();
            if should_check {
                egui_app::start_launch_update_check(&cc.egui_ctx, mode);
                // Record the attempt (success OR failure) so the interval throttle
                // suppresses the next launch's check until due.
                update::record_check_now();
            }
            Ok(Box::new(app))
        }),
    );

    // A GPU adapter/device or window-init failure comes back as a clean `Err`
    // (NOT a panic), so the panic hook never fires and a release GUI build — which
    // has no console — would otherwise show nothing at all. Surface it with a
    // diagnostic + a recovery hint before propagating the error.
    if let Err(e) = &result {
        panic_hook::show_startup_error("C0PL4ND couldn't start", &user_error::gpu_init_failed(e));
    }
    result
}

/// Decide whether to run the on-launch update check and which update MODE the
/// shared in-app updater should run under. Reads the persisted config directly
/// (the same load path the `c0pl4nd update` CLI subcommand uses) so the decision
/// honours the canonical `[update] mode` (`notify`/`auto` check on launch) as
/// well as the legacy `check_on_launch` flag, without depending on the host
/// app's accessor. When no config file exists yet (first-ever launch) the
/// canonical [`UpdateConfig`] default is used, so a brand-new user gets the same
/// default (`notify`) behaviour as one whose config has already been written —
/// no special-cased divergence. The returned mode maps to the updater's launch
/// kind inside `egui_app::start_launch_update_check` (`auto` → hands-free
/// download+apply; `notify` → one-click banner).
fn launch_check_config() -> (bool, c0pl4nd_core::config::UpdateMode) {
    // The configured update settings (from the persisted config, or the canonical
    // default when no config exists yet). A check runs only when the mode opts in
    // (`notify`/`auto` or the legacy flag) AND the interval throttle says it is due
    // — so the default `notify` mode does not hit the GitHub API on every launch.
    let update = c0pl4nd_core::Config::default_path()
        .filter(|p| p.exists())
        .and_then(|p| {
            std::fs::read_to_string(&p)
                .ok()
                .and_then(|s| c0pl4nd_core::Config::from_toml(&s, &p).ok())
        })
        .map(|c| c.update)
        .unwrap_or_else(|| c0pl4nd_core::Config::default().update);
    let should_check = update.checks_on_launch() && update::check_due(update.check_interval_hours);
    (should_check, update.mode)
}

/// Whether the launch should pick a transparency-capable wgpu backend + adapter
/// (see [`prefer_backend_on_windows`] / [`install_transparency_adapter_selector`]).
/// The window is ALWAYS created transparent-capable now (`with_transparent(true)`)
/// and the single `opacity` slider drives the see-through level, so this is
/// unconditionally `true`: the launch always takes the see-through GPU path
/// (integrated / display-driving adapter) that composites transparency correctly
/// on the hybrid-GPU (Optimus) target.
fn launch_transparency_enabled() -> bool {
    true
}

/// The persisted `always_on_top` flag, read from the on-disk config at startup so
/// the viewport can be created already at the always-on-top window level. Mirrors
/// [`launch_gpu_preference`]: a missing / unreadable config yields `false` (the
/// opt-in default), never a crash.
fn launch_always_on_top() -> bool {
    c0pl4nd_core::Config::default_path()
        .filter(|p| p.exists())
        .and_then(|p| {
            std::fs::read_to_string(&p)
                .ok()
                .and_then(|s| c0pl4nd_core::Config::from_toml(&s, &p).ok())
        })
        .map(|c| c.always_on_top)
        .unwrap_or_default()
}

/// The persisted quake-mode block, read from the on-disk config at startup.
/// Mirrors [`launch_always_on_top`]: a missing / unreadable / malformed config
/// yields [`QuakeConfig::default`] — which is `enabled: false` — so a config
/// problem can never accidentally claim a global hotkey.
fn launch_quake_config() -> c0pl4nd_core::config::QuakeConfig {
    c0pl4nd_core::Config::default_path()
        .filter(|p| p.exists())
        .and_then(|p| {
            std::fs::read_to_string(&p)
                .ok()
                .and_then(|s| c0pl4nd_core::Config::from_toml(&s, &p).ok())
        })
        .map(|c| c.quake)
        .unwrap_or_default()
}

/// The persisted `graphics_gpu` preference, read from the on-disk config at
/// startup. Mirrors [`launch_transparency_enabled`]: a missing / unreadable config
/// yields the default [`GpuPreference::Auto`], never a crash. Fed into
/// [`apply_gpu_preference`], where `WGPU_POWER_PREF` still wins.
fn launch_gpu_preference() -> c0pl4nd_core::config::GpuPreference {
    c0pl4nd_core::Config::default_path()
        .filter(|p| p.exists())
        .and_then(|p| {
            std::fs::read_to_string(&p)
                .ok()
                .and_then(|s| c0pl4nd_core::Config::from_toml(&s, &p).ok())
        })
        .map(|c| c.graphics_gpu)
        .unwrap_or_default()
}

/// The persisted `graphics_backend` override, read from the on-disk config at
/// startup (BEFORE the GPU device is created). Mirrors
/// [`launch_transparency_enabled`]: a missing / unreadable config yields the
/// default [`GraphicsBackend::Auto`] (platform-smart choice), never a crash.
/// Fed into [`prefer_backend_on_windows`], where `WGPU_BACKEND` still wins.
fn launch_backend_override() -> c0pl4nd_core::config::GraphicsBackend {
    c0pl4nd_core::Config::default_path()
        .filter(|p| p.exists())
        .and_then(|p| {
            std::fs::read_to_string(&p)
                .ok()
                .and_then(|s| c0pl4nd_core::Config::from_toml(&s, &p).ok())
        })
        .map(|c| c.graphics_backend)
        .unwrap_or_default()
}

/// Choose the wgpu backend on Windows.
///
/// **Opaque window → force VULKAN.** Vulkan is immune to the DX12 font-atlas
/// `write_texture`→sample hazard that intermittently garbles the terminal grid on
/// some NVIDIA DX12 drivers (wgpu#1306 / #6829, DX12-only).
///
/// **Translucent window → do NOT force a backend; use eframe's DEFAULT backends,**
/// exactly like the sibling app SCR1B3 (identical eframe/egui-wgpu/wgpu stack),
/// which IS see-through on the same hybrid-GPU (Optimus) laptop. Forcing Vulkan was
/// the transparency bug: it pinned the integrated / display-driving GPU to its
/// VULKAN surface, whose `alpha_modes` are `[Opaque, Inherit]` (no
/// `PreMultiplied`/`PostMultiplied`), so egui-wgpu configures `Auto` → wgpu-core
/// resolves it to `Opaque` → the window composites solid BLACK. Meanwhile the
/// discrete NVIDIA Vulkan surface DOES expose `PreMultiplied`, but on Optimus it
/// renders off-screen and the copy to the Intel-driven display is opaque, so its
/// transparency never reaches the screen either. Leaving the DEFAULT backends lets
/// wgpu reach the surface/adapter path that actually composites (paired with the
/// integrated-GPU adapter selector + `LowPower` — see
/// [`install_transparency_adapter_selector`]).
///
/// `backend_override` (the persisted [`GraphicsBackend`](c0pl4nd_core::config::GraphicsBackend))
/// and `WGPU_BACKEND` still force a backend in BOTH modes — the in-app escape hatch
/// (e.g. a Vulkan-overlay crash → force DX12). Precedence: `WGPU_BACKEND` env >
/// config override > mode default (Vulkan opaque / eframe-default translucent).
/// Effective on Windows; inert elsewhere.
fn prefer_backend_on_windows(
    options: &mut eframe::NativeOptions,
    want_transparency: bool,
    backend_override: c0pl4nd_core::config::GraphicsBackend,
) {
    #[cfg(target_os = "windows")]
    {
        use eframe::wgpu::Backends;
        if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup
        {
            if let Some(backends) =
                resolve_backends(Backends::from_env(), backend_override, want_transparency)
            {
                setup.instance_descriptor.backends = backends;
            }
        }
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = want_transparency;
        let _ = backend_override;
        let _ = options; // used on every platform; backend default is correct off Windows
    }
}

/// Decide the wgpu `Backends` on Windows. `None` means "leave eframe's DEFAULT
/// backends untouched".
///
/// Precedence: `WGPU_BACKEND` env (one-off debug) > the persisted
/// `graphics_backend` config override > the mode default.
///
/// The two mode defaults each encode a REAL shipped bug, which is why this is
/// worth pinning:
/// * **Opaque → Vulkan** — dodges the NVIDIA DX12 glyph garble.
/// * **Translucent → leave eframe's default** — forcing Vulkan here is what caused
///   the opaque black window on the Optimus laptop.
///
/// Pure and parameterised on `env_override` (mirroring [`resolve_power_preference`])
/// so the precedence is unit-testable WITHOUT mutating process-global env — which
/// this binary could not do anyway under `#![deny(unsafe_code)]`. Extracting it is
/// what makes the logic reachable from a test at all; the caller
/// [`prefer_backend_on_windows`] only reads the env and applies the result, so
/// behaviour is unchanged.
#[cfg(target_os = "windows")]
fn resolve_backends(
    env_override: Option<eframe::wgpu::Backends>,
    backend_override: c0pl4nd_core::config::GraphicsBackend,
    want_transparency: bool,
) -> Option<eframe::wgpu::Backends> {
    use c0pl4nd_core::config::GraphicsBackend;
    use eframe::wgpu::Backends;
    let explicit = env_override.or(match backend_override {
        GraphicsBackend::Auto => None,
        GraphicsBackend::Dx12 => Some(Backends::DX12),
        GraphicsBackend::Vulkan => Some(Backends::VULKAN),
        GraphicsBackend::Gl => Some(Backends::GL),
    });
    match explicit {
        Some(backends) => Some(backends),
        // Opaque default: Vulkan (dodges the NVIDIA DX12 glyph garble).
        None if !want_transparency => Some(Backends::VULKAN),
        // Translucent default: leave eframe's DEFAULT backends untouched
        // (SCR1B3-parity). Forcing Vulkan here is what caused the opaque black
        // window on the Optimus laptop.
        None => None,
    }
}

/// Resolve the wgpu `PowerPreference` from the persisted GPU preference, the
/// `WGPU_POWER_PREF` env override, and the platform default. Precedence:
/// `env_override` (one-off debug) > the explicit config choice > `default`. `Auto`
/// defers to `default`. Pure + cross-platform so it is unit-testable everywhere.
fn resolve_power_preference(
    gpu: c0pl4nd_core::config::GpuPreference,
    env_override: Option<eframe::wgpu::PowerPreference>,
    default: eframe::wgpu::PowerPreference,
) -> eframe::wgpu::PowerPreference {
    use c0pl4nd_core::config::GpuPreference;
    use eframe::wgpu::PowerPreference;
    if let Some(env) = env_override {
        return env; // WGPU_POWER_PREF wins over the persisted setting
    }
    match gpu {
        GpuPreference::Auto => default,
        GpuPreference::Integrated => PowerPreference::LowPower,
        GpuPreference::Discrete => PowerPreference::HighPerformance,
    }
}

/// Apply the persisted GPU preference to the wgpu setup by setting the adapter
/// `power_preference`. The default depends on the window mode:
///
/// * **Translucent → `LowPower`** — the integrated / display-driving GPU, matching
///   the sibling app SCR1B3 (which is see-through on the same Optimus laptop). On a
///   hybrid machine only the iGPU composites a see-through window; the discrete GPU
///   renders off-screen and its result is copied back opaque.
/// * **Opaque → `HighPerformance`** — the discrete GPU, for terminal-glyph
///   throughput.
///
/// `Auto` defers to that mode default; an explicit `graphics_gpu` config choice and
/// `WGPU_POWER_PREF` still win via [`resolve_power_preference`]. This is a HINT —
/// the authoritative transparency fix is the adapter selector
/// ([`install_transparency_adapter_selector`]), which overrides the hint — but the
/// hint keeps the opaque path on the discrete GPU. No-op unless the setup is
/// `CreateNew` (eframe is creating its own device).
fn apply_gpu_preference(
    options: &mut eframe::NativeOptions,
    gpu: c0pl4nd_core::config::GpuPreference,
    want_transparency: bool,
) {
    use eframe::wgpu::PowerPreference;
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        let env_override = PowerPreference::from_env();
        let mode_default = if want_transparency {
            PowerPreference::LowPower
        } else {
            PowerPreference::HighPerformance
        };
        setup.power_preference = resolve_power_preference(gpu, env_override, mode_default);
    }
}

/// Install a `native_adapter_selector` that picks the INTEGRATED / display-driving
/// GPU when a see-through window is requested — the real fix for the "opaque black"
/// window on a hybrid-GPU (Optimus) laptop.
///
/// The selector is handed BOTH the enumerated adapters and the compatible surface,
/// so it queries each adapter's real `surface.get_capabilities(&adapter).alpha_modes`,
/// logs them (plus the mode egui-wgpu will configure) to `<config_dir>/gpu-diag.log`,
/// and picks via [`gpu_diag::choose_display_driving_adapter`] — integrated GPU
/// first, then richest transparent capability. This deliberately does NOT prefer the
/// discrete GPU even though its Vulkan surface advertises `PreMultiplied`: on Optimus
/// the discrete GPU renders off-screen and its result is copied back OPAQUE, so only
/// the display-driving iGPU composites see-through (the gpu-diag.log evidence from the
/// first attempt). Overrides wgpu's `power_preference` HINT with a hard device-class
/// rule, matching why SCR1B3 (`LowPower` → the iGPU) is see-through on the same box.
///
/// No-op when transparency is off (the opaque path keeps the discrete GPU for
/// terminal-glyph throughput). Correct on any native platform (single-adapter
/// machines pass through; the capability query works on every native backend).
fn install_transparency_adapter_selector(
    options: &mut eframe::NativeOptions,
    want_transparency: bool,
) {
    if !want_transparency {
        return;
    }
    use std::sync::Arc;
    if let eframe::egui_wgpu::WgpuSetup::CreateNew(setup) = &mut options.wgpu_options.wgpu_setup {
        gpu_diag::begin_session(&format!(
            "transparency=ON backends={:?} (default=unforced when Auto)",
            setup.instance_descriptor.backends
        ));
        setup.native_adapter_selector = Some(Arc::new(
            move |adapters: &[eframe::wgpu::Adapter],
                  surface: Option<&eframe::wgpu::Surface<'_>>| {
                use eframe::wgpu::CompositeAlphaMode;
                gpu_diag::log_line(&format!(
                    "adapter enumeration: {} candidate(s), surface_present={}",
                    adapters.len(),
                    surface.is_some()
                ));
                let mut metas = Vec::with_capacity(adapters.len());
                for adapter in adapters {
                    let info = adapter.get_info();
                    let modes = surface
                        .map(|s| s.get_capabilities(adapter).alpha_modes)
                        .unwrap_or_default();
                    let mode_names: Vec<&str> = modes
                        .iter()
                        .map(|m| gpu_diag::alpha_mode_name(*m))
                        .collect();
                    gpu_diag::log_line(&format!(
                        "  adapter: name='{}' type={} backend={:?} driver='{} {}' \
                         alpha_modes=[{}] egui_wgpu_would_configure={} transparent={}",
                        info.name,
                        gpu_diag::device_type_name(info.device_type),
                        info.backend,
                        info.driver,
                        info.driver_info,
                        mode_names.join(", "),
                        gpu_diag::alpha_mode_name(gpu_diag::egui_wgpu_configured_mode(&modes)),
                        gpu_diag::supports_transparency(&modes),
                    ));
                    metas.push(gpu_diag::AdapterMeta {
                        device_type: info.device_type,
                        premultiplied: modes.contains(&CompositeAlphaMode::PreMultiplied),
                        postmultiplied: modes.contains(&CompositeAlphaMode::PostMultiplied),
                        inherit: modes.contains(&CompositeAlphaMode::Inherit),
                    });
                }
                let idx = gpu_diag::choose_display_driving_adapter(&metas).unwrap_or(0);
                match adapters.get(idx) {
                    Some(chosen) => {
                        let info = chosen.get_info();
                        let modes = surface
                            .map(|s| s.get_capabilities(chosen).alpha_modes)
                            .unwrap_or_default();
                        gpu_diag::log_line(&format!(
                            "  CHOSEN adapter[{idx}]: name='{}' type={} backend={:?} \
                             configured_alpha_mode={}",
                            info.name,
                            gpu_diag::device_type_name(info.device_type),
                            info.backend,
                            gpu_diag::alpha_mode_name(gpu_diag::egui_wgpu_configured_mode(&modes)),
                        ));
                        Ok(chosen.clone())
                    }
                    None => Err("no wgpu adapter available for a transparent window".to_string()),
                }
            },
        ));
    }
}

/// Decode the embedded sigil PNG into an eframe window icon. Returns `None` on a
/// decode failure (the caller falls back to the platform default).
fn load_app_icon() -> Option<egui::IconData> {
    // `packaging/windows/c0pl4nd-256.png` is the 256px sigil (same mark as the
    // embedded `.ico`), included at compile time so the icon ships in the binary.
    const PNG: &[u8] = include_bytes!("../../../packaging/windows/c0pl4nd-256.png");
    let img = image::load_from_memory(PNG).ok()?.into_rgba8();
    let (width, height) = img.dimensions();
    Some(egui::IconData {
        rgba: img.into_raw(),
        width,
        height,
    })
}

#[cfg(test)]
mod tests {
    use super::resolve_power_preference;

    /// Reachability + correctness guard for the `win_chrome` Snap-Layouts wiring.
    ///
    /// `main` registers a begin-pass hook (`cc.egui_ctx.on_begin_pass(...)`) whose
    /// body is `win_chrome::tick(ui.ctx())`, and `tick` calls
    /// `win_chrome::publish_from_geometry(content_right, ppp)`. This test drives
    /// that EXACT published-rect function (the one the shipping binary calls every
    /// frame) and asserts it produces the maximize-button rect the un-editable
    /// `chrome.rs` caption cluster paints — so the module is proven reachable from
    /// egui_main and geometrically correct, not merely defined. The compiled live
    /// call site above (plus `-D warnings` dead-code) is the wiring; this is its
    /// behavioural assertion.
    #[test]
    fn win_chrome_publish_is_wired_and_matches_the_caption_layout() {
        crate::win_chrome::publish_from_geometry(1100.0, 1.0);
        let r = crate::win_chrome::published_rect()
            .expect("the shipping-path publish must produce a non-empty maximize rect");
        // right_edge = 1100 - 8 = 1092; maximize right = 1092 - 44 = 1048; left 1006.
        assert_eq!(
            (r.left, r.right),
            (1006, 1048),
            "maximize-button x mirrors chrome.rs"
        );
        // titlebar 40px, button 28px tall, centred → top 6, bottom 34.
        assert_eq!(
            (r.top, r.bottom),
            (6, 34),
            "maximize-button y mirrors chrome.rs"
        );
    }

    /// Quake mode and the tray icon both toggle the SAME window, so they must
    /// share one state model or they will fight: if quake kept a private "is it
    /// dropped?" latch, a tray minimize would invalidate it and the next hotkey
    /// press would retract an already-hidden window — the user presses the key and
    /// nothing appears.
    ///
    /// Both decide from the live OS state instead. This pins the contract across
    /// all eight `(visible, minimized, foreground)` combinations: whenever quake
    /// reads the window as out-of-view it drops it DOWN (never retracts), and the
    /// tray — fed the identical predicate — restores it. Only the one state where
    /// the window is genuinely in view AND focused retracts, and the resulting
    /// hidden window is exactly what the tray then reads as restorable.
    ///
    /// This is a genuine cross-module test: `quake` and `tray` are separate
    /// `#[path]` modules that never import each other, so nothing but this
    /// assertion keeps their state models aligned.
    #[test]
    fn quake_and_tray_agree_on_window_state() {
        use crate::quake::{is_out_of_view, quake_action, QuakeAction};
        use crate::tray::{toggle_action, ToggleAction};

        for visible in [false, true] {
            for minimized in [false, true] {
                for foreground in [false, true] {
                    // Derive the expectation INDEPENDENTLY rather than calling
                    // `is_out_of_view` and feeding both sides from it — that would
                    // make the test self-referential (cutting `is_out_of_view`
                    // would move both sides together and the assertions would still
                    // pass, proving nothing). Restating the predicate here is what
                    // makes the check adversarial; the assert below then pins the
                    // production function to this independent statement.
                    let out = !visible || minimized;
                    assert_eq!(
                        is_out_of_view(visible, minimized),
                        out,
                        "the production out-of-view predicate must be `!visible || \
                         minimized` ({visible},{minimized})"
                    );
                    let q = quake_action(visible, minimized, foreground);
                    let t = toggle_action(out);
                    if out {
                        assert_eq!(
                            q,
                            QuakeAction::DropDown,
                            "quake must SHOW an out-of-view window ({visible},{minimized},{foreground})"
                        );
                        assert_eq!(
                            t,
                            ToggleAction::Restore,
                            "the tray must agree the window is out of view"
                        );
                    } else {
                        assert_eq!(
                            t,
                            ToggleAction::Minimize,
                            "the tray must agree the window is in view"
                        );
                        // In view: quake retracts only when it also has focus.
                        let expected = if foreground {
                            QuakeAction::Retract
                        } else {
                            QuakeAction::DropDown
                        };
                        assert_eq!(q, expected, "({visible},{minimized},{foreground})");
                    }
                }
            }
        }

        // The hand-off itself: a quake retract HIDES the window (visible = false),
        // and the tray's very next click must therefore restore it — the escape
        // hatch stays correct after quake has run.
        assert_eq!(quake_action(true, false, true), QuakeAction::Retract);
        assert_eq!(
            toggle_action(is_out_of_view(false, false)),
            ToggleAction::Restore,
            "after a quake retract the tray must restore, not minimize again"
        );
        // And symmetrically: after a tray minimize the next hotkey must drop down.
        assert_eq!(toggle_action(false), ToggleAction::Minimize);
        assert_eq!(quake_action(true, true, false), QuakeAction::DropDown);
    }

    /// Reachability + correctness guard for the quake wiring, mirroring the
    /// `win_chrome` test above: `main` calls `quake::init(&ctx,
    /// &launch_quake_config())`, so this drives that EXACT config reader and the
    /// EXACT geometry function the hotkey path uses, and asserts the two
    /// safety-critical properties — the shipped default claims no global hotkey,
    /// and the drop-down rect the shipping path computes clears the taskbar.
    #[test]
    fn quake_launch_config_defaults_off_and_geometry_clears_the_taskbar() {
        // `launch_quake_config` is what the live call site feeds `quake::init`. On
        // a machine with no config (CI) it must yield the default; on a developer
        // machine with one it must still not be *enabled* by accident, so assert
        // the invariant that actually matters: a default-constructed block is off.
        let def = c0pl4nd_core::config::QuakeConfig::default();
        assert!(!def.enabled, "quake mode must ship OFF");
        let launched = super::launch_quake_config();
        // Reading it must never panic and must produce a usable fraction.
        assert!(
            (0.1..=1.0).contains(&launched.effective_height_fraction()),
            "the launch-path height fraction must always be usable"
        );

        // The geometry the shipping hotkey path computes, on a 1920x1080 monitor
        // with a 40px bottom taskbar.
        let work = crate::quake::ScreenRect::new(0, 0, 1920, 1040);
        let r = crate::quake::quake_rect(work, def.effective_height_fraction())
            .expect("the shipping-path geometry must produce a rect for a real work area");
        assert_eq!(r.top, 0, "anchored to the top of the work area");
        assert_eq!(r.width(), 1920, "full work-area width");
        assert!(
            r.bottom <= work.bottom,
            "the drop-down must never overlap the taskbar"
        );
        // The default combo the same path would register.
        let spec = crate::quake::parse_hotkey(&def.hotkey)
            .expect("the shipped default combo must parse");
        assert_ne!(spec.modifiers, 0, "the default combo carries a modifier");
    }

    #[test]
    fn gpu_preference_maps_and_env_wins() {
        use c0pl4nd_core::config::GpuPreference;
        use eframe::wgpu::PowerPreference;
        // Auto defers to the platform default.
        assert_eq!(
            resolve_power_preference(GpuPreference::Auto, None, PowerPreference::HighPerformance),
            PowerPreference::HighPerformance,
        );
        // Integrated → low-power (the iGPU escape hatch); Discrete → high-perf.
        assert_eq!(
            resolve_power_preference(
                GpuPreference::Integrated,
                None,
                PowerPreference::HighPerformance
            ),
            PowerPreference::LowPower,
        );
        assert_eq!(
            resolve_power_preference(GpuPreference::Discrete, None, PowerPreference::LowPower),
            PowerPreference::HighPerformance,
        );
        // WGPU_POWER_PREF env overrides even an explicit config choice.
        assert_eq!(
            resolve_power_preference(
                GpuPreference::Discrete,
                Some(PowerPreference::LowPower),
                PowerPreference::HighPerformance,
            ),
            PowerPreference::LowPower,
            "env override wins over the persisted setting",
        );
    }

    // -- backend selection (Windows) ------------------------------------
    //
    // Both mode defaults below encode a REAL shipped bug, and neither was pinned
    // by a test before: the logic lived inline in `prefer_backend_on_windows`,
    // reachable only through `Backends::from_env()`, so nothing could reach it
    // without mutating process-global env.

    #[cfg(target_os = "windows")]
    #[test]
    fn opaque_launch_defaults_to_vulkan_to_dodge_the_dx12_glyph_garble() {
        use c0pl4nd_core::config::GraphicsBackend;
        use eframe::wgpu::Backends;
        assert_eq!(
            super::resolve_backends(None, GraphicsBackend::Auto, false),
            Some(Backends::VULKAN),
            "an opaque window with no explicit choice must force Vulkan",
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn translucent_launch_leaves_eframe_default_backends_alone() {
        use c0pl4nd_core::config::GraphicsBackend;
        // The Optimus regression: forcing Vulkan on the translucent path produced
        // an opaque BLACK window. `None` here means "do not touch eframe's
        // default" -- the fix. If this ever returns Some(VULKAN), that bug is back.
        assert_eq!(
            super::resolve_backends(None, GraphicsBackend::Auto, true),
            None,
            "a translucent window must inherit eframe's default backends",
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn a_config_backend_override_wins_over_both_mode_defaults() {
        use c0pl4nd_core::config::GraphicsBackend;
        use eframe::wgpu::Backends;
        // The in-app escape hatch (e.g. a Vulkan-overlay crash -> force DX12) must
        // work in BOTH modes, including the translucent one that otherwise returns
        // None.
        assert_eq!(
            super::resolve_backends(None, GraphicsBackend::Dx12, true),
            Some(Backends::DX12),
        );
        assert_eq!(
            super::resolve_backends(None, GraphicsBackend::Dx12, false),
            Some(Backends::DX12),
            "the override must beat the opaque Vulkan default too",
        );
        assert_eq!(
            super::resolve_backends(None, GraphicsBackend::Gl, true),
            Some(Backends::GL),
        );
        assert_eq!(
            super::resolve_backends(None, GraphicsBackend::Vulkan, true),
            Some(Backends::VULKAN),
        );
    }

    #[cfg(target_os = "windows")]
    #[test]
    fn wgpu_backend_env_wins_over_the_config_override() {
        use c0pl4nd_core::config::GraphicsBackend;
        use eframe::wgpu::Backends;
        // Documented precedence: env > config > mode default. Passing the env value
        // as a parameter is what lets this be asserted at all -- the binary denies
        // unsafe_code, so a test cannot call the unsafe `std::env::set_var`.
        assert_eq!(
            super::resolve_backends(Some(Backends::GL), GraphicsBackend::Dx12, false),
            Some(Backends::GL),
            "WGPU_BACKEND must beat the persisted graphics_backend override",
        );
    }
}
