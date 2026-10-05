//! Transient, process-instance-scoped authority. Never serialized into recovery.
use crate::{
    config::Config,
    discovery::{Game, canonical},
    processes::ActiveGame,
};
use anyhow::{Result, bail};

#[derive(Clone, Debug)]
pub struct GameEvidence {
    pub all: Vec<ActiveGame>,
    pub triggers: Vec<ActiveGame>,
}

fn reliable(game: &ActiveGame) -> bool {
    game.pid != 0
        && game.created_at != 0
        && std::path::Path::new(&game.executable).is_absolute()
        && std::path::Path::new(&game.path).is_absolute()
}
fn same(a: &ActiveGame, b: &ActiveGame) -> bool {
    a.pid == b.pid
        && a.created_at == b.created_at
        && canonical(&a.executable) == canonical(&b.executable)
}
fn exact(a: &[ActiveGame], b: &[ActiveGame]) -> bool {
    a.len() == b.len()
        && a.iter().all(|game| b.iter().any(|other| same(game, other)))
        && b.iter().all(|game| a.iter().any(|other| same(game, other)))
}
impl GameEvidence {
    pub fn reliable(&self) -> bool {
        self.all.iter().all(reliable)
            && self
                .all
                .iter()
                .enumerate()
                .all(|(index, game)| !self.all[..index].iter().any(|prior| prior.pid == game.pid))
            && self
                .triggers
                .iter()
                .all(|game| self.all.iter().any(|other| same(game, other)))
    }
    pub fn gaming(&self) -> bool {
        !self.all.is_empty()
    }
}

