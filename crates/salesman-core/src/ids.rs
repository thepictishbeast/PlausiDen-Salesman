//! Strongly-typed identifiers. Newtypes around UUIDv7 so timestamps
//! sort naturally and you can't accidentally pass a CompanyId where a
//! ProspectId is expected (the boolean-blindness equivalent for ids).

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::now_v7())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

id_type!(CompanyId);
id_type!(ContactId);
id_type!(CampaignId);
id_type!(ProspectId);
id_type!(TouchId);
id_type!(ReceiptId);
