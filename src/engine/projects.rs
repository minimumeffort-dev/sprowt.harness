use super::{Engine, View};

impl Engine {
    pub(crate) fn set_creation_draft(&mut self, text: String) -> rusqlite::Result<()> {
        self.store.save_creation_draft(self.project_id, &text)?;
        self.creation_draft = text.clone();
        if self.view == View::NewMod {
            self.composer = text;
        }
        Ok(())
    }
}