#[cfg(test)]
pub(crate) mod fixtures {
    use super::*;
    pub fn game(pid: u32, created_at: u64) -> ActiveGame {
        ActiveGame {
            pid,
            created_at,
            executable: format!(r"D:\Fixture Games\Game {pid}\play.exe"),
            path: format!(r"D:\Fixture Games\Game {pid}"),
            game: format!("Fixture game {pid}"),
            launcher: "Fixture".into(),
        }
    }
    pub fn evidence(games: Vec<ActiveGame>) -> GameEvidence {
        GameEvidence {
            triggers: games.clone(),
            all: games,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::{evidence, game};
    use super::*;
    #[test]
    fn offer_is_one_use_and_never_broadens_to_new_games_or_pid_reuse() {
        let original = evidence(vec![game(42, 10)]);
        let mut policy = GameplayControl::default();
        policy.refresh_offer(Some(&original), true);
        let id = policy.offer().unwrap().id;
        let changed = evidence(vec![game(42, 11)]);
        assert!(policy.confirm(id, &changed).is_err());
        assert!(!policy.active());
        policy.refresh_offer(Some(&original), true);
        let current = policy.offer().unwrap().id;
        assert_ne!(id, current);
        assert!(policy.confirm(id, &original).is_err());
        policy.confirm(current, &original).unwrap();
        assert!(policy.confirm(current, &original).is_err());
        assert!(
            policy.active(),
            "duplicate confirmation cannot alter an accepted scope"
        );
        policy.reconcile(Some(&changed));
        assert!(!policy.active());
    }
    #[test]
    fn new_nonignored_game_exit_and_unknown_detection_revoke_authority() {
        let original = evidence(vec![game(42, 10)]);
        for changed in [
            Some(evidence(vec![game(42, 10), game(7, 2)])),
            Some(evidence(vec![])),
            None,
        ] {
            let mut policy = GameplayControl::default();
            policy.refresh_offer(Some(&original), true);
            policy
                .confirm(policy.offer().unwrap().id, &original)
                .unwrap();
            policy.reconcile(changed.as_ref());
            assert!(!policy.active());
        }
        let mut policy = GameplayControl::default();
        policy.refresh_offer(Some(&original), true);
        policy
            .confirm(policy.offer().unwrap().id, &original)
            .unwrap();
        let mut ignored_new = evidence(vec![game(42, 10), game(7, 2)]);
        ignored_new.triggers = vec![game(42, 10)];
        policy.reconcile(Some(&ignored_new));
        assert!(
            policy.active(),
            "an ignored new game does not expand approved authority"
        );
    }
    #[test]
    fn missing_identity_duplicate_process_or_unknown_detection_never_offers_restore() {
        let mut missing = game(42, 0);
        for invalid in [
            evidence(vec![missing.clone()]),
            evidence(vec![game(42, 10), game(42, 10)]),
        ] {
            let mut policy = GameplayControl::default();
            policy.refresh_offer(Some(&invalid), true);
            assert!(policy.offer().is_none());
        }
        missing.created_at = 10;
        missing.executable.clear();
        assert!(!evidence(vec![missing]).reliable());
        let mut policy = GameplayControl::default();
        policy.refresh_offer(None, true);
        assert!(policy.offer().is_none());
    }
    #[test]
    fn ignore_selections_only_add_selected_executables_and_keep_other_preferences() {
        let games = vec![game(42, 10), game(7, 2)];
        let mut config = Config::default();
        config
            .excluded_paths
            .push(r"D:\Fixture Other\tool.exe".into());
        let updated = selected_exclusions(
            &config,
            &games,
            &[
                games[1].executable.to_uppercase(),
                games[1].executable.clone(),
            ],
        )
        .unwrap();
        assert_eq!(
            updated.excluded_paths,
            vec![
                config.excluded_paths[0].clone(),
                games[1].executable.clone()
            ]
        );
        assert!(updated.automation_enabled);
        assert!(updated.ignored_games.is_empty());
        assert_eq!(
            selected_exclusions(&config, &games, &[])
                .unwrap()
                .excluded_paths,
            config.excluded_paths
        );
        assert!(selected_exclusions(&config, &games, &[games[0].path.clone()]).is_err());
        assert!(selected_exclusions(&config, &games, &[r"D:\Unrelated\game.exe".into()]).is_err());
    }
}

#[derive(Clone, Debug)]
pub struct RestoreOffer {
    pub id: u64,
    pub games: Vec<ActiveGame>,
}

#[derive(Default)]
pub struct GameplayControl {
    offer: Option<RestoreOffer>,
    approved: Option<Vec<ActiveGame>>,
    next_id: u64,
}
impl GameplayControl {
    pub fn active(&self) -> bool {
        self.approved.is_some()
    }
    pub fn offer(&self) -> Option<RestoreOffer> {
        self.offer.clone()
    }
    pub fn revoke(&mut self) {
        self.approved = None;
        self.offer = None;
    }
    pub fn invalidate_offer(&mut self) {
        self.offer = None;
    }
    pub fn remembered_games(&self) -> Vec<Game> {
        self.approved
            .iter()
            .flatten()
            .map(|game| Game {
                identity: format!("confirmed-{}-{}", game.pid, game.created_at),
                name: game.game.clone(),
                path: game.path.clone(),
                launcher: game.launcher.clone(),
            })
            .collect()
    }
    /// Unknown detection, an approved process exiting, or a new trigger revokes
    /// the exception. A newly ignored process cannot broaden the approved set.
    pub fn reconcile(&mut self, evidence: Option<&GameEvidence>) {
        let Some(evidence) = evidence.filter(|evidence| evidence.reliable()) else {
            self.revoke();
            return;
        };
        if self.approved.as_ref().is_some_and(|approved| {
            !approved
                .iter()
                .all(|game| evidence.all.iter().any(|live| same(game, live)))
                || evidence
                    .triggers
                    .iter()
                    .any(|game| !approved.iter().any(|allowed| same(game, allowed)))
        }) {
            self.revoke();
        }
        if self
            .offer
            .as_ref()
            .is_some_and(|offer| !exact(&offer.games, &evidence.all))
        {
            self.offer = None;
        }
    }
    pub fn refresh_offer(&mut self, evidence: Option<&GameEvidence>, eligible: bool) {
        self.reconcile(evidence);
        let Some(evidence) =
            evidence.filter(|e| eligible && e.reliable() && e.gaming() && !self.active())
        else {
            self.offer = None;
            return;
        };
        if self.offer.is_none() {
            self.next_id = self
                .next_id
                .checked_add(1)
                .expect("restore offer ID exhausted");
            self.offer = Some(RestoreOffer {
                id: self.next_id,
                games: evidence.all.clone(),
            });
        }
    }
    pub fn confirm(&mut self, id: u64, evidence: &GameEvidence) -> Result<Vec<ActiveGame>> {
        if self.offer.as_ref().is_none_or(|offer| offer.id != id) {
            bail!("Restore confirmation expired; open Restore again for the current games");
        }
        let offer = self.offer.take().unwrap();
        if self.active() || !evidence.reliable() || !exact(&offer.games, &evidence.all) {
            bail!("Running games changed or detection is unknown; Restore confirmation refused");
        }
        self.approved = Some(offer.games.clone());
        Ok(offer.games)
    }
}

/// Selections can name only executables in the accepted offer. Unselected
/// games and broader registration folders are never added implicitly.
pub fn selected_exclusions(
    config: &Config,
    games: &[ActiveGame],
    selected: &[String],
) -> Result<Config> {
    let mut updated = config.clone();
    for path in selected {
        let key = canonical(path);
        let Some(game) = games
            .iter()
            .find(|game| reliable(game) && canonical(&game.executable) == key)
        else {
            bail!("Ignore selection does not belong to the confirmed running games");
        };
        if !updated
            .excluded_paths
            .iter()
            .any(|path| canonical(path) == key)
        {
            updated.excluded_paths.push(game.executable.clone());
        }
    }
    updated.validate()?;
    Ok(updated)
}

#[derive(Clone, Debug, Default)]
pub struct RestoreFeedback {
    pub accepted: bool,
    pub preference_failure: bool,
    pub exclusions: String,
    pub restoration: String,
    pub tracking_recovery: bool,
}
impl RestoreFeedback {
    pub fn text(&self) -> String {
        format!(
            "{}{}{}",
            self.exclusions,
            if self.exclusions.is_empty() {
                ""
            } else {
                "  |  "
            },
            self.restoration
        )
    }
}
