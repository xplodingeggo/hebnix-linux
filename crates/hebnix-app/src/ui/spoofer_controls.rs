//! HTTP feature requirements and certificate controls shared by Customisation and System.
use super::*;

#[derive(Clone, Copy, Default)]
pub(super) struct HttpSpooferFeatures {
    username: bool,
    friends: bool,
}

impl HttpSpooferFeatures {
    pub(super) fn any(self) -> bool {
        self.username || self.friends
    }

    fn can_run(self, certificate: bool) -> bool {
        self.any() && certificate
    }
}

impl HebnixApp {
    pub(super) fn http_spoofer_features(&self) -> HttpSpooferFeatures {
        HttpSpooferFeatures {
            username: self.spoofer_username_enabled,
            friends: self.spoofer_friends_enabled,
        }
    }

    pub(super) fn http_spoofer_ready(&self) -> bool {
        self.http_spoofer_features()
            .can_run(self.spoofer_cert_installed)
    }

    pub(super) fn prompt_for_missing_spoofer_certificate(&mut self) {
        self.spoofer_cert_installed = spoofer::ca::is_current_installed(&self.base_dir);
        let requested = self.http_spoofer_features();
        // linux-port: no administrator relaunch. Installing the certificate
        // asks for authentication through pkexec when the user clicks it.
        if requested.any() && !self.spoofer_cert_installed {
            self.disable_certificate_features();
            self.spoofer_certificate_prompt_open = true;
            self.console.write("[Spoofer] Install the certificate in Settings > System before enabling HTTP features again.");
        } else if self.spoofer_cert_installed {
            self.spoofer_mgr.certificate_setup_ready();
            self.spoofer_certificate_prompt_open = false;
        }
    }

    pub(super) fn install_spoofer_certificate(&mut self) {
        match spoofer::run_privileged(spoofer::PrivilegedAction::InstallCa, &self.base_dir) {
            Ok(()) => {
                self.spoofer_cert_installed = spoofer::ca::is_current_installed(&self.base_dir);
                if self.spoofer_cert_installed {
                    self.spoofer_mgr.certificate_setup_ready();
                    self.spoofer_certificate_prompt_open = false;
                    self.console.write(
                        "[Spoofer] Certificate installed. HTTP features can now be enabled.",
                    );
                    self.evaluate_proxies();
                } else {
                    self.spoofer_certificate_prompt_open = true;
                    self.console.write("[Spoofer] Certificate installation did not establish trust. Try installing it again.");
                }
            }
            Err(error) => {
                self.spoofer_certificate_prompt_open = true;
                self.console
                    .write(format!("[Spoofer] Certificate install failed: {error}"));
            }
        }
    }

    pub(super) fn render_spoofer_certificate_settings(&mut self, ui: &mut egui::Ui) {
        ui.heading("HTTP proxy certificate");
        ui.weak("Name and Friends point Rocket League's Wine prefix at the HTTP proxy before the game starts. This certificate is required; installing or removing it asks for authentication.");
        ui.horizontal_wrapped(|ui| {
            ui.label(t("spoofer-certificate-status"));
            ui.label(if self.spoofer_cert_installed {
                t("spoofer-installed")
            } else {
                t("spoofer-missing")
            });
            if self.spoofer_cert_installed {
                if ui.button(t("spoofer-remove")).clicked() {
                    self.disable_certificate_features();
                    match spoofer::run_privileged(
                        spoofer::PrivilegedAction::UninstallCa,
                        &self.base_dir,
                    ) {
                        Ok(()) => self.console.write("[Spoofer] Certificate removed."),
                        Err(error) => self
                            .console
                            .write(format!("[Spoofer] Certificate removal failed: {error}")),
                    }
                    self.spoofer_cert_installed =
                        spoofer::ca::is_current_installed(&self.base_dir);
                }
            } else if ui.button(t("spoofer-install-certificate")).clicked() {
                self.install_spoofer_certificate();
            }
            if ui
                .button("↻")
                .on_hover_text(t("spoofer-refresh-status"))
                .clicked()
            {
                self.prompt_for_missing_spoofer_certificate();
                self.evaluate_proxies();
            }
        });
        ui.horizontal_wrapped(|ui| {
            ui.label(t("spoofer-http-proxy"));
            ui.label(if self.spoofer_mgr.http_running() {
                t("spoofer-running")
            } else if self.proxy_options().account && !self.spoofer_mgr.game_running() {
                t("spoofer-waiting-for-game")
            } else {
                t("spoofer-stopped")
            });
        });
    }
}
