use super::{
    LinuxDisplayServer, Settings, ShortcutAction, ShortcutBinding, ShortcutKey, ShortcutStatus,
    ShortcutTool, egui,
};

impl ShortcutTool {
    pub(crate) fn show(&mut self, ui: &mut egui::Ui, display: Option<LinuxDisplayServer>) {
        ui.heading("Global recorder shortcuts");
        ui.weak("Optional recorder controls, not keystroke recording. No shortcut is registered on the launcher or outside recorder scope.");
        ui.weak("Open/prepare the recorder first; press again to start once the frame/controller is ready.");
        ui.horizontal_wrapped(|ui| {
            if !self.settings.enabled {
                if action_button(ui, "enable", "Enable shortcuts", true) {
                    self.notice = self.set_enabled(true).err();
                }
            } else if action_button(ui, "disable", "Disable shortcuts", true) {
                self.notice = self.set_enabled(false).err();
            }
            if matches!(self.status, ShortcutStatus::Registering)
                && action_button(ui, "cancel", "Cancel registration", true)
            {
                self.notice = self.set_enabled(false).err();
            }
            if self.settings.enabled
                && (matches!(self.status, ShortcutStatus::Failed(_) | ShortcutStatus::Stopped)
                    || matches!(&self.status, ShortcutStatus::Active(bindings) if bindings.len() < self.settings.bindings.len()))
                && action_button(ui, "retry", "Retry registration", true)
            {
                self.retry();
            }
            let changed = self.draft_bindings != self.settings.bindings;
            if action_button(ui, "apply", "Apply bindings", changed) {
                self.notice = self.apply_bindings().err();
            }
        });
        self.show_status(ui, display);
        let before = self.draft_bindings.clone();
        egui::ScrollArea::vertical()
            .id_salt("recorder-shortcut-bindings")
            .max_height(240.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for binding in &mut self.draft_bindings {
                    ui.push_id(binding.action.id(), |ui| binding_controls(ui, binding));
                }
            });
        if self.draft_bindings != before {
            self.io.edited(false);
        }
        let draft = Settings {
            enabled: self.settings.enabled,
            bindings: self.draft_bindings.clone(),
        };
        if let Err(error) = draft.validate() {
            ui.colored_label(ui.visuals().warn_fg_color, error);
        } else if self.draft_bindings != self.settings.bindings {
            ui.weak("Edits are not active yet. Apply waits for the previous registration to close before binding the new keys.");
        }
        if let Some(notice) = &self.notice {
            ui.colored_label(ui.visuals().warn_fg_color, notice);
        }
        if let Some(notice) = self.io.notice() {
            ui.colored_label(ui.visuals().warn_fg_color, notice);
        }
        if self.io.needs_reload() && ui.button("Retry loading shortcut settings").clicked() {
            self.io.retry_load();
        }
        ui.weak("If registration is unavailable, denied, conflicting or incomplete, use the recorder's visible buttons and timed stop. Discard has no global shortcut.");
    }

    fn show_status(&self, ui: &mut egui::Ui, display: Option<LinuxDisplayServer>) {
        if !self.settings.enabled {
            ui.label("Shortcuts are disabled.");
        } else if !self.scope {
            ui.label("Enabled preference; registration starts only in recorder scope.");
        } else if display.is_none() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "No supported Linux display was detected; recorder buttons remain available.",
            );
        }
        match &self.status {
            ShortcutStatus::Registering => {
                ui.label("Registering… Complete or cancel the system permission dialog.");
            }
            ShortcutStatus::Stopping => {
                ui.label("Stopping the previous registration… Its actions are already ignored. Use recorder buttons meanwhile.");
            }
            ShortcutStatus::Stopped => {
                ui.weak("No shortcut registration is active.");
            }
            ShortcutStatus::Failed(error) => {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    format!("Registration failed: {error}"),
                );
            }
            ShortcutStatus::Active(bindings) => {
                ui.label("Actually registered by the desktop:");
                for binding in bindings {
                    ui.label(format!(
                        "{} — {}",
                        action_label(binding.action),
                        binding.trigger_description
                    ));
                }
                for requested in &self.settings.bindings {
                    if !bindings
                        .iter()
                        .any(|binding| binding.action == requested.action)
                    {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            format!(
                                "{} was not registered. Use its recorder button.",
                                action_label(requested.action)
                            ),
                        );
                    }
                }
            }
        }
    }
}

fn action_label(action: ShortcutAction) -> &'static str {
    match action {
        ShortcutAction::StartPause => "Start / pause / resume",
        ShortcutAction::Stop => "Stop and save",
        ShortcutAction::Snapshot => "Manual snapshot",
    }
}

fn action_button(ui: &mut egui::Ui, id: &str, label: &str, enabled: bool) -> bool {
    ui.push_id(id, |ui| ui.add_enabled(enabled, egui::Button::new(label)))
        .inner
        .clicked()
}

fn key_label(key: ShortcutKey) -> String {
    match key {
        ShortcutKey::Function(number) => format!("F{number}"),
        ShortcutKey::Character(key) => key.to_string(),
    }
}

fn binding_controls(ui: &mut egui::Ui, binding: &mut ShortcutBinding) {
    ui.label(action_label(binding.action));
    ui.horizontal_wrapped(|ui| {
        egui::ComboBox::from_id_salt("key")
            .width(65.0)
            .height(180.0)
            .selected_text(key_label(binding.trigger.key))
            .show_ui(ui, |ui| {
                for number in 1..=24 {
                    let key = ShortcutKey::Function(number);
                    ui.selectable_value(&mut binding.trigger.key, key, key_label(key));
                }
                for key in ('A'..='Z').chain('0'..='9') {
                    ui.selectable_value(
                        &mut binding.trigger.key,
                        ShortcutKey::Character(key),
                        key.to_string(),
                    );
                }
            });
        ui.checkbox(&mut binding.trigger.control, "Ctrl");
        ui.checkbox(&mut binding.trigger.shift, "Shift");
        ui.checkbox(&mut binding.trigger.alt, "Alt");
        ui.checkbox(&mut binding.trigger.super_key, "Super");
    });
}
