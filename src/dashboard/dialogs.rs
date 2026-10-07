//! The dashboard's modal dialogs: add, rename and remove a game, the gameplay
//! restore confirmation, the round-trip test, help and discarding a draft.

use super::{
    Dashboard, Modal, apply_rename,
    game_rules::{add_game, set_ignored},
    native::browse,
};
use crate::{
    app::{Action, Shared},
    commands::Outcome,
    dashboard_theme::{self as design, CheckboxUi, Palette},
    discovery::canonical,
};
use eframe::egui::{self, *};
use std::collections::HashSet;
pub(super) fn confirmed_resume(
    s: &Shared,
    offer: &crate::gameplay::RestoreOffer,
    checked: &[bool],
) -> Result<Action, String> {
    if !s.controls().availability().restore
        || s.commands.settings_pending
        || checked.len() != offer.games.len()
        || s.restore_offer
            .as_ref()
            .is_none_or(|current| current.id != offer.id)
    {
        return Err(
            "Running games or control availability changed. Cancel and request Resume again."
                .into(),
        );
    }
    let mut seen = HashSet::new();
    let ignored = offer
        .games
        .iter()
        .zip(checked)
        .filter(|(_, on)| **on)
        .map(|(game, _)| game.executable.clone())
        .filter(|path| seen.insert(canonical(path)))
        .collect();
    Ok(Action::ConfirmedRestore {
        offer_id: offer.id,
        ignored,
    })
}
pub(super) fn confirmed_lm_replacement(
    s: &Shared,
    offer: &crate::lmstudio::LoadoutOffer,
) -> Result<Action, String> {
    if s.lm_loadout_offer.as_ref() != Some(offer)
        || !s.controls().availability().restore
        || !s.active_games.is_empty()
        || s.commands.settings_pending
    {
        return Err(
            "Loadout or control availability changed; review the current prompt again.".into(),
        );
    }
    Ok(Action::ReplaceLMLoadout(offer.clone()))
}
impl Dashboard {
    pub(super) fn modal(&mut self, ctx: &Context, s: &Shared, p: Palette) {
        let Some(mut modal) = self.modal.take() else {
            self.modal_active = false;
            return;
        };
        let first = !self.modal_active;
        self.modal_active = true;
        let mut cancel = false;
        let mut accepted = false;
        let title = match &modal {
            Modal::Add { .. } => "Add game",
            Modal::Rename(..) => "Rename game",
            Modal::Remove(..) => "Remove custom game?",
            Modal::Resume(..) => "Resume AI while a game is running?",
            Modal::Verify => "Test live pause and restore?",
            Modal::ReplaceLM(_) => "Replace loaded LM Studio models?",
            Modal::Help => "Keyboard and controls",
            Modal::Discard => "Discard unsaved settings?",
        };
        let response=egui::Modal::new(Id::new("gamepause-modal")).frame(p.card().inner_margin(20)).show(ctx,|ui| {
            ui.set_width(510.0_f32.min(ctx.content_rect().width()-64.0));
            ui.heading(title);ui.add_space(design::GAP);
            match &mut modal {
                Modal::Add{name,path,auto}=> {
                    ui.label("Choose the game executable. Its name is filled from the filename.");
                    ui.label("Game name");ui.text_edit_singleline(name);
                    ui.label("Executable");ui.horizontal(|ui| {
                        ui.add_sized([ui.available_width()-112.0,design::CONTROL],TextEdit::singleline(path));
                        if ui.button("Browse...").clicked(){match browse(self.owner){Ok(Some(value))=>{*path=value;if name.trim().is_empty(){*name=std::path::Path::new(path).file_stem().unwrap_or_default().to_string_lossy().into_owned();}},Ok(None)=>(),Err(e)=>self.validation=format!("Could not choose executable: {e:#}")}}
                    });
                    ui.label("Source: Custom registration");ui.styled_checkbox(auto,"Automatically pause AI for this game");
                },
                Modal::Rename(row,name)=> {ui.label(&row.path);ui.label("Game name");ui.text_edit_singleline(name);},
                Modal::Remove(row)=> {ui.label(&row.name);ui.add(Label::new(&row.path).wrap());ui.label("Removes only this custom registration. Pending recovery keeps its original game checks.");},
                Modal::Resume(offer,checked)=> {
                    ui.colored_label(p.accent,"Resuming AI may consume GPU memory and affect game performance.");
                    ui.label("This approval ends when a listed game exits/restarts, another nonignored game starts, you pause AI, or GamePause restarts.");
                    ui.add_space(8.0);
                    ScrollArea::vertical().max_height(220.0).show(ui,|ui| {
                        for (game,ignore) in offer.games.iter().zip(checked.iter_mut()) {
                            ui.strong(format!("{} (PID {})",game.game,game.pid));
                            ui.add(Label::new(RichText::new(&game.executable).small().color(p.muted)).wrap());
                            ui.styled_checkbox(ignore,"Also ignore this executable for future pauses");ui.separator();
                        }
                    });
                    ui.colored_label(p.muted,"Ignore selections are optional. Resume works without selecting any games.");
                },
                Modal::ReplaceLM(offer)=> {
                    ui.label("Loaded LM Studio model doesn't match saved configuration; do you want to unload the current model(s) and load the saved model(s)?");
                    ui.colored_label(p.error,"This interrupts current generation and replaces the loaded models.");
                    ScrollArea::vertical().max_height(220.0).show(ui,|ui| {
                        ui.strong("Currently loaded");
                        for model in &offer.loaded { ui.label(format!("{} ({})",model.model_key,model.identifier)); }
                        ui.strong("Saved models");
                        for model in &offer.saved { ui.label(format!("{} ({})",model.model_key,model.identifier)); }
                    });
                },
                Modal::Verify=> {ui.label("This live test captures settings, unloads models and restores them. It can interrupt current inference. Recovery safeguards and fresh game checks remain in force.");},
                Modal::Discard=> {ui.label("Advanced has edits that were not saved. Discard them and go back to games, or keep editing.");},
                Modal::Help=> {ui.label("Tab / Shift+Tab moves focus. Enter / Space activates controls. Arrow keys select table rows. Escape closes this dialog or returns to Games. F1 opens this help. Closing the dashboard keeps the tray watcher running. Quit uses the existing safe shutdown path.");},
            }
            if !self.validation.is_empty(){ui.colored_label(p.error,&self.validation);}
            ui.add_space(design::GAP);
            ui.horizontal(|ui| {
                let cancel_button=ui.button(match modal {Modal::Help=>"Close",Modal::Discard=>"Keep editing",_=>"Cancel"});
                // Cancel is first in keyboard order. Dangerous actions require explicit activation.
                if first {cancel_button.request_focus();}
                cancel=cancel_button.clicked();
                let label=match modal {Modal::Add{..}=>"Add game",Modal::Rename(..)=>"Rename",Modal::Remove(..)=>"Remove",Modal::Resume(..)=>"Resume AI",Modal::Verify=>"Test round-trip",Modal::ReplaceLM(_)=>"Unload current and restore saved",Modal::Discard=>"Discard",Modal::Help=>""};
                if !label.is_empty(){accepted=ui.add_enabled(!s.commands.settings_pending,Button::new(label).fill(p.selected).stroke(Stroke::new(1.0_f32,p.accent))).clicked();}
            });
        });
        cancel |= response.should_close();
        if cancel {
            self.modal_active = false;
            // Closing help or returning to the edits cancels nothing.
            if !matches!(modal, Modal::Help | Modal::Discard) {
                self.validation.clear();
                crate::app::local_result(
                    &self.shared,
                    Outcome::Cancelled,
                    "Dialog cancelled; AI and preferences unchanged.",
                );
            }
            return;
        }
        if accepted {
            self.modal_active = false;
            match &modal {
                Modal::Add { name, path, auto } => {
                    if name.trim().is_empty() {
                        self.validation = "Enter a game name.".into();
                    } else if !std::path::Path::new(path).is_absolute()
                        || !path.to_lowercase().ends_with(".exe")
                        || !std::path::Path::new(path).is_file()
                    {
                        self.validation =
                            "Choose an existing executable with an absolute path.".into();
                    } else {
                        let mut c = s.config.clone();
                        add_game(&mut c, path.clone(), name.trim().into());
                        set_ignored(&mut c, path, !auto);
                        self.save(c, false);
                        if self.validation.is_empty() {
                            return;
                        }
                    }
                }
                Modal::Rename(row, name) => match apply_rename(&s.config, &row.path, name) {
                    Ok(c) => {
                        self.save(c, false);
                        if self.validation.is_empty() {
                            return;
                        }
                    }
                    Err(e) => self.validation = e,
                },
                Modal::Remove(row) => {
                    // The worker validates exact custom path/name again before persisting.
                    self.action(
                        Action::RemoveCustom {
                            path: row.path.clone(),
                            name: row.name.clone(),
                        },
                        "Remove custom game",
                    );
                    return;
                }
                Modal::Resume(offer, checked) => match confirmed_resume(s, offer, checked) {
                    Ok(action) => {
                        self.action(action, "Resume AI during gameplay");
                        return;
                    }
                    Err(error) => self.validation = error,
                },
                Modal::ReplaceLM(offer) => match confirmed_lm_replacement(s, offer) {
                    Ok(action) => {
                        self.action(action, "Replace LM Studio loadout");
                        return;
                    }
                    Err(error) => self.validation = error,
                },
                Modal::Verify => {
                    if crate::ui_commands::verify_available(s) {
                        crate::app::request_verify(&self.shared, &self.tx);
                        return;
                    }
                    self.validation="The test is no longer available. Wait for games, recovery or current work to finish.".into();
                }
                Modal::Discard => {
                    self.discard_draft(s);
                    self.action(Action::AdvancedVisibility(false), "Close Advanced");
                    return;
                }
                Modal::Help => return,
            }
        }
        self.modal = Some(modal);
    }
}
