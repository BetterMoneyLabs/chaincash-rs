use crate::context::{ContextProvider, PredicateContext};
use crate::predicates::{Accept, Predicate};
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Or {
    conditions: Vec<Predicate>,
}

impl Or {
    pub(crate) fn uses_collateral(&self) -> bool {
        self.conditions.iter().any(Predicate::uses_collateral)
    }
}

impl Accept for Or {
    fn accept<P: ContextProvider>(&self, context: &PredicateContext<P>) -> bool {
        for condition in &self.conditions {
            if condition.accept(context) {
                return true;
            }
        }

        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{test_util::TestContextProvider, NoteContext};
    use crate::predicates::whitelist::{Whitelist, WhitelistKind};

    #[test]
    fn test_returns_true_if_any_condition_returns_true() {
        let issuer_pk = "issuer1".to_owned();
        let note = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner1".to_owned(),
            signers: vec![issuer_pk.clone()],
        };
        let provider = TestContextProvider { agents: vec![] };
        let context = PredicateContext { note, provider };
        let acceptable = Whitelist {
            agents: vec!["PK0".to_string(), "owner1".to_string()],
            kind: WhitelistKind::Owner,
        };
        let unacceptable = Whitelist {
            agents: vec!["PK0".to_string(), "notowner".to_string()],
            kind: WhitelistKind::Owner,
        };
        let p = Or {
            conditions: vec![
                Predicate::Whitelist(unacceptable),
                Predicate::Whitelist(acceptable),
            ],
        };

        assert!(p.accept(&context))
    }

    #[test]
    fn test_returns_false_if_all_conditions_return_false() {
        let issuer_pk = "issuer1".to_owned();
        let note = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner1".to_owned(),
            signers: vec![issuer_pk.clone()],
        };
        let provider = TestContextProvider { agents: vec![] };
        let context = PredicateContext { note, provider };
        let unacceptable2 = Whitelist {
            agents: vec!["PK0".to_string(), "alsonotowner".to_string()],
            kind: WhitelistKind::Owner,
        };
        let unacceptable = Whitelist {
            agents: vec!["PK0".to_string(), "notowner".to_string()],
            kind: WhitelistKind::Owner,
        };
        let p = Or {
            conditions: vec![
                Predicate::Whitelist(unacceptable),
                Predicate::Whitelist(unacceptable2),
            ],
        };

        assert!(!p.accept(&context))
    }
}
