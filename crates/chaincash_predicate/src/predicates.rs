use crate::context::{ContextProvider, PredicateContext};
use crate::Error;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub mod blacklist;
pub mod collateral;
pub mod or;
pub mod whitelist;

pub trait Accept {
    fn accept<P: ContextProvider>(&self, context: &PredicateContext<P>) -> bool;
}

#[derive(Deserialize, Serialize, Debug, Clone, utoipa::ToSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Predicate {
    Or(or::Or),
    Whitelist(whitelist::Whitelist),
    Blacklist(blacklist::Blacklist),
    Collateral(collateral::Collateral),
}

impl Predicate {
    pub fn from_file(path: &PathBuf) -> Result<Self, Error> {
        let s = std::fs::read_to_string(path).map_err(|e| Error::LoadFromFile {
            source: e,
            path: path.display().to_string(),
        })?;

        Ok(toml::from_str(&s)?)
    }
}

impl Accept for Predicate {
    fn accept<P: ContextProvider>(&self, context: &PredicateContext<P>) -> bool {
        match self {
            Predicate::Or(p) => p.accept(context),
            Predicate::Whitelist(p) => p.accept(context),
            Predicate::Blacklist(p) => p.accept(context),
            Predicate::Collateral(p) => p.accept(context),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_predicate_deser() {
        let s = r#"
            type = "or"
            conditions = [
                {type = "whitelist", kind = "owner", agents = ["PK1", "PK2"]},
                {type = "collateral", percent = 110}
            ]
            "#;
        assert!(toml::from_str::<Predicate>(s).is_ok())
    }
}
