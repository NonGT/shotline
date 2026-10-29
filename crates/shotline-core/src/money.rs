use std::collections::{BTreeMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use shotline_ledger::{Books, GlobalLimit, Ledger, Rates};
use shotline_protocol::Rate;

use crate::{Error, Money, Project, Result, fsx};

/// `~/.config/shotline/config.toml`: what applies across all films.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserConfig {
    pub money: Money,
    pub budget: UserBudget,
    pub rates: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct UserBudget {
    /// Across all films, in the user's currency.
    pub per_day: Option<f64>,
}

impl UserConfig {
    pub fn load(user_dir: &Path) -> Result<UserConfig> {
        let path = user_dir.join("config.toml");
        if !path.exists() {
            return Ok(UserConfig::default());
        }
        fsx::read_toml(&path)
    }
}

fn ledger_err(e: shotline_ledger::Error) -> Error {
    Error::Invalid(e.to_string())
}

impl Project {
    pub fn ledger(&self) -> Ledger {
        Ledger::new(self.root.join(".shotline").join("ledger.jsonl"))
    }

    /// Rates from plugins, then the user's config, then the film's; later ones win.
    pub fn rates(&self, user: &UserConfig, plugin_rates: &[Rate]) -> Result<Rates> {
        let mut r = Rates::default();
        for x in plugin_rates {
            r.add(&x.unit, x.amount, &x.currency, "plugin");
        }
        r.add_table(&user.rates, "config").map_err(ledger_err)?;
        r.add_table(&self.config.rates, "config")
            .map_err(ledger_err)?;
        Ok(r)
    }

    /// The film's ledger with its currency, rates and budgets, tied to the user's cross-film
    /// ledger in `user_dir`.
    pub fn books(
        &self,
        user_dir: &Path,
        user: &UserConfig,
        plugin_rates: &[Rate],
    ) -> Result<Books> {
        Ok(Books {
            ledger: self.ledger(),
            currency: self.config.money.currency.clone(),
            rates: self.rates(user, plugin_rates)?,
            budgets: self.config.budget.clone(),
            tz: jiff::tz::TimeZone::system(),
            project: self.root.display().to_string(),
            global: Some(GlobalLimit {
                ledger: Ledger::new(user_dir.join("spend.jsonl")),
                currency: user.money.currency.clone(),
                per_day: user.budget.per_day,
            }),
        })
    }

    /// Ids of every selected take, i.e. what's in the cut.
    pub fn selected_take_ids(&self) -> Result<HashSet<String>> {
        Ok(self
            .shots(None)?
            .into_iter()
            .filter_map(|(_, s)| s.selected_take)
            .collect())
    }

    /// Length of the cut so far: shots with a selected take, at their planned duration.
    pub fn cut_seconds(&self) -> Result<f64> {
        Ok(self
            .shots(None)?
            .into_iter()
            .filter(|(_, s)| s.selected_take.is_some())
            .map(|(_, s)| s.duration.unwrap_or(self.config.duration))
            .sum())
    }
}
