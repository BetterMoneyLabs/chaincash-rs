use serde::{Deserialize, Serialize};

pub type NanoErg = u64;
pub type PubKeyHex = String;

/// Context related to a note that holds information required by predicates
/// to determine if the note is acceptable.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct NoteContext {
    /// The nanoerg value of the related note
    /// The denomination of the note converted to its erg value
    pub nanoerg: NanoErg,
    /// Owner of the note as hex encoded public key
    pub owner: PubKeyHex,
    /// Issuer of the note as hex encoded public key
    pub issuer: PubKeyHex,
    /// Agents that have signed and traded the note
    pub signers: Vec<PubKeyHex>,
}

/// Implementors provide a way to access extra context when processing a note
/// inside a predicate.
pub trait ContextProvider {
    /// Get all notes as `NoteContext` issued by the specified agent
    fn agent_issued_notes(&self, agent: &str) -> Vec<NoteContext>;

    /// Get the amount of reserves the specified agent has
    fn agent_reserves_nanoerg(&self, agent: &str) -> NanoErg;
}

/// Context passed to predicates during evaluation
pub struct PredicateContext<P: ContextProvider> {
    pub note: NoteContext,
    pub provider: P,
}

#[cfg(test)]
pub(crate) mod test_util {
    use super::*;

    pub struct TestAgent {
        pub pk: String,
        pub issued_notes: Vec<NoteContext>,
        pub reserves: u64,
    }

    pub struct TestContextProvider {
        pub agents: Vec<TestAgent>,
    }

    impl ContextProvider for TestContextProvider {
        fn agent_issued_notes(&self, agent: &str) -> Vec<NoteContext> {
            self.agents
                .iter()
                .find(|n| n.pk == agent)
                .map(|a| a.issued_notes.clone())
                .unwrap_or_default()
        }

        fn agent_reserves_nanoerg(&self, agent: &str) -> u64 {
            self.agents
                .iter()
                .find(|n| n.pk == agent)
                .map(|a| a.reserves)
                .unwrap_or_default()
        }
    }
}
