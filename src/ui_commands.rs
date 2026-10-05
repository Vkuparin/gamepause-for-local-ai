//! Native command IDs and visibility shared by the dashboard and tray.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(i32)]
pub enum Command {
    Automation = 101,
    Startup = 102,
    Refresh = 106,
    Resume = 113,
    Pause = 114,
    Verify = 127,
    OpenFolder = 134,
    Quit = 135,
    OpenDashboard = 136,
    Doctor = 151,
}
impl Command {
    pub const ALL: [Self; 10] = [
        Self::Automation,
        Self::Startup,
        Self::Refresh,
        Self::Resume,
        Self::Pause,
        Self::Verify,
        Self::OpenFolder,
        Self::Quit,
        Self::OpenDashboard,
        Self::Doctor,
    ];
    pub fn advanced(self) -> bool {
        matches!(
            self,
            Self::Startup | Self::Verify | Self::OpenFolder | Self::Doctor
        )
    }
    pub fn visible(self, advanced: bool) -> bool {
        !self.advanced() || advanced
    }
    pub fn from_id(id: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|command| *command as i32 == id)
    }
}

pub fn allowed(shared: &crate::app::SharedState, command: Command) -> bool {
    let visible = shared
        .lock()
        .is_ok_and(|state| command.visible(state.config.advanced_settings_visible));
    if !visible {
        crate::app::local_result(
            shared,
            crate::commands::Outcome::Failed,
            "Advanced settings is hidden; show it before using this tool.",
        );
    }
    visible
}
pub fn verify_available(state: &crate::app::Shared) -> bool {
    state.config.advanced_settings_visible
        && state.config.lm_enabled()
        && state.active_mode
        && state.discovery_ready
        && state.detection_ok
        && !state.disabled
        && !state.pending
        && !state.manual_pause
        && !state.verifying
        && !state.activity.busy()
        && state.active_games.is_empty()
        && state.discovery_errors.is_empty()
}
