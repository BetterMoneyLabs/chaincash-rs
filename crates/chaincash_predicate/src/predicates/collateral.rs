use crate::context::{ContextProvider, PredicateContext};
use crate::predicates::Accept;
use serde::{Deserialize, Serialize};

#[derive(Deserialize, Serialize, Debug, Clone, Default)]
#[serde(rename_all = "snake_case")]
pub enum CollateralAlgorithm {
    #[default]
    Initial,
}

impl CollateralAlgorithm {
    fn initial<P: ContextProvider>(&self, percent: u16, context: &PredicateContext<P>) -> bool {
        let issuer_note_tally: u64 = context
            .provider
            .agent_issued_notes(&context.note.issuer)
            .iter()
            .map(|n| n.nanoerg)
            .sum();
        let issuer_reserves = context
            .provider
            .agent_reserves_nanoerg(&context.note.issuer);
        let issuer_collateral = (issuer_reserves as f64 / issuer_note_tally as f64) * 100.0;

        if issuer_collateral >= percent as f64 {
            return true;
        }

        for signer in context.note.signers.iter().skip(1) {
            let signer_notes = context.provider.agent_issued_notes(signer);
            let highest_value_note = signer_notes.iter().max_by_key(|n| n.nanoerg);

            if let Some(signer_note) = highest_value_note {
                let signer_reserves = context.provider.agent_reserves_nanoerg(signer);
                let signer_collateral =
                    (signer_reserves as f64 / signer_note.nanoerg as f64) * 100.0;

                if signer_collateral >= percent as f64 {
                    return true;
                }
            }
        }

        false
    }

    pub fn eval<P: ContextProvider>(&self, percent: u16, context: &PredicateContext<P>) -> bool {
        match self {
            CollateralAlgorithm::Initial => self.initial(percent, context),
        }
    }
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub struct Collateral {
    #[serde(default = "CollateralAlgorithm::default")]
    algorithm: CollateralAlgorithm,
    pub(crate) percent: u16,
}

impl Accept for Collateral {
    fn accept<P: ContextProvider>(&self, context: &PredicateContext<P>) -> bool {
        self.algorithm.eval(self.percent, context)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::{test_util::*, NoteContext};

    // * first, take value of notes issued by issuer of a note of interest is divided by its reserves, if collateralization is enough (e.g. 100%), finish
    #[test]
    fn test_initial_returns_true_if_collaterized_by_issuer() {
        let issuer_pk = "issuer1".to_owned();
        let note_of_interest = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner1".to_owned(),
            signers: vec![issuer_pk.clone()],
        };
        // issuer of note of interest
        // has 90% reserves of note of interest
        let issuer = TestAgent {
            pk: issuer_pk,
            issued_notes: vec![note_of_interest.clone()],
            reserves: 900,
        };
        let provider = TestContextProvider {
            agents: vec![issuer],
        };
        let context = PredicateContext {
            note: note_of_interest,
            provider,
        };
        // only require 86%
        let p = Collateral {
            percent: 86,
            algorithm: CollateralAlgorithm::Initial,
        };
        // acceptable
        assert!(p.accept(&context))
    }

    // * if not, take max of notes issued (not passed through) by second signer divided by its reserves, third etc, stop when signer with enough collateralization found
    #[test]
    fn test_initial_returns_true_if_collaterized_by_signer() {
        let issuer_pk = "issuer1".to_owned();
        let signer_pk = "signer2".to_owned();
        // ownership:
        // issuer1 -> signer2 -> owner5
        let note_of_interest = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner5".to_owned(),
            signers: vec![issuer_pk.clone(), signer_pk.clone()],
        };
        // issuer, only has 10% reserves for note of interest
        // which is not enough
        let issuer = TestAgent {
            pk: issuer_pk,
            issued_notes: vec![note_of_interest.clone()],
            reserves: 100,
        };
        let signer_note = NoteContext {
            nanoerg: 1000,
            issuer: signer_pk.clone(),
            owner: "owner5".to_owned(),
            signers: vec![signer_pk.clone()],
        };
        // has 1000 reserves, has one note worth 1000
        // thus has 100% collateral
        let signer = TestAgent {
            pk: signer_pk,
            issued_notes: vec![signer_note],
            reserves: 1000,
        };
        let provider = TestContextProvider {
            agents: vec![issuer, signer],
        };
        let context = PredicateContext {
            note: note_of_interest,
            provider,
        };
        // requires 100%
        let p = Collateral {
            percent: 100,
            algorithm: CollateralAlgorithm::Initial,
        };
        // acceptable
        assert!(p.accept(&context))
    }

    // * if not found in the whole signatures-chain, acceptance predicate returns false
    #[test]
    fn test_initial_returns_false_if_no_required_collateral() {
        let issuer_pk = "issuer1".to_owned();
        let signer_pk = "signer2".to_owned();
        // ownership:
        // issuer -> signer2 -> owner5
        let note_of_interest = NoteContext {
            nanoerg: 1000,
            issuer: issuer_pk.clone(),
            owner: "owner5".to_owned(),
            signers: vec![issuer_pk.clone(), signer_pk.clone()],
        };
        // issuer only has 10% collateral
        let issuer = TestAgent {
            pk: issuer_pk,
            issued_notes: vec![note_of_interest.clone()],
            reserves: 100,
        };
        let signer_note = NoteContext {
            nanoerg: 1000,
            issuer: signer_pk.clone(),
            owner: "owner5".to_owned(),
            signers: vec![signer_pk.clone()],
        };
        // this signer note is 100% collaterized, which meets the `percent` requirement
        // but it is not the highest valued note issued by signer so it is not considered.
        let signer_note2 = NoteContext {
            nanoerg: 800,
            issuer: signer_pk.clone(),
            owner: "owner5".to_owned(),
            signers: vec![signer_pk.clone()],
        };
        // signer has 800 reserves but their max value note is 1000 value
        // 80% collateral - we require 100%
        let signer = TestAgent {
            pk: signer_pk,
            issued_notes: vec![signer_note2, signer_note],
            reserves: 800,
        };
        let provider = TestContextProvider {
            agents: vec![issuer, signer],
        };
        let context = PredicateContext {
            note: note_of_interest,
            provider,
        };
        let p = Collateral {
            percent: 100,
            algorithm: CollateralAlgorithm::Initial,
        };
        // not acceptable, issuer and signer dont have 100% collateral
        assert!(!p.accept(&context))
    }
}
