//! `C0pl4ndApp` W1TN3SS crash-consent + report-issue dialogs.
//!
//! The per-launch crash-consent overlay (editable preview, equal-weight Send /
//! Don't-send, remembered Always/Never) and the report-an-issue form, both gated
//! through the SDK consent path. Grouped out of the C0pl4ndApp god-impl; behaviour
//! unchanged. Nothing transmits without an explicit click + consent token.

use eframe::egui;

impl super::C0pl4ndApp {
    /// Render the W1TN3SS per-launch crash-consent dialog, if a spooled crash
    /// report is pending (drained by [`Self::drain_crash_spool`] on launch when
    /// the crash stream is `AskEachTime`). Presents ONE report at a time with an
    /// EDITABLE preview + equal-weight Send / Don't-send; persists a remembered
    /// "Always"/"Never" choice into the config. A no-op when nothing is pending
    /// (the opted-out / opted-in-but-empty case) — so the default experience is
    /// untouched. Nothing transmits until the user clicks Send (which mints the
    /// consent token inside the SDK-gated `consent_and_send`).
    pub(crate) fn render_crash_consent(&mut self, ctx: &egui::Context) {
        if !self.crash_consent.has_pending() {
            return;
        }
        let mut do_send = false;
        let mut do_decline = false;
        egui::Window::new("Send this crash report?")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    "C0PL4ND captured a crash. You can review and edit the report \
                     below before deciding — nothing is sent unless you choose Send.",
                );
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .max_height(220.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(self.crash_consent.edited_text_mut())
                                .desired_rows(8)
                                .desired_width(f32::INFINITY)
                                .font(egui::TextStyle::Monospace),
                        );
                    });
                ui.add_space(8.0);
                ui.label("Remember my choice for future crashes:");
                let remember = self.crash_consent.remember_mut();
                ui.horizontal(|ui| {
                    ui.radio_value(
                        remember,
                        Some(crate::reporting::RememberChoice::JustThisTime),
                        "Just this time",
                    );
                    ui.radio_value(
                        remember,
                        Some(crate::reporting::RememberChoice::Always),
                        "Always send",
                    );
                    ui.radio_value(
                        remember,
                        Some(crate::reporting::RememberChoice::Never),
                        "Never",
                    );
                });
                ui.add_space(8.0);
                // Equal-weight Send / Don't-send — no dark-pattern asymmetry.
                ui.horizontal(|ui| {
                    if ui.button("Send report").clicked() {
                        do_send = true;
                    }
                    if ui.button("Don't send").clicked() {
                        do_decline = true;
                    }
                });
            });
        // Persist a remembered Always/Never BEFORE advancing (so the choice
        // applies from the next launch). JustThisTime persists nothing.
        let remembered = *self.crash_consent.remember_mut();
        if do_send {
            if let Some(choice) = remembered {
                self.apply_remember_choice(choice);
            }
            let _ = self.crash_consent.consent_and_send();
        } else if do_decline {
            if let Some(choice) = remembered {
                self.apply_remember_choice(choice);
            }
            self.crash_consent.decline_and_discard();
        }
    }

    /// Persist a remembered crash-consent choice into the config (graduating the
    /// crash stream to `Always` or `Off`), then save the config so the next
    /// launch honours it. `JustThisTime` persists nothing. Best-effort save.
    ///
    /// The disk write is REAL-WINDOW-ONLY, exactly like every other config-save
    /// site (`persist_config_change`, `prepare_shutdown`, the settings handler):
    /// the headless `egui_kittest` harness has `live_window == false`, so a test
    /// driving this dialog never writes the user's real
    /// `%APPDATA%\c0pl4nd\config.toml` (test pollution). This mirrors the spool
    /// I/O of this very dialog, which is already rooted at an explicit
    /// [`crate::reporting::CrashConsentState::set_config_dir`] rather than a
    /// resolved global. The in-memory graduation is what the tests observe.
    fn apply_remember_choice(&mut self, choice: crate::reporting::RememberChoice) {
        let path = self.remember_save_path();
        self.apply_remember_choice_to(choice, path.as_deref());
    }

    /// The config file this dialog's remembered-choice save targets: the platform
    /// config path in a REAL window, and NOTHING when headless. Split out as its own
    /// decision so it is unit-testable WITHOUT redirecting the process-global env
    /// vars `Config::default_path()` reads (the `reporting` tests already serialise
    /// those behind their own lock; a second, unrelated lock here would not compose).
    /// Short-circuits: when headless it never even resolves the global.
    fn remember_save_path(&self) -> Option<std::path::PathBuf> {
        self.live_window
            .then(c0pl4nd_core::Config::default_path)
            .flatten()
    }

    /// Pure core of the remembered-choice apply, parameterised on the save path so
    /// it is unit-testable (the real entry resolves `Config::default_path()`, and
    /// only in a real window). The in-memory graduation ALWAYS happens — it is the
    /// load-bearing behaviour the headless tests observe; a `None` path writes
    /// nothing at all. A write failure surfaces as a toast and never blocks the
    /// in-memory apply. `JustThisTime` (`persisted_mode() == None`) changes nothing.
    fn apply_remember_choice_to(
        &mut self,
        choice: crate::reporting::RememberChoice,
        path: Option<&std::path::Path>,
    ) {
        let Some(mode) = choice.persisted_mode() else {
            return;
        };
        self.config.reporting.streams.crash_reports = mode;
        let Some(path) = path else {
            return;
        };
        self.save_config_guarded(path, "Your reporting choice");
    }

    /// Render the W1TN3SS manual "Report an issue" dialog, opened from the
    /// titlebar script menu ([`crate::issue_intake::IssueIntakeState::open_fresh`]
    /// sets `open`). User-initiated only: renders only when `issue_intake.open`.
    /// Builds a prefilled GitHub Issue-Form deep link (or a clipboard / mailto
    /// fallback); diagnostics are OFF unless the user ticks them; the body is
    /// previewable + editable before anything leaves. Nothing transmits — it
    /// hands a URL to the browser / a body to the clipboard on an explicit click.
    pub(crate) fn render_report_issue(&mut self, ctx: &egui::Context) {
        if !self.issue_intake.open {
            return;
        }
        let repo = self.config.reporting.issue_intake.repo.clone();
        let alias = self.config.reporting.issue_intake.mailto_alias.clone();
        let renderer = crate::issue_intake::RENDERER;

        // Decisions deferred past the closure borrow (like paste_confirm_window).
        let mut do_open = false;
        let mut do_mailto = false;
        let mut do_close = false;
        egui::Window::new("Report an issue")
            .collapsible(false)
            .resizable(true)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(
                    "Open a prefilled GitHub issue. Nothing is sent automatically — \
                     your browser opens the issue form for you to review and submit.",
                );
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.label("Kind:");
                    for kind in crate::issue_intake::IssueKind::ALL {
                        ui.radio_value(&mut self.issue_intake.kind, kind, kind.display());
                    }
                });
                ui.add_space(6.0);
                ui.label("Describe the issue:");
                egui::ScrollArea::vertical()
                    .max_height(160.0)
                    .show(ui, |ui| {
                        ui.add(
                            egui::TextEdit::multiline(&mut self.issue_intake.description)
                                .desired_rows(5)
                                .desired_width(f32::INFINITY),
                        );
                    });
                ui.add_space(6.0);
                ui.checkbox(
                    &mut self.issue_intake.include_diagnostics,
                    "Include non-identifying diagnostics (app version, OS, renderer)",
                );
                ui.add_space(6.0);
                // Faithful preview of EXACTLY what will be sent.
                let preview = self.issue_intake.preview_body(renderer);
                ui.label("Preview:");
                egui::ScrollArea::vertical()
                    .id_salt("issue_preview")
                    .max_height(120.0)
                    .show(ui, |ui| {
                        ui.add(egui::Label::new(egui::RichText::new(&preview).monospace()).wrap());
                    });
                ui.add_space(8.0);
                if !self.issue_intake.fits_url_length(&repo, renderer) {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(
                                "This report is long — \"Open on GitHub\" will copy it \
                                 to your clipboard to paste into a blank issue instead.",
                            )
                            .weak()
                            .small(),
                        )
                        .wrap(),
                    );
                    ui.add_space(4.0);
                }
                // Show the last outcome (status line).
                if let Some(outcome) = &self.issue_intake.last_outcome {
                    let msg = match outcome {
                        crate::issue_intake::IntakeOutcome::OpenedDeepLink => {
                            "Opened the issue form in your browser."
                        }
                        crate::issue_intake::IntakeOutcome::CopiedToClipboard => {
                            "Copied the report to your clipboard — paste it into a new issue."
                        }
                        crate::issue_intake::IntakeOutcome::OpenedMailto => {
                            "Opened your mail client."
                        }
                        crate::issue_intake::IntakeOutcome::Failed(_) => {
                            "That didn't work. You can copy the report to your clipboard and \
                             paste it into a new GitHub issue instead."
                        }
                    };
                    ui.add(egui::Label::new(egui::RichText::new(msg).small()).wrap());
                    ui.add_space(4.0);
                }
                ui.horizontal(|ui| {
                    if ui.button("Open on GitHub").clicked() {
                        do_open = true;
                    }
                    // Offered ONLY when an operator configured a real alias. The
                    // shipped default is empty (no mailbox is compiled into a
                    // public binary), and a `mailto:` with no recipient opens an
                    // empty compose window that silently goes nowhere — worse
                    // than not offering the route. "Open on GitHub" and the
                    // clipboard fallback are always available.
                    if email_fallback_offered(&alias) && ui.button("Email instead").clicked() {
                        do_mailto = true;
                    }
                    if ui.button("Close").clicked() {
                        do_close = true;
                    }
                });
            });

        if do_open {
            let req = self.issue_intake.request(&repo, renderer);
            let outcome = match crate::issue_intake::open_or_copy(&req) {
                crate::issue_intake::IntakeAction::Opened(o) => o,
                crate::issue_intake::IntakeAction::CopyToClipboard(body) => {
                    // C0PL4ND has no `arboard`: copy via egui's clipboard here.
                    ctx.copy_text(body);
                    crate::issue_intake::IntakeOutcome::CopiedToClipboard
                }
            };
            crate::issue_intake::log_outcome(&outcome);
            self.issue_intake.last_outcome = Some(outcome);
        } else if do_mailto {
            let req = self.issue_intake.request(&repo, renderer);
            let outcome = crate::issue_intake::open_mailto(&alias, &req.title, &req.body);
            crate::issue_intake::log_outcome(&outcome);
            self.issue_intake.last_outcome = Some(outcome);
        } else if do_close {
            self.issue_intake.open = false;
        }
    }
}

