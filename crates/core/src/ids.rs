//! Strongly typed identifiers.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            /// Generates a fresh random identifier.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Uuid::parse_str(s).map(Self)
            }
        }
    };
}

id_type!(
    /// Identifies an agent.
    AgentId
);
id_type!(
    /// Identifies a registered repository.
    RepoId
);
id_type!(
    /// Identifies a chat message.
    MessageId
);
id_type!(
    /// Identifies an intent.
    IntentId
);
id_type!(
    /// Identifies a question inside an intent.
    QuestionId
);
id_type!(
    /// Identifies a precondition, criterion, story or dependency item.
    ItemId
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_and_from_str_round_trip() {
        let id = AgentId::new();
        assert_eq!(id.to_string().parse::<AgentId>().unwrap(), id);
        assert!("nope".parse::<RepoId>().is_err());
    }
}
