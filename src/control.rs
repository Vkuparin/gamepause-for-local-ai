//! Worker evidence and the core command policy shared by native UI surfaces.
//! A pending recovery journal is an obligation, not proof of a completed pause.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    #[default]
    Unknown,
    Watching,
    Observation,
    Unavailable,
    DetectionUnavailable,
    Capturing,
    WaitingForInference,
    Unloading,
    Paused,
    ManualHold,
    Countdown,
    Restoring,
    Recovery,
    PartialFailure,
    Verifying,
    Coexistence,
}

impl Activity {
    pub fn busy(self) -> bool {
        matches!(
            self,
            Self::Capturing | Self::Unloading | Self::Restoring | Self::Verifying
        )
    }

    pub fn progress_message(self) -> &'static str {
        match self {
            Self::Capturing => "AI: capturing provider settings before pausing",
            Self::Unloading => "AI: unloading captured models; pause not yet complete",
            Self::Restoring => "AI: restoring saved recovery; completion not yet verified",
            Self::Verifying => "Testing unload and restoration",
            _ => "",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreCommand {
    Pause,
    Resume,
    Restore,
}

#[derive(Clone, Copy, Debug)]
pub struct ControlState {
    pub activity: Activity,
    pub active_mode: bool,
    pub provider_enabled: bool,
    pub detection_ready: bool,
    pub gaming: bool,
    pub pending: bool,
    pub manual_hold: bool,
    pub gameplay_restore: bool,
    pub coexistence: bool,
}

#[derive(Clone, Copy, Debug)]
pub struct Availability {
    pub pause: bool,
    pub resume: bool,
    pub restore: bool,
    pub reason: &'static str,
}

impl Availability {
    pub fn allows(self, command: CoreCommand) -> bool {
        match command {
            CoreCommand::Pause => self.pause,
            CoreCommand::Resume => self.resume,
            CoreCommand::Restore => self.restore,
        }
    }
}

impl ControlState {
    pub fn availability(self) -> Availability {
        let blocked = |reason| Availability {
            pause: false,
            resume: false,
            restore: false,
            reason,
        };
        if !self.active_mode {
            return blocked("Observation mode leaves AI unchanged.");
        }
        if !self.provider_enabled && !self.pending {
            return blocked("No AI provider is enabled. Enable a provider in Advanced settings.");
        }
        if !self.detection_ready
            || matches!(
                self.activity,
                Activity::Unknown | Activity::DetectionUnavailable
            )
        {
            return blocked("Wait for successful game detection before controlling AI.");
        }
        if self.activity.busy() {
            return blocked("An AI operation is already in progress.");
        }
        Availability {
            pause: self.provider_enabled
                && (self.coexistence || (!self.pending && !self.manual_hold)),
            resume: self.manual_hold && !self.gaming,
            restore: self.pending && (!self.gaming || self.gameplay_restore),
            reason: if self.gaming {
                "Resume during gameplay requires current process evidence and explicit confirmation."
            } else if self.pending {
                "Recovery is pending; Resume AI retries it immediately."
            } else {
                "There is no saved AI to resume."
            },
        }
    }
}