/// Whether the "Email instead" fallback should be offered at all.
///
/// `false` for an absent or whitespace-only alias. The shipped default IS empty
/// — no mailbox is compiled into a public binary (see
/// `IssueIntakeConfig::mailto_alias`) — so on a stock install this route is
/// simply not shown, and the GitHub Issue form + clipboard fallback carry the
/// whole flow.
///
/// Pure, so the decision is testable without a live window.
pub(crate) fn email_fallback_offered(alias: &str) -> bool {
    !alias.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::super::C0pl4ndApp;
    use super::email_fallback_offered;
    use crate::reporting::{RememberChoice, ReportingMode};
    use std::path::Path;

    // These tests deliberately do NOT write the config-dir-rooting env vars
    // (`APPDATA` / `XDG_CONFIG_HOME` / `HOME`). `reporting.rs`'s tests redirect
    // those behind their OWN private lock, which this module cannot reach; a
    // second unrelated lock would not compose, and the two would corrupt each
    // other. Instead the path DECISION (`remember_save_path`) and the path USE
    // (`apply_remember_choice_to`) are asserted separately, which needs no env
    // mutation at all and yields the same end-to-end guarantee.

    /// The email route is offered only for a REAL alias — and the shipped
    /// default is not one.
    ///
    /// The last assertion is the load-bearing half: it reads the live default
    /// rather than a literal, so re-introducing an address into the shipped
    /// config turns this test red instead of silently re-publishing a mailbox.
    #[test]
    fn the_email_fallback_is_offered_only_for_a_configured_alias() {
        assert!(email_fallback_offered("support@example.org"));
        assert!(!email_fallback_offered(""), "no alias -> no email route");
        assert!(
            !email_fallback_offered("   \t "),
            "whitespace is not an address"
        );
        assert!(
            !email_fallback_offered(
                &c0pl4nd_core::config::IssueIntakeConfig::default().mailto_alias
            ),
            "the SHIPPED default must not offer an email route — it carries no \
             mailbox, and a mailto: with no recipient goes nowhere"
        );
    }

    fn headless_app() -> C0pl4ndApp {
        C0pl4ndApp::bootstrap_with(c0pl4nd_core::Config::default())
    }

    /// Read the crash-stream mode back out of a config file ON DISK, so the
    /// persistence assertions observe what a NEXT LAUNCH would actually load
    /// rather than the in-memory struct we just mutated.
    fn crash_mode_on_disk(path: &Path) -> ReportingMode {
        let body = std::fs::read_to_string(path).expect("config file readable");
        c0pl4nd_core::Config::from_toml(&body, path)
            .expect("config file parses")
            .reporting
            .streams
            .crash_reports
    }

    /// REGRESSION (the real bug), half 1 of 2. A headless app — `live_window ==
    /// false`, which is what `bootstrap_with` and every `egui_kittest` harness
    /// produce — selects NO save path at all, so a remembered crash-consent choice
    /// cannot reach the user's real `%APPDATA%\c0pl4nd\config.toml`. Before the fix
    /// `apply_remember_choice` resolved the GLOBAL `Config::default_path()` and
    /// called `save_to` UNCONDITIONALLY, so merely exercising this path from a test
    /// wrote the developer's real config file. Paired with
    /// [`a_none_path_applies_in_memory_and_writes_nothing`] (which proves a `None`
    /// path writes nothing), this composes to the full end-to-end guarantee without
    /// this test having to mutate any process-global env var.
    #[test]
    fn a_headless_app_selects_no_save_path() {
        let app = headless_app();
        assert!(!app.live_window, "precondition: bootstrap_with is headless");
        assert_eq!(
            app.remember_save_path(),
            None,
            "a headless run must target no config file — it would be the user's real one"
        );
    }

    /// REGRESSION half 2 of 2 — the CONTROL. The gate must not be "never save": in a
    /// REAL window the remembered choice still targets the platform config path, so
    /// the next launch honours it. Without this, "fixing" the bug by deleting the
    /// save outright would pass [`a_headless_app_selects_no_save_path`] — this is
    /// what makes that test a real safeguard rather than a rubber stamp.
    #[test]
    fn a_real_window_targets_the_platform_config_path() {
        let mut app = headless_app();
        app.live_window = true;
        assert_eq!(
            app.remember_save_path(),
            c0pl4nd_core::Config::default_path(),
            "a real window must persist to the platform config path"
        );
        assert!(
            app.remember_save_path().is_some(),
            "precondition: this host resolves a platform config path, so the \
             assertion above is not a vacuous None == None"
        );
    }

    /// "Always send" graduates the crash stream to `Always` and round-trips
    /// through the file a next launch reads. Drives the injectable seam directly.
    #[test]
    fn always_graduates_the_crash_stream_and_round_trips() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let path = tmp.path().join("config.toml");
        let mut app = headless_app();

        app.apply_remember_choice_to(RememberChoice::Always, Some(&path));

        assert_eq!(
            app.config.reporting.streams.crash_reports,
            ReportingMode::Always
        );
        assert_eq!(crash_mode_on_disk(&path), ReportingMode::Always);
    }

    /// A real shipping writer routes through the guarded write seam, so a
    /// session whose config could not be read sets the user's file aside rather
    /// than replacing it with defaults.
    ///
    /// This is the WIRING half of the fix: the seam existing is worth nothing if
    /// the five writers still call `self.config.save_to` directly. Asserted here
    /// because "remember my choice" is a writer that takes an explicit path and
    /// needs no live window, so it can be driven headlessly end to end.
    #[test]
    fn a_writer_sets_an_unreadable_config_aside_instead_of_overwriting_it() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let path = tmp.path().join("config.toml");
        let bak = tmp.path().join("config.toml.bak");
        // Values that live ONLY in this file — after a failed load the app's
        // in-memory config is defaults, so nothing here is recoverable from
        // memory. `opacity = 1.5` is what made the load fail.
        let original = "theme = \"ghost-paper\"\nopacity = 1.5\n";
        std::fs::write(&path, original).expect("seed");
        let mut app = headless_app();
        app.config_unreadable = true;

        app.apply_remember_choice_to(RememberChoice::Always, Some(&path));

        assert_eq!(
            std::fs::read_to_string(&bak).expect("the original must be set aside"),
            original,
            "the user's settings must survive the write that replaced them"
        );
        assert_eq!(
            crash_mode_on_disk(&path),
            ReportingMode::Always,
            "and the choice still persists"
        );
    }

    /// "Never" sets the crash stream to `Off` — the opt-OUT must persist just as
    /// firmly as the opt-in, or a user who declined keeps being asked.
    ///
    /// The stream is put at `Always` FIRST: `Off` is the built-in default, so
    /// starting from a default config would make the assertion pass even if this
    /// method did nothing at all.
    #[test]
    fn never_turns_the_crash_stream_off_and_round_trips() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let path = tmp.path().join("config.toml");
        let mut app = headless_app();
        app.config.reporting.streams.crash_reports = ReportingMode::Always;

        app.apply_remember_choice_to(RememberChoice::Never, Some(&path));

        assert_eq!(
            app.config.reporting.streams.crash_reports,
            ReportingMode::Off
        );
        assert_eq!(crash_mode_on_disk(&path), ReportingMode::Off);
    }

    /// "Just this time" persists NOTHING: no mode change and no file, so the next
    /// crash asks again. The stream is put at `Always` first so "unchanged" is a
    /// value this method would have had to actively overwrite to fail.
    #[test]
    fn just_this_time_persists_nothing() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let path = tmp.path().join("config.toml");
        let mut app = headless_app();
        app.config.reporting.streams.crash_reports = ReportingMode::Always;

        app.apply_remember_choice_to(RememberChoice::JustThisTime, Some(&path));

        assert_eq!(
            app.config.reporting.streams.crash_reports,
            ReportingMode::Always,
            "JustThisTime must not change the stream"
        );
        assert!(!path.exists(), "JustThisTime must not write a config file");
    }

    /// A save failure is BEST-EFFORT: it surfaces as a toast and never blocks the
    /// live in-memory apply. The unwritable path is a `config.toml` whose PARENT is
    /// a regular FILE, so `save_to`'s `create_dir_all` cannot succeed.
    #[test]
    fn a_save_failure_surfaces_a_toast_and_keeps_the_in_memory_apply() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let blocker = tmp.path().join("not-a-dir");
        std::fs::write(&blocker, b"i am a file, not a directory").expect("write blocker");
        let path = blocker.join("config.toml");

        let mut app = headless_app();
        app.apply_remember_choice_to(RememberChoice::Always, Some(&path));

        assert_eq!(
            app.config.reporting.streams.crash_reports,
            ReportingMode::Always,
            "a write failure must never block the live in-memory apply"
        );
        let toast = app.toast.expect("a failed save must surface a toast");
        assert!(
            toast.contains("couldn't be saved"),
            "the toast must name the persist failure: {toast:?}"
        );
        assert!(
            toast.contains("Your reporting choice"),
            "the toast must name WHAT failed to save: {toast:?}"
        );
    }

    /// A `None` path writes nothing but still applies in memory — the exact shape
    /// the headless entry relies on.
    #[test]
    fn a_none_path_applies_in_memory_and_writes_nothing() {
        let mut app = headless_app();
        app.apply_remember_choice_to(RememberChoice::Always, None);
        assert_eq!(
            app.config.reporting.streams.crash_reports,
            ReportingMode::Always
        );
        assert!(app.toast.is_none(), "no write was attempted, so no toast");
    }

    // ---- dialog rendering (headless egui_kittest, real widgets) ----
    //
    // NOT covered here, for specific reasons rather than convenience:
    //
    //  * "Email instead" (`do_mailto`) and the SHORT-body "Open on GitHub" both
    //    call `itasha_report_core::intake::launch`, which really spawns the
    //    user's browser / mail client. Clicking them in a test would open real
    //    windows on the developer's desktop, so only the LONG-body "Open on
    //    GitHub" arm — which returns `CopyToClipboard` and launches nothing — is
    //    driven below.
    //  * "Send report" (`consent_and_send`) resolves its transport endpoint from
    //    the process-global env (`onion_from_env` / `endpoint_from_env`), which
    //    `reporting.rs`'s tests mutate behind their own private lock. Clicking
    //    Send here could observe a concurrently-set endpoint and attempt a real
    //    network send, so the hermetic DECLINE path is driven instead.

    use egui_kittest::kittest::Queryable;
    use egui_kittest::Harness;
    use std::cell::RefCell;

    /// A headless harness driving the REAL `render_report_issue` each frame.
    fn issue_harness(app: &RefCell<C0pl4ndApp>) -> Harness<'_> {
        #[allow(deprecated)]
        let mut h = Harness::new(move |ctx| app.borrow_mut().render_report_issue(ctx));
        h.set_size(egui::vec2(1000.0, 800.0));
        h.run();
        h
    }

    /// A headless harness driving the REAL `render_crash_consent` each frame.
    fn consent_harness(app: &RefCell<C0pl4ndApp>) -> Harness<'_> {
        #[allow(deprecated)]
        let mut h = Harness::new(move |ctx| app.borrow_mut().render_crash_consent(ctx));
        h.set_size(egui::vec2(1000.0, 800.0));
        h.run();
        h
    }

    /// An app with ONE crash report spooled in `dir` and loaded into the dialog.
    /// The spool is rooted at an explicit temp dir via `set_config_dir`, so no
    /// real user state is touched.
    fn app_with_pending_crash(dir: &Path) -> C0pl4ndApp {
        let spool = crate::reporting::open_spool_in(dir).expect("spool opens in temp dir");
        let report = crate::reporting::build_crash_report("boom", "src/x.rs:1");
        spool.enqueue(&report).expect("enqueue a crash report");

        let mut app = headless_app();
        app.crash_consent.set_config_dir(Some(dir.to_path_buf()));
        app.crash_consent.load_from_spool();
        assert!(
            app.crash_consent.has_pending(),
            "precondition: a spooled crash report must be pending"
        );
        app
    }

    /// The report-issue dialog is USER-INITIATED: with `open == false` it renders
    /// nothing at all, so the default experience is untouched.
    #[test]
    fn report_issue_renders_nothing_until_opened() {
        let app = RefCell::new(headless_app());
        assert!(!app.borrow().issue_intake.open, "precondition: closed");
        let h = issue_harness(&app);
        assert!(
            h.query_by_label("Open on GitHub").is_none(),
            "a closed dialog must render no widgets"
        );
    }

    /// Opening the dialog renders the real form: the action buttons and the
    /// live preview of EXACTLY what would be sent.
    #[test]
    fn report_issue_renders_the_form_and_a_live_preview_when_open() {
        let mut a = headless_app();
        a.issue_intake.open = true;
        a.issue_intake.description = "the terminal ate my homework".to_string();
        let app = RefCell::new(a);
        let h = issue_harness(&app);

        h.get_by_label("Open on GitHub");
        h.get_by_label("Close");
        // "Email instead" is deliberately ABSENT here: the shipped default
        // carries no mailbox (no personal address is compiled into a public
        // binary), and a `mailto:` with no recipient opens an empty compose
        // window that goes nowhere. See the configured case below.
        assert!(
            h.query_by_label("Email instead").is_none(),
            "the stock config has no alias, so the email route must not be offered"
        );
        // The preview must show the user's actual words — proof it previews the
        // real body rather than a placeholder.
        assert!(
            h.query_by_label_contains("the terminal ate my homework")
                .is_some(),
            "the preview must render the user's description verbatim"
        );
    }

    /// …and a CONFIGURED alias brings the email route back.
    ///
    /// This is the half that stops the assertion above passing vacuously: if the
    /// button had simply been deleted, or if `email_fallback_offered` were wired
    /// to a constant `false`, the absence check would still pass and the feature
    /// would be silently gone. Driven through the REAL dialog on a real config,
    /// so it asserts the live config value reaches the widget.
    #[test]
    fn report_issue_offers_the_email_route_when_an_alias_is_configured() {
        let mut a = headless_app();
        a.issue_intake.open = true;
        a.config.reporting.issue_intake.mailto_alias = "support@example.org".to_string();
        let app = RefCell::new(a);
        let h = issue_harness(&app);

        assert!(
            h.query_by_label("Email instead").is_some(),
            "a configured alias must offer the email route — otherwise the \
             absence assertion in the sibling test proves nothing"
        );
    }

    /// Clicking Close closes the dialog — the real widget → real state path.
    #[test]
    fn report_issue_close_button_closes_the_dialog() {
        let mut a = headless_app();
        a.issue_intake.open = true;
        let app = RefCell::new(a);
        let mut h = issue_harness(&app);

        h.get_by_label("Close").click();
        h.run();

        assert!(
            !app.borrow().issue_intake.open,
            "Close must close the dialog"
        );
    }

    /// A report too long for a GitHub deep link warns the user UP FRONT that
    /// "Open on GitHub" will fall back to the clipboard.
    #[test]
    fn report_issue_warns_when_the_report_is_too_long_for_a_deep_link() {
        let mut a = headless_app();
        a.issue_intake.open = true;
        a.issue_intake.description = "x".repeat(9000);
        assert!(
            !a.issue_intake
                .fits_url_length(&a.config.reporting.issue_intake.repo, "test-renderer"),
            "precondition: this body must exceed the deep-link ceiling"
        );
        let app = RefCell::new(a);
        let h = issue_harness(&app);

        assert!(
            h.query_by_label_contains("copy it to your clipboard")
                .is_some(),
            "an over-long report must warn about the clipboard fallback"
        );
    }

    /// A SHORT report renders NO clipboard warning — the control proving the
    /// warning above is driven by the length, not always on screen.
    #[test]
    fn report_issue_does_not_warn_when_the_report_fits_a_deep_link() {
        let mut a = headless_app();
        a.issue_intake.open = true;
        a.issue_intake.description = "short".to_string();
        let app = RefCell::new(a);
        let h = issue_harness(&app);

        assert!(
            h.query_by_label_contains("copy it to your clipboard")
                .is_none(),
            "a short report must not show the clipboard-fallback warning"
        );
    }

    /// "Open on GitHub" on an over-long report copies to the clipboard instead of
    /// launching a browser, and reports that outcome. This is the ONE `do_open`
    /// arm that launches nothing, so it is the only one drivable in a test.
    #[test]
    fn report_issue_open_on_github_falls_back_to_the_clipboard_when_too_long() {
        let mut a = headless_app();
        a.issue_intake.open = true;
        a.issue_intake.description = "x".repeat(9000);
        let app = RefCell::new(a);
        let mut h = issue_harness(&app);

        h.get_by_label("Open on GitHub").click();
        h.run();

        assert_eq!(
            app.borrow().issue_intake.last_outcome,
            Some(crate::issue_intake::IntakeOutcome::CopiedToClipboard),
            "an over-long report must copy to the clipboard, never launch a browser"
        );
    }

    /// Each recorded outcome renders its own status line, so the user always
    /// learns what actually happened.
    #[test]
    fn report_issue_renders_a_status_line_for_every_outcome() {
        let cases = [
            (
                crate::issue_intake::IntakeOutcome::OpenedDeepLink,
                "Opened the issue form",
            ),
            (
                crate::issue_intake::IntakeOutcome::CopiedToClipboard,
                "Copied the report to your clipboard",
            ),
            (
                crate::issue_intake::IntakeOutcome::OpenedMailto,
                "Opened your mail client",
            ),
            (
                crate::issue_intake::IntakeOutcome::Failed("nope".to_string()),
                "That didn't work",
            ),
        ];
        for (outcome, expected) in cases {
            let mut a = headless_app();
            a.issue_intake.open = true;
            a.issue_intake.last_outcome = Some(outcome.clone());
            let app = RefCell::new(a);
            let h = issue_harness(&app);
            assert!(
                h.query_by_label_contains(expected).is_some(),
                "outcome {outcome:?} must render the status line {expected:?}"
            );
        }
    }

    /// The crash-consent dialog is a no-op when nothing is spooled — the
    /// opted-out / nothing-crashed case, i.e. the default experience.
    #[test]
    fn crash_consent_renders_nothing_when_no_report_is_pending() {
        let app = RefCell::new(headless_app());
        assert!(!app.borrow().crash_consent.has_pending(), "precondition");
        let h = consent_harness(&app);
        assert!(
            h.query_by_label("Send report").is_none(),
            "no pending report must render no dialog"
        );
    }

    /// A pending crash report renders the consent dialog with EQUAL-WEIGHT Send /
    /// Don't-send and the remember-choice radios.
    #[test]
    fn crash_consent_renders_equal_weight_choices_when_a_report_is_pending() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let app = RefCell::new(app_with_pending_crash(tmp.path()));
        let h = consent_harness(&app);

        h.get_by_label("Send report");
        h.get_by_label("Don't send");
        h.get_by_label("Just this time");
        h.get_by_label("Always send");
        h.get_by_label("Never");
    }

    /// Don't-send discards the report WITHOUT transmitting: the dialog clears and
    /// the spooled file is gone. Hermetic — the spool is rooted at a temp dir.
    #[test]
    fn crash_consent_dont_send_discards_the_report() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let app = RefCell::new(app_with_pending_crash(tmp.path()));
        let mut h = consent_harness(&app);

        h.get_by_label("Don't send").click();
        h.run();

        assert!(
            !app.borrow().crash_consent.has_pending(),
            "Don't-send must clear the pending report"
        );
    }

    /// END-TO-END over the real widgets, and the UI-level statement of the bug this
    /// module was fixed for: picking "Always send" + Don't-send graduates the crash
    /// stream IN MEMORY, and — because the harness is headless — targets NO config
    /// file, so the developer's real `%APPDATA%\c0pl4nd\config.toml` is untouched.
    ///
    /// "Always send" (not "Never") is the choice driven here because `Off` is the
    /// built-in default: remembering Never would assert a value the config already
    /// had, and would pass even if the radio were wired to nothing.
    #[test]
    fn crash_consent_remembering_always_applies_in_memory_without_writing_config() {
        let tmp = tempfile::tempdir().expect("temp dir");
        let app = RefCell::new(app_with_pending_crash(tmp.path()));
        assert_eq!(
            app.borrow().config.reporting.streams.crash_reports,
            ReportingMode::Off,
            "precondition: the crash stream starts Off (the opt-in default)"
        );
        let mut h = consent_harness(&app);

        h.get_by_label("Always send").click();
        h.run();
        h.get_by_label("Don't send").click();
        h.run();

        let app = app.borrow();
        assert_eq!(
            app.config.reporting.streams.crash_reports,
            ReportingMode::Always,
            "remembering Always must graduate the crash stream in memory"
        );
        assert_eq!(
            app.remember_save_path(),
            None,
            "a headless run must not target the user's real config file"
        );
    }
}
