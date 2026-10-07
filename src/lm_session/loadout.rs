//! Read-only loadout acceptance and explicit replacement approvals.
use super::*;
use crate::lmstudio::{LoadoutOffer, Resident};

impl Progress {
    pub fn loadout_offer(&self) -> Option<LoadoutOffer> {
        self.offer.clone()
    }
    pub fn approve_replacement(&mut self, offer: &LoadoutOffer) -> Result<()> {
        if self.offer.as_ref() != Some(offer) {
            bail!("LM Studio loadout changed; review the current prompt again");
        }
        self.replacement = Some(offer.clone());
        self.offer = None;
        Ok(())
    }
    pub fn revoke_replacement(&mut self) {
        self.replacement = None;
    }
}

impl<B: Backend> LMRuntime<B> {
    fn mismatch(&self, snapshot: &Snapshot, loaded: &[Resident]) -> Result<()> {
        *self.offer.borrow_mut() = Some(lmstudio::loadout_offer(snapshot, loaded.to_vec())?);
        self.replacement.borrow_mut().take();
        Err(crate::provider::ManualRetryRequired(
            "Loaded LM Studio model doesn't match saved configuration; do you want to unload the current model(s) and load the saved model(s)? Open GamePause to review. Recovery retained".into()
        ).into())
    }
    pub(super) fn loadout_plan(&mut self, snapshot: &Snapshot) -> Result<Option<Operation>> {
        if self.verify_raw {
            return Ok(None);
        }
        let Some(loaded) = self.backend.recovery_models()? else {
            return Ok(None);
        };
        let approved = self.replacement.borrow().clone();
        if let Some(approved) = approved {
            let current = lmstudio::loadout_offer(snapshot, loaded.clone())?;
            if current.authority != approved.authority
                || loaded.iter().any(|m| !approved.loaded.contains(m))
            {
                self.mismatch(snapshot, &loaded)?;
            }
            if let Some(first) = loaded.first() {
                let index = approved
                    .loaded
                    .iter()
                    .position(|m| m == first)
                    .context("Replacement instance changed")?;
                return Ok(Some(Operation::UnloadReplacement(index)));
            }
            self.replacement.borrow_mut().take();
            self.normal_restore.set(true);
        }
        if lmstudio::loadout_matches(snapshot, &loaded) && !loaded.is_empty() {
            self.offer.borrow_mut().take();
            return Ok(Some(Operation::AcceptLoadout));
        }
        if loaded.is_empty() {
            self.normal_restore.set(true);
            self.offer.borrow_mut().take();
            return Ok(None);
        }
        // The adapter may be halfway through restoring its own saved models.
        if self.normal_restore.get()
            && loaded.iter().all(|resident| {
                snapshot.models.iter().any(|m| {
                    m.identifier == resident.identifier
                        && m.model_key == resident.model_key
                        && m.namespace == resident.namespace
                })
            })
        {
            return Ok(None);
        }
        self.mismatch(snapshot, &loaded)?;
        Ok(None)
    }
    pub(super) fn accept_loadout(&mut self, snapshot: &mut Snapshot) -> Result<()> {
        let loaded = self
            .backend
            .recovery_models()?
            .context("LM Studio residency is unknown")?;
        if !lmstudio::loadout_matches(snapshot, &loaded) || loaded.is_empty() {
            self.mismatch(snapshot, &loaded)?;
        }
        // If this attempt started the service for its own restoration, return
        // that service to its captured state. An external loadout is left alone.
        if self.normal_restore.get() {
            let port = snapshot.server["port"]
                .as_u64()
                .context("Missing captured port")? as u16;
            if snapshot.close_restored_service() {
                self.backend.stop_server()?;
            }
            if self.state(port)? != (snapshot.server["running"] == true) {
                bail!("Original LM Studio server state is unverified; recovery retained");
            }
        }
        for model in &mut snapshot.models {
            model.stage = "restored".into();
        }
        self.accepted_loadout.set(true);
        self.offer.borrow_mut().take();
        Ok(())
    }
    pub(super) fn unload_replacement(&mut self, snapshot: &Snapshot, index: usize) -> Result<()> {
        let approved = self
            .replacement
            .borrow()
            .clone()
            .context("LM Studio replacement approval expired")?;
        let loaded = self
            .backend
            .recovery_models()?
            .context("LM Studio residency is unknown")?;
        let current = lmstudio::loadout_offer(snapshot, loaded.clone())?;
        if current.authority != approved.authority
            || loaded.iter().any(|m| !approved.loaded.contains(m))
        {
            self.mismatch(snapshot, &loaded)?;
        }
        let target = approved
            .loaded
            .get(index)
            .context("Invalid replacement instance")?;
        if loaded.contains(target) {
            self.backend.unload(&target.identifier)?;
        }
        Ok(())
    }
}
