use super::{
    LinuxDisplayServer, Settings, ShortcutAction, ShortcutBinding, ShortcutKey, ShortcutStatus,
    ShortcutTool, egui,
};
use gif_from_screen_localization::{Localizer, Message};

impl ShortcutTool {
    pub(crate) fn show(
        &mut self,
        ui: &mut egui::Ui,
        display: Option<LinuxDisplayServer>,
        localizer: Localizer,
    ) {
        ui.heading(localizer.text(Message::RecorderGlobalShortcuts));
        ui.weak(localizer.text(Message::ShortcutsHelp));
        ui.weak(localizer.text(Message::ShortcutsPrepareHint));
        ui.horizontal_wrapped(|ui| {
            if !self.settings.enabled {
                if action_button(ui, "enable", localizer.text(Message::ShortcutsEnable), true) {
                    self.notice = self.set_enabled(true).err();
                }
            } else if action_button(ui, "disable", localizer.text(Message::ShortcutsDisable), true) {
                self.notice = self.set_enabled(false).err();
            }
            if matches!(self.status, ShortcutStatus::Registering)
                && action_button(ui, "cancel", localizer.text(Message::ShortcutsCancelRegistration), true)
            {
                self.notice = self.set_enabled(false).err();
            }
            if self.settings.enabled
                && (matches!(self.status, ShortcutStatus::Failed(_) | ShortcutStatus::Stopped)
                    || matches!(&self.status, ShortcutStatus::Active(bindings) if bindings.len() < self.settings.bindings.len()))
                && action_button(ui, "retry", localizer.text(Message::ShortcutsRetryRegistration), true)
            {
                self.retry();
            }
            let changed = self.draft_bindings != self.settings.bindings;
            if action_button(ui, "apply", localizer.text(Message::ShortcutsApplyBindings), changed) {
                self.notice = self.apply_bindings().err();
            }
        });
        self.show_status(ui, display, localizer);
        let before = self.draft_bindings.clone();
        egui::ScrollArea::vertical()
            .id_salt("recorder-shortcut-bindings")
            .max_height(240.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for binding in &mut self.draft_bindings {
                    ui.push_id(binding.action.id(), |ui| {
                        binding_controls(ui, binding, localizer);
                    });
                }
            });
        if self.draft_bindings != before {
            self.io.edited(false);
        }
        let draft = Settings {
            enabled: self.settings.enabled,
            bindings: self.draft_bindings.clone(),
        };
        if let Err(error) = draft.validate_for_ui() {
            ui.colored_label(ui.visuals().warn_fg_color, error.render(localizer));
        } else if self.draft_bindings != self.settings.bindings {
            ui.weak(localizer.text(Message::ShortcutsEditsPending));
        }
        if let Some(notice) = &self.notice {
            ui.colored_label(ui.visuals().warn_fg_color, notice.render(localizer));
        }
        if let Some(notice) = self.io.notice() {
            ui.colored_label(ui.visuals().warn_fg_color, notice.render(localizer));
        }
        if self.io.needs_reload()
            && action_button(
                ui,
                "reload-settings",
                localizer.text(Message::ShortcutsRetryLoading),
                true,
            )
        {
            self.io.retry_load();
        }
        ui.weak(localizer.text(Message::ShortcutsButtonsFallback));
    }

    fn show_status(
        &self,
        ui: &mut egui::Ui,
        display: Option<LinuxDisplayServer>,
        localizer: Localizer,
    ) {
        if !self.settings.enabled {
            ui.label(localizer.text(Message::ShortcutsDisabled));
        } else if !self.scope {
            ui.label(localizer.text(Message::ShortcutsScopePending));
        } else if display.is_none() {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                localizer.text(Message::ShortcutsNoDisplay),
            );
        }
        match &self.status {
            ShortcutStatus::Registering => {
                ui.label(localizer.text(Message::ShortcutsRegistering));
            }
            ShortcutStatus::Stopping => {
                ui.label(localizer.text(Message::ShortcutsStopping));
            }
            ShortcutStatus::Stopped => {
                ui.weak(localizer.text(Message::ShortcutsInactive));
            }
            ShortcutStatus::Failed(error) => {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    crate::format_message(
                        localizer,
                        Message::ShortcutsRegistrationFailed,
                        &[("error", error)],
                    ),
                );
            }
            ShortcutStatus::Active(bindings) => {
                ui.label(localizer.text(Message::ShortcutsActuallyRegistered));
                for binding in bindings {
                    ui.label(crate::format_message(
                        localizer,
                        Message::ShortcutsRegisteredBinding,
                        &[
                            ("action", action_label(binding.action, localizer)),
                            ("trigger", &binding.trigger_description),
                        ],
                    ));
                }
                for requested in &self.settings.bindings {
                    if !bindings
                        .iter()
                        .any(|binding| binding.action == requested.action)
                    {
                        ui.colored_label(
                            ui.visuals().warn_fg_color,
                            crate::format_message(
                                localizer,
                                Message::ShortcutsMissingBinding,
                                &[("action", action_label(requested.action, localizer))],
                            ),
                        );
                    }
                }
            }
        }
    }
}

fn action_label(action: ShortcutAction, localizer: Localizer) -> &'static str {
    localizer.text(match action {
        ShortcutAction::StartPause => Message::ShortcutsActionStartPause,
        ShortcutAction::Stop => Message::RecorderStop,
        ShortcutAction::Snapshot => Message::ShortcutsActionSnapshot,
    })
}

pub(super) fn action_button(ui: &mut egui::Ui, id: &str, label: &str, enabled: bool) -> bool {
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

fn binding_controls(ui: &mut egui::Ui, binding: &mut ShortcutBinding, localizer: Localizer) {
    ui.label(action_label(binding.action, localizer));
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
