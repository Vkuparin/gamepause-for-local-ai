//! Transient user-command results, separate from engine state and verification history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Requested,
    Working,
    Completed,
    NoChange,
    Cancelled,
    Failed,
}
#[derive(Clone, Debug)]
pub struct CommandResult {
    pub id: u64,
    pub outcome: Outcome,
    pub message: String,
}
#[derive(Clone, Debug)]
pub enum Waiting {
    Pause,
    Restore,
    GameplayRestore(String),
}
#[derive(Clone, Default)]
pub struct Commands {
    next: u64,
    pub latest: Option<CommandResult>,
    pub settings_pending: bool,
    pub waiting: Option<(u64, Waiting)>,
    refresh: Option<(u64, Option<u64>)>,
}
impl Commands {
    pub fn begin(&mut self, message: impl Into<String>) -> u64 {
        self.next = self.next.checked_add(1).expect("command ID exhausted");
        self.latest = Some(CommandResult {
            id: self.next,
            outcome: Outcome::Requested,
            message: message.into(),
        });
        self.next
    }
    pub fn update(&mut self, id: u64, outcome: Outcome, message: impl Into<String>) {
        if let Some(result) = &mut self.latest
            && result.id == id
        {
            result.outcome = outcome;
            result.message = message.into();
        }
    }
    pub fn local(&mut self, outcome: Outcome, message: impl Into<String>) {
        let id = self.begin(message);
        if let Some(result) = &mut self.latest {
            debug_assert_eq!(result.id, id);
            result.outcome = outcome;
        }
    }
    pub fn request_refresh(&mut self) -> Option<u64> {
        if self.refresh.is_some() {
            let id = self.refresh.unwrap().0;
            self.update(
                id,
                Outcome::Working,
                "A discovery refresh is already queued or in progress.",
            );
            if self
                .latest
                .as_ref()
                .is_none_or(|r| Some(r.id) != self.refresh.map(|r| r.0))
            {
                self.local(
                    Outcome::NoChange,
                    "A discovery refresh is already in progress.",
                );
            }
            return None;
        }
        let id = self.begin("Discovery refresh requested.");
        self.refresh = Some((id, None));
        Some(id)
    }
    pub fn bind_refresh(&mut self, id: u64, generation: u64) {
        if self.refresh.is_some_and(|r| r.0 == id) {
            self.refresh = Some((id, Some(generation)));
            self.update(id, Outcome::Working, "Refreshing installed games...");
        }
    }
    pub fn refresh_failed(&mut self, id: u64, message: String) {
        if self.refresh.is_some_and(|r| r.0 == id) {
            self.refresh = None;
        }
        self.update(id, Outcome::Failed, message);
    }
    pub fn discovery_unavailable(&mut self, message: String) {
        if let Some((id, _)) = self.refresh {
            self.refresh_failed(id, message);
        }
    }
    pub fn accept_inventory(&mut self, generation: u64, changed: bool, errors: &str) {
        let Some((id, Some(target))) = self.refresh else {
            return;
        };
        if generation < target {
            return;
        }
        self.refresh = None;
        let (outcome, message) = if !errors.is_empty() {
            (
                Outcome::Failed,
                format!("Discovery refresh has errors: {errors}. Last-good entries retained."),
            )
        } else if changed {
            (Outcome::Completed, "Installed games refreshed.".into())
        } else {
            (
                Outcome::NoChange,
                "Discovery refresh completed; no changes.".into(),
            )
        };
        self.update(id, outcome, message);
    }
    pub fn observe_engine(&mut self, paused: bool, pending: bool, error: &str, progress: &str) {
        let Some((id, waiting)) = self.waiting.clone() else {
            return;
        };
        let done = match waiting {
            Waiting::Pause => paused,
            Waiting::Restore | Waiting::GameplayRestore(_) => !pending,
        };
        let prefix = match &waiting {
            Waiting::GameplayRestore(exclusions) => format!("{exclusions}  |  "),
            _ => String::new(),
        };
        if !error.is_empty() {
            self.waiting = None;
            self.update(
                id,
                Outcome::Failed,
                format!("{prefix}Command failed: {error}. Recovery retained if pending."),
            );
        } else if done {
            self.waiting = None;
            self.update(
                id,
                Outcome::Completed,
                format!(
                    "{prefix}{}",
                    match waiting {
                        Waiting::Pause => "AI pause completed and verified.",
                        Waiting::Restore | Waiting::GameplayRestore(_) =>
                            "AI restoration completed and verified.",
                    }
                ),
            );
        } else {
            self.update(id, Outcome::Working, format!("{prefix}{progress}"));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn accepted_refresh_and_gameplay_completion_keep_their_results() {
        let mut commands = Commands::default();
        let id = commands.request_refresh().unwrap();
        commands.bind_refresh(id, 4);
        commands.accept_inventory(4, true, "");
        assert_eq!(
            commands.latest.as_ref().unwrap().outcome,
            Outcome::Completed
        );
        let id = commands.begin("Restore requested.");
        commands.waiting = Some((
            id,
            Waiting::GameplayRestore("Selected Ignore preferences saved.".into()),
        ));
        commands.observe_engine(false, true, "", "Restoring");
        assert!(
            commands
                .latest
                .as_ref()
                .unwrap()
                .message
                .contains("Ignore preferences saved")
        );
        commands.observe_engine(false, false, "", "Coexistence");
        assert!(
            commands
                .latest
                .as_ref()
                .unwrap()
                .message
                .contains("Ignore preferences saved")
        );
        assert_eq!(
            commands.latest.as_ref().unwrap().outcome,
            Outcome::Completed
        );
        let id = commands.request_refresh().unwrap();
        commands.bind_refresh(id, 5);
        commands.discovery_unavailable("Fixture discovery worker stopped.".into());
        assert_eq!(commands.latest.as_ref().unwrap().outcome, Outcome::Failed);
        assert!(commands.request_refresh().is_some());
    }
    #[test]
    fn refresh_coalesces_and_waits_for_its_generation() {
        let mut commands = Commands::default();
        let id = commands.request_refresh().unwrap();
        commands.bind_refresh(id, 3);
        assert!(commands.request_refresh().is_none());
        commands.accept_inventory(2, true, "");
        assert_eq!(commands.latest.as_ref().unwrap().outcome, Outcome::Working);
        commands.accept_inventory(3, false, "");
        assert_eq!(commands.latest.as_ref().unwrap().outcome, Outcome::NoChange);
        assert!(commands.request_refresh().is_some());
    }
    #[test]
    fn older_refresh_and_engine_work_never_replace_newer_results() {
        let mut commands = Commands::default();
        let id = commands.request_refresh().unwrap();
        commands.bind_refresh(id, 3);
        commands.waiting = Some((id, Waiting::Restore));
        commands.local(Outcome::Cancelled, "Picker cancelled.");
        commands.accept_inventory(3, true, "adapter failure");
        commands.observe_engine(false, false, "", "Watching");
        assert_eq!(
            commands.latest.as_ref().unwrap().message,
            "Picker cancelled."
        );
        assert!(commands.request_refresh().is_some());
    }
    #[test]
    fn failures_finish_refresh_and_engine_feedback_survives_idle() {
        let mut commands = Commands::default();
        let id = commands.request_refresh().unwrap();
        commands.bind_refresh(id, 1);
        commands.accept_inventory(1, false, "Fixture launcher unavailable");
        assert_eq!(commands.latest.as_ref().unwrap().outcome, Outcome::Failed);
        let id = commands.begin("Pause requested.");
        commands.waiting = Some((id, Waiting::Pause));
        commands.observe_engine(false, true, "", "Unloading");
        assert_eq!(commands.latest.as_ref().unwrap().outcome, Outcome::Working);
        commands.observe_engine(true, true, "", "Paused");
        commands.observe_engine(false, false, "", "Watching");
        assert_eq!(
            commands.latest.as_ref().unwrap().outcome,
            Outcome::Completed
        );
    }
}
