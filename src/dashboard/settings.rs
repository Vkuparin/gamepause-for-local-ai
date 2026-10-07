//! The Advanced settings pages and their draft: edits stay in `edit_config`
//! until saved, and follow the saved settings where the draft did not touch them.

use super::{Dashboard, Modal, SettingsPage, native::browse};
use crate::{
    app::{Action, Shared},
    config::Config,
    dashboard_theme::{self as design, CheckboxUi, Icon, Palette},
    tray,
};
use eframe::egui::*;
/// Carries unsaved Advanced edits over a change to the saved settings: a value
/// the draft did not touch follows the saved settings, and one it did touch
/// stays as typed. `None` when the result is not a valid configuration.
pub(super) fn rebase_draft(base: &Config, draft: &Config, saved: &Config) -> Option<Config> {
    use serde_json::Value;
    fn merge(base: &Value, draft: &Value, saved: &Value) -> Value {
        if draft == base {
            return saved.clone();
        }
        if saved == base {
            return draft.clone();
        }
        match (base, draft, saved) {
            (Value::Object(base), Value::Object(draft), Value::Object(saved)) => Value::Object(
                draft
                    .iter()
                    .map(|(key, value)| {
                        let other = |map: &serde_json::Map<String, Value>| {
                            map.get(key).cloned().unwrap_or(Value::Null)
                        };
                        (key.clone(), merge(&other(base), value, &other(saved)))
                    })
                    .collect(),
            ),
            (Value::Array(base), Value::Array(draft), Value::Array(saved))
                if base.len() == draft.len() && draft.len() == saved.len() =>
            {
                Value::Array(
                    base.iter()
                        .zip(draft)
                        .zip(saved)
                        .map(|((base, draft), saved)| merge(base, draft, saved))
                        .collect(),
                )
            }
            // Both changed the same value: the edit on screen wins.
            _ => draft.clone(),
        }
    }
    let value = |config: &Config| serde_json::to_value(config).ok();
    let merged: Config =
        serde_json::from_value(merge(&value(base)?, &value(draft)?, &value(saved)?)).ok()?;
    merged.validate().ok()?;
    Some(merged)
}
fn numeric(ui: &mut Ui, label: &str, value: &mut f64, dirty: &mut bool) {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        *dirty |= ui.add(DragValue::new(value).speed(0.5)).changed();
    });
}
fn string_list(ui: &mut Ui, label: &str, list: &mut Vec<String>, dirty: &mut bool) {
    ui.collapsing(label, |ui| {
        let mut remove = None;
        for (index, value) in list.iter_mut().enumerate() {
            ui.push_id((label, index), |ui| {
                ui.horizontal(|ui| {
                    *dirty |= ui.text_edit_singleline(value).changed();
                    if ui.small_button("Remove").clicked() {
                        remove = Some(index);
                    }
                });
            });
        }
        if let Some(index) = remove {
            list.remove(index);
            *dirty = true;
        }
        if ui.button("Add entry").clicked() {
            list.push(String::new());
            *dirty = true;
        }
    });
}
impl Dashboard {
    pub(super) fn advanced(&mut self, ui: &mut Ui, s: &Shared, p: Palette) {
        ui.heading("Advanced settings");
        ui.colored_label(
            p.muted,
            "Settings are saved atomically. Recovery keeps its original provider routes.",
        );
        ui.add_space(design::GAP);
        p.card().show(ui, |ui| {
            ui.set_min_width(ui.available_width());
            ui.horizontal_wrapped(|ui| {
                for (page, label) in [
                    (SettingsPage::General, "General"),
                    (SettingsPage::Detection, "Detection"),
                    (SettingsPage::LMStudio, "LM Studio"),
                    (SettingsPage::Ollama, "Ollama"),
                    (SettingsPage::Apps, "Other apps"),
                    (SettingsPage::Recovery, "Recovery"),
                    (SettingsPage::Diagnostics, "Diagnostics"),
                ] {
                    if ui
                        .selectable_label(self.settings_page == page, label)
                        .clicked()
                    {
                        self.settings_page = page;
                        self.validation.clear();
                    }
                }
            });
        });
        ui.add_space(design::GAP);
        p.card().show(ui,|ui| {
            ui.set_min_width(ui.available_width());
            ui.add_enabled_ui(!s.commands.settings_pending,|ui| {
                match self.settings_page {
                    SettingsPage::General=> {
                        ui.heading("General");
                        let mut startup=tray::startup_enabled();
                        if ui.styled_checkbox(&mut startup,"Start when I sign in to Windows").changed(){tray::request_startup(&self.shared,startup,&self.folder);}
                        let mut visual=s.config.notifications_enabled;let mut sound=s.config.sound_enabled;
                        let changed=ui.styled_checkbox(&mut visual,"Windows notifications").changed() | ui.styled_checkbox(&mut sound,"Sound for notifications").changed();
                        if changed {self.action(Action::NotificationPreferences{visual,sound},"Notification preferences");}
                        ui.add_space(8.0);ui.label("Appearance");
                        let mut appearance=s.config.appearance;
                        ComboBox::from_id_salt("appearance").selected_text(match appearance {crate::config::Appearance::System=>"Follow Windows",crate::config::Appearance::Light=>"Light",crate::config::Appearance::Dark=>"Dark"}).show_ui(ui,|ui| {
                            for (value,label) in [(crate::config::Appearance::System,"Follow Windows"),(crate::config::Appearance::Light,"Light"),(crate::config::Appearance::Dark,"Dark")] {ui.selectable_value(&mut appearance,value,label);}
                        });
                        if appearance!=s.config.appearance {self.action(Action::Appearance(appearance),"Appearance");}
                        ui.colored_label(p.muted,"High contrast follows Windows colors. Closing the window keeps GamePause in the tray.");
                        ui.add_space(8.0);ui.label("Pause AI / Resume AI shortcut");
                        self.dirty |= ui.add(TextEdit::singleline(&mut self.edit_config.pause_hotkey).hint_text("None, for example Ctrl+Alt+P")).changed();
                        ui.colored_label(p.muted,"Works everywhere, including in games. Needs Ctrl, Alt or Win plus one key. Leave empty for no shortcut.");
                        self.save_bar(ui,s,p);
                        if ui.button("Keyboard help").clicked(){self.modal=Some(Modal::Help);}
                    },
                    SettingsPage::Detection=> {
                        ui.heading("Game detection");
                        ui.colored_label(p.muted,"Launcher metadata and registered paths identify games. Running apps helps you add missed executables.");
                        numeric(ui,"Process poll interval (seconds)",&mut self.edit_config.poll_seconds,&mut self.dirty);
                        self.dirty |= ui.styled_checkbox(&mut self.edit_config.suggest_unknown_games,"Point out fullscreen programs that look like games").changed();
                        ui.colored_label(p.muted,"A suggestion only: AI is never paused for a program until you add it.");
                        numeric(ui,"Discovery interval (seconds)",&mut self.edit_config.discovery_seconds,&mut self.dirty);
                        string_list(ui,"Additional game folders",&mut self.edit_config.game_roots,&mut self.dirty);
                        string_list(ui,"Steam roots",&mut self.edit_config.steam_roots,&mut self.dirty);
                        string_list(ui,"Epic manifest folders",&mut self.edit_config.epic_manifest_dirs,&mut self.dirty);
                        string_list(ui,"Excluded executable names",&mut self.edit_config.excluded_executables,&mut self.dirty);
                        if ui.button("Refresh game list").on_hover_text("Look for newly installed games now.").clicked(){self.action(Action::Refresh,"Discovery refresh");}
                        self.save_bar(ui,s,p);
                    },
                    SettingsPage::LMStudio=> {
                        ui.heading("LM Studio");
                        let editable=!s.provider_pending(crate::provider::Kind::LMStudio);
                        if !editable {ui.colored_label(p.accent,"Restore pending LM Studio recovery before editing its connection.");}
                        ui.add_enabled_ui(editable,|ui| {
                            if let Some(crate::config::Provider::LMStudio{enabled,connection,..})=self.edit_config.providers.iter_mut().find(|p|p.kind()==crate::provider::Kind::LMStudio) {
                                self.dirty |= ui.styled_checkbox(enabled,"Enable LM Studio control").changed();
                                ui.label("Loopback API address");self.dirty |= ui.text_edit_singleline(&mut connection.endpoint).changed();
                                ui.label("lms executable");
                                ui.horizontal(|ui| {self.dirty|=ui.text_edit_singleline(&mut connection.lms_path).changed();if ui.button("Browse...").clicked(){match browse(self.owner){Ok(Some(path))=>{connection.lms_path=path;self.dirty=true;},Ok(None)=>(),Err(e)=>self.validation=format!("File picker: {e:#}")}}});
                                self.dirty |= ui.styled_checkbox(&mut connection.stop_server_during_gaming,"Stop the captured server during gaming").changed();
                            } else {ui.label("No LM Studio entry is configured.");}
                        });
                        ui.colored_label(p.muted,"Restores captured load settings and verifies the original server state. No automatic downloads.");
                        self.save_bar(ui,s,p);
                    },
                    SettingsPage::Ollama=> {
                        ui.heading("Ollama");
                        ui.label("Works when Ollama is running; nothing happens when it is not. Local models are unloaded for gaming. GGUF completion models come back with their context and the keep-alive time they had left; other local models, such as embedding models, stay unloaded. Full load options, parallelism, conversations and KV cache are not preserved.");
                        ui.label("Ollama itself keeps running. GamePause does not download models or fight later client reloads. Tested with Ollama 0.35.1 and 0.40.0.");
                        let editable=!s.provider_pending(crate::provider::Kind::Ollama);
                        ui.add_enabled_ui(editable,|ui| {
                            if let Some(crate::config::Provider::Ollama{enabled,endpoint,..})=self.edit_config.providers.iter_mut().find(|p|p.kind()==crate::provider::Kind::Ollama) {
                                self.dirty |= ui.styled_checkbox(enabled,"Pause Ollama models while gaming").changed();
                                ui.label("Loopback endpoint");self.dirty |= ui.text_edit_singleline(endpoint).changed();
                            } else {ui.label("No Ollama entry is configured.");}
                        });
                        if !editable {ui.colored_label(p.accent,"Finish pending Ollama recovery before turning it off or changing its endpoint.");}
                        self.save_bar(ui,s,p);
                        ui.hyperlink_to("Report Ollama problems or contribute fixes",concat!(env!("CARGO_PKG_REPOSITORY"),"/blob/main/CONTRIBUTING.md"));
                    },
                    SettingsPage::Apps=> {
                        ui.heading("Other AI apps");
                        ui.colored_label(p.accent,"Not yet tested with real AI tools such as llama.cpp or KoboldCpp.");
                        ui.label("Choose the program file of a local AI server, for example llama-server.exe or koboldcpp.exe. GamePause stops that exact file when a game starts and starts it again afterwards with the same command line and folder.");
                        ui.label("Work in progress is interrupted. Environment variables set by a launcher are not kept. The saved start command is encrypted for your Windows account and never shown.");
                        let editable=!s.provider_pending(crate::provider::Kind::Process);
                        if !editable {ui.colored_label(p.accent,"Finish the pending restart before changing these apps.");}
                        ui.add_enabled_ui(editable,|ui| {
                            if let Some(crate::config::Provider::Process{enabled,apps,..})=self.edit_config.providers.iter_mut().find(|p|p.kind()==crate::provider::Kind::Process) {
                                self.dirty |= ui.styled_checkbox(enabled,"Stop these apps while gaming").changed();
                                let mut remove=None;
                                for (index,app) in apps.iter_mut().enumerate() {
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(RichText::new(&app.name).strong());
                                        ui.colored_label(p.muted,&app.path);
                                        self.dirty |= ui.styled_checkbox(&mut app.relaunch,"Start again after gaming").changed();
                                        if ui.button("Remove").clicked(){remove=Some(index);}
                                    });
                                }
                                if let Some(index)=remove {apps.remove(index);self.dirty=true;}
                                if apps.is_empty(){ui.colored_label(p.muted,"No app chosen. Nothing is stopped.");}
                                if ui.button("Add app...").clicked(){
                                    match browse(self.owner){
                                        Ok(Some(path))=>{
                                            let name=crate::process_session::file_name(&path).trim_end_matches(".exe").trim_end_matches(".EXE").to_owned();
                                            apps.push(crate::config::ProcessApp{name,path,relaunch:true});
                                            self.dirty=true;
                                        },
                                        Ok(None)=>(),
                                        Err(e)=>self.validation=format!("File picker: {e:#}"),
                                    }
                                }
                            } else {ui.label("No entry for other apps is configured.");}
                        });
                        self.save_bar(ui,s,p);
                    },
                    SettingsPage::Recovery=> {
                        ui.heading("Pause and recovery");
                        numeric(ui,"Restore after games exit (seconds)",&mut self.edit_config.restore_delay_seconds,&mut self.dirty);
                        numeric(ui,"Retry interval (seconds)",&mut self.edit_config.retry_seconds,&mut self.dirty);
                        ui.label("Original model settings are saved before unloading. Partial failures retain recovery until verification succeeds. Ignoring a game does not erase pending recovery checks.");
                        ui.colored_label(if s.pending {p.accent}else{p.muted},if s.pending {"Recovery is pending"}else{"No saved recovery is pending"});
                        self.save_bar(ui,s,p);
                        ui.add_space(design::GAP);
                        if ui.add_enabled(crate::ui_commands::verify_available(s),Button::new("Test round-trip...")).clicked(){self.modal=Some(Modal::Verify);}
                        ui.colored_label(p.muted,"The test captures, unloads and reloads the models LM Studio and Ollama have loaded. It requires no running games or pending recovery.");
                    },
                    SettingsPage::Diagnostics=> {
                        ui.heading("Read-only diagnostics");
                        if ui.add_enabled(!s.doctor_pending,Button::new(if s.doctor_pending {"Checking..."}else{"Check enabled providers"})).clicked(){self.action(Action::Doctor,"Read-only diagnostics");}
                        ui.colored_label(p.muted,"Uses saved connections. Does not start services or load/unload models.");
                        let report=crate::diagnostics::render(s);
                        ui.add(Label::new(report).wrap());
                        if design::button(ui,"Open logs and status folder",Icon::Folder,false,p).clicked(){tray::request_folder(&self.shared,&self.folder);}
                    },
                }
            });
            if !self.validation.is_empty(){ui.colored_label(p.error,&self.validation);}
            if !s.settings_error.is_empty(){ui.colored_label(p.error,&s.settings_error);}
        });
    }
    pub(super) fn discard_draft(&mut self, s: &Shared) {
        self.edit_config = s.config.clone();
        self.edit_base = s.config.clone();
        self.edit_revision = s.revision;
        self.dirty = false;
        self.validation.clear();
    }
    pub(super) fn save_bar(&mut self, ui: &mut Ui, s: &Shared, p: Palette) {
        ui.add_space(design::GAP);
        ui.horizontal(|ui| {
            if ui
                .add_enabled(
                    self.dirty && !s.commands.settings_pending,
                    Button::new("Save settings").fill(p.selected),
                )
                .clicked()
            {
                self.save(self.edit_config.clone(), true);
                if self.validation.is_empty() {
                    self.dirty = false;
                }
            }
            if ui
                .add_enabled(self.dirty, Button::new("Discard edits"))
                .clicked()
            {
                self.discard_draft(s);
            }
            if self.dirty {
                ui.colored_label(p.muted, "Unsaved changes");
            }
        });
    }
}
