//! One agent turn = one undo step. A [`Checkpoint`] remembers the project before the turn; at the
//! end [`Checkpoint::collapse`] folds every step the turn made into one entry named after it, and
//! [`Checkpoint::revert`] undoes that entry (it stays redoable, so a reverted turn isn't lost).

use std::sync::Arc;

use effectcraft_engine::history::state_id;
use effectcraft_engine::project::Project;
use effectcraft_engine::{History, Session};

#[derive(Clone)]
pub struct Checkpoint {
    snap: Arc<Project>,
    prior: History,
    saved: Arc<Project>,
}

impl Checkpoint {
    /// Remember the session's current state.
    pub fn take(s: &Session) -> Checkpoint {
        Checkpoint { snap: s.project.clone(), prior: s.history.clone(), saved: s.saved_project.clone() }
    }

    /// Id of the state before the turn (`edit.history.goto {id}` returns to it).
    pub fn state_id(&self) -> String {
        state_id(&self.snap)
    }

    /// Whether the project changed since the checkpoint.
    pub fn changed(&self, s: &Session) -> bool {
        !Arc::ptr_eq(&s.project, &self.snap)
    }

    /// Fold the steps since the checkpoint into one undo entry labelled `label`. Returns whether
    /// there was anything to fold. Also when the turn made more steps than Levels of Undo keeps
    /// (as the script runtime's undo groups do); a New / Open Project in between is left alone.
    pub fn collapse(&self, s: &mut Session, label: &str) -> bool {
        if !self.changed(s) {
            return false;
        }
        let h = &mut s.history;
        if let Some(start) = h.undo.iter().rposition(|(_, p)| Arc::ptr_eq(p, &self.snap)) {
            let Some((_, first)) = h.undo.get(start).cloned() else { return false };
            h.undo.truncate(start);
            h.undo.push((label.to_string(), first));
            h.merge_key = None;
            true
        } else if Arc::ptr_eq(&s.saved_project, &self.saved) && !h.undo.is_empty() {
            *h = self.prior.clone();
            h.record(label, self.snap.clone(), s.prefs.general.undo_levels.max(1) as usize);
            true
        } else {
            false
        }
    }

    /// Whether the turn (after [`Checkpoint::collapse`]) is still the latest undo step, so
    /// [`Checkpoint::revert`] would undo exactly it.
    pub fn is_latest(&self, s: &Session) -> bool {
        self.changed(s) && s.history.undo.last().is_some_and(|(_, p)| Arc::ptr_eq(p, &self.snap))
    }

    /// Undo the turn after [`Checkpoint::collapse`]: only when the turn is still the latest step,
    /// so a later edit of the user's is never undone with it. Returns whether it reverted.
    pub fn revert(&self, s: &mut Session) -> bool {
        if !self.is_latest(s) {
            return false;
        }
        s.execute("edit.undo", serde_json::json!({})).is_ok() && !self.changed(s)
    }
}
