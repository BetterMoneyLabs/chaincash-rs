use crate::context::{ContextProvider, PredicateContext};
use crate::predicates::Accept;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum WhitelistKind {
    Issuer,
    Owner,
    Historical,
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
pub struct Whitelist {
    pub(crate) agents: Vec<String>,
    pub(crate) kind: WhitelistKind,
}

impl Accept for Whitelist {
    fn accept<P: ContextProvider>(&self, context: &PredicateContext<P>) -> bool {
        match self.kind {
            WhitelistKind::Issuer => self.agents.contains(&context.note.issuer),
            WhitelistKind::Owner => self.agents.contains(&context.note.owner),
            WhitelistKind::Historical => {
                context.note.signers.iter().any(|s| self.agents.contains(s))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{test_util::*, NoteContext};

    #[test]
    fn test_returns_true_if_owner_whitelisted() {
        let issuer_pk = "issuer1".to_owned();
        let note = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner1".to_owned(),
            signers: vec![issuer_pk.clone()],
        };
        let provider = TestContextProvider { agents: vec![] };
        let context = PredicateContext { note, provider };
        let p = Whitelist {
            agents: vec!["PK0".to_string(), "owner1".to_string()],
            kind: WhitelistKind::Owner,
        };

        assert!(p.accept(&context))
    }

    #[test]
    fn test_returns_false_if_owner_not_whitelisted() {
        let issuer_pk = "issuer1".to_owned();
        let note = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner1".to_owned(),
            signers: vec![issuer_pk.clone()],
        };
        let provider = TestContextProvider { agents: vec![] };
        let context = PredicateContext { note, provider };
        let p = Whitelist {
            agents: vec!["PK0".to_string(), "PK2".to_string()],
            kind: WhitelistKind::Owner,
        };
        assert!(!p.accept(&context))
    }

    #[test]
    fn test_returns_true_if_issuer_whitelisted() {
        let issuer_pk = "issuer1".to_owned();
        let note = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner1".to_owned(),
            signers: vec![issuer_pk.clone()],
        };
        let provider = TestContextProvider { agents: vec![] };
        let context = PredicateContext { note, provider };
        let p = Whitelist {
            agents: vec!["PK0".to_string(), "issuer1".to_string()],
            kind: WhitelistKind::Issuer,
        };

        assert!(p.accept(&context))
    }

    #[test]
    fn test_returns_false_if_issuer_not_whitelisted() {
        let issuer_pk = "issuer1".to_owned();
        let note = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner1".to_owned(),
            signers: vec![issuer_pk.clone()],
        };
        let provider = TestContextProvider { agents: vec![] };
        let context = PredicateContext { note, provider };
        let p = Whitelist {
            agents: vec!["PK0".to_string(), "PK2".to_string()],
            kind: WhitelistKind::Issuer,
        };
        assert!(!p.accept(&context))
    }

    #[test]
    fn test_returns_true_if_historical_signer_whitelisted() {
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
        let p = Whitelist {
            agents: vec!["PK0".to_string(), "signer1".to_string()],
            kind: WhitelistKind::Historical,
        };

        assert!(p.accept(&context))
    }

    #[test]
    fn test_returns_false_if_historical_signer_not_whitelisted() {
        let issuer_pk = "issuer1".to_owned();
        let note = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner1".to_owned(),
            signers: vec![issuer_pk.clone(), "another1".to_owned()],
        };
        let provider = TestContextProvider { agents: vec![] };
        let context = PredicateContext { note, provider };
        let p = Whitelist {
            agents: vec!["PK0".to_string(), "PK2".to_string(), "owner1".to_owned()],
            kind: WhitelistKind::Historical,
        };
        assert!(!p.accept(&context))
    }
}
