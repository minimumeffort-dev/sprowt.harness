use rusqlite::{OptionalExtension, Result, params};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Appearance {
    #[default]
    System,
    Light,
    Dark,
}

impl Appearance {
    pub const ALL: [Self; 3] = [Self::System, Self::Light, Self::Dark];

    pub fn label(self) -> &'static str {
        match self {
            Self::System => "System",
            Self::Light => "Light",
            Self::Dark => "Dark",
        }
    }
}

impl crate::store::Store {
    pub fn appearance(&self) -> Result<Appearance> {
        let value: Option<String> = self
            .0
            .query_row(
                "SELECT appearance FROM preferences WHERE id = 1",
                [],
                |row| row.get(0),
            )
            .optional()?;
        match value.as_deref() {
            None | Some("system") => Ok(Appearance::System),
            Some("light") => Ok(Appearance::Light),
            Some("dark") => Ok(Appearance::Dark),
            Some(_) => Err(rusqlite::Error::InvalidQuery),
        }
    }

    pub fn set_appearance(&self, appearance: Appearance) -> Result<()> {
        self.0.execute(
            "INSERT INTO preferences (id, appearance) VALUES (1, ?1)
             ON CONFLICT(id) DO UPDATE SET appearance = excluded.appearance",
            params![appearance.label().to_ascii_lowercase()],
        )?;
        Ok(())
    }
}

impl crate::engine::Engine {
    pub fn set_appearance(&mut self, appearance: Appearance) -> Result<()> {
        self.store.set_appearance(appearance)?;
        self.appearance = appearance;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        engine::{
            Engine,
            api::{Command, Request},
        },
        store::test_support::TestData,
    };

    #[test]
    fn appearance_is_shared_across_projects_and_does_not_change_work() {
        let data = TestData::new();
        let mut first = Engine::load(data.0.join("one"), data.store()).unwrap();
        first.composer = "Unsent creation".into();
        assert_eq!(first.appearance, Appearance::System);
        first
            .command(Command::SetAppearance {
                appearance: Appearance::Light,
            })
            .unwrap();
        assert_eq!(first.composer, "Unsent creation");
        assert_eq!(
            serde_json::to_value(first.snapshot()).unwrap()["appearance"],
            "light"
        );
        let mut second = Engine::load(data.0.join("two"), data.store()).unwrap();
        assert_eq!(second.appearance, Appearance::Light);
        second
            .command(Command::SetAppearance {
                appearance: Appearance::Dark,
            })
            .unwrap();
        first.poll_workers().unwrap();
        assert_eq!(first.appearance, Appearance::Dark);
        assert_eq!(first.composer, "Unsent creation");
        drop(first);
        let reopened = Engine::load(data.0.join("one"), data.store()).unwrap();
        assert_eq!(reopened.appearance, Appearance::Dark);
    }

    #[test]
    fn invalid_wire_values_and_failed_saves_leave_the_preference_intact() {
        let data = TestData::new();
        let mut engine = Engine::load(data.0.join("project"), data.store()).unwrap();
        engine
            .command(Command::SetAppearance {
                appearance: Appearance::Light,
            })
            .unwrap();
        let bad = serde_json::from_str::<Request>(
            r#"{"id":1,"command":{"type":"set_appearance","appearance":"gold"}}"#,
        );
        assert!(bad.err().unwrap().to_string().contains("unknown variant"));
        engine
            .store
            .0
            .execute_batch("PRAGMA query_only = ON;")
            .unwrap();
        let error = engine
            .command(Command::SetAppearance {
                appearance: Appearance::Dark,
            })
            .unwrap_err();
        assert!(error.contains("readonly"));
        assert_eq!(engine.appearance, Appearance::Light);
        assert_eq!(engine.store.appearance().unwrap(), Appearance::Light);
    }
}
