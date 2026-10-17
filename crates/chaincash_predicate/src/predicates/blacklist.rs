use crate::context::{ContextProvider, PredicateContext};
use crate::predicates::Accept;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum BlacklistKind {
    Issuer,
    Owner,
    Historical,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct Blacklist {
    pub(crate) agents: Vec<String>,
    pub(crate) kind: BlacklistKind,
}

impl Accept for Blacklist {
    fn accept<P: ContextProvider>(&self, context: &PredicateContext<P>) -> bool {
        match self.kind {
            BlacklistKind::Issuer => !self.agents.contains(&context.note.issuer),
            BlacklistKind::Owner => !self.agents.contains(&context.note.owner),
            BlacklistKind::Historical => context
                .note
                .signers
                .iter()
                .all(|s| !self.agents.contains(s)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{test_util::*, NoteContext};

    #[test]
    fn test_returns_false_if_owner_blacklisted() {
        let issuer_pk = "issuer1".to_owned();
        let note = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner1".to_owned(),
            signers: vec![issuer_pk.clone()],
        };
        let provider = TestContextProvider { agents: vec![] };
        let context = PredicateContext { note, provider };
        let p = Blacklist {
            agents: vec!["PK0".to_string(), "owner1".to_string()],
            kind: BlacklistKind::Owner,
        };

        assert!(!p.accept(&context))
    }

    #[test]
    fn test_returns_true_if_owner_not_blacklisted() {
        let issuer_pk = "issuer1".to_owned();
        let note = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner1".to_owned(),
            signers: vec![issuer_pk.clone()],
        };
        let provider = TestContextProvider { agents: vec![] };
        let context = PredicateContext { note, provider };
        let p = Blacklist {
            agents: vec!["PK0".to_string(), "PK2".to_string()],
            kind: BlacklistKind::Owner,
        };
        assert!(p.accept(&context))
    }

    #[test]
    fn test_returns_false_if_issuer_blacklisted() {
        let issuer_pk = "issuer1".to_owned();
        let note = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner1".to_owned(),
            signers: vec![issuer_pk.clone()],
        };
        let provider = TestContextProvider { agents: vec![] };
        let context = PredicateContext { note, provider };
        let p = Blacklist {
            agents: vec!["PK0".to_string(), "issuer1".to_string()],
            kind: BlacklistKind::Issuer,
        };

        assert!(!p.accept(&context))
    }

    #[test]
    fn test_returns_true_if_issuer_not_blacklisted() {
        let issuer_pk = "issuer1".to_owned();
        let note = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner1".to_owned(),
            signers: vec![issuer_pk.clone()],
        };
        let provider = TestContextProvider { agents: vec![] };
        let context = PredicateContext { note, provider };
        let p = Blacklist {
            agents: vec!["PK0".to_string(), "PK2".to_string()],
            kind: BlacklistKind::Issuer,
        };
        assert!(p.accept(&context))
    }

    #[test]
    fn test_returns_false_if_historical_signer_blacklisted() {
        let issuer_pk = "issuer1".to_owned();
        let note = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner1".to_owned(),
            signers: vec![
                issuer_pk.clone(),
                "signer1".to_owned(),
                "next_owner".to_owned(),
            ],
        };
        let provider = TestContextProvider { agents: vec![] };
        let context = PredicateContext { note, provider };
        let p = Blacklist {
            agents: vec!["PK0".to_string(), "signer1".to_string()],
            kind: BlacklistKind::Historical,
        };

        assert!(!p.accept(&context))
    }

    #[test]
    fn test_returns_true_if_historical_signer_not_blacklisted() {
        let issuer_pk = "issuer1".to_owned();
        let note = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner1".to_owned(),
            signers: vec![issuer_pk.clone(), "another1".to_owned()],
        };
        let provider = TestContextProvider { agents: vec![] };
        let context = PredicateContext { note, provider };
        let p = Blacklist {
            agents: vec!["PK0".to_string(), "PK2".to_string(), "owner1".to_owned()],
            kind: BlacklistKind::Historical,
        };
        assert!(p.accept(&context))
    }
}
