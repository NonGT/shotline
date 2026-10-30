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

/// `~/.config/shotline/measured-rates.toml`: what units turned out to cost, worked out from
/// provider balances read before and after jobs. Kept apart from the rates you set and the ones
/// providers publish, and only used for units neither has a rate for.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct MeasuredRates {
    pub rates: BTreeMap<String, Measured>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Measured {
    pub currency: String,
    /// Units used and what they cost over every sample, so the rate is the average.
    pub units: f64,
    pub cost: f64,
    pub samples: u32,
    pub updated: jiff::Timestamp,
}

impl Measured {
    pub fn rate(&self) -> f64 {
        self.cost / self.units
    }
}

impl MeasuredRates {
    fn path(user_dir: &Path) -> std::path::PathBuf {
        user_dir.join("measured-rates.toml")
    }

    pub fn load(user_dir: &Path) -> Result<MeasuredRates> {
        let path = Self::path(user_dir);
        if !path.exists() {
            return Ok(MeasuredRates::default());
        }
        fsx::read_toml(&path)
    }

    /// Adds a sample: `units` of `unit` cost `cost` in `currency`. A sample far off the rate so
    /// far is more likely another job or the website spending at the same time, so it's left out
    /// and `false` returned.
    pub fn add(
        user_dir: &Path,
        unit: &str,
        units: f64,
        cost: f64,
        currency: &str,
        now: jiff::Timestamp,
    ) -> Result<bool> {
        if !(units > 0.0 && cost > 0.0) {
            return Ok(false);
        }
        let mut all = Self::load(user_dir)?;
        match all.rates.get_mut(unit) {
            Some(m) if m.currency == currency => {
                let r = cost / units;
                if r > m.rate() * 2.0 || r < m.rate() / 2.0 {
                    return Ok(false);
                }
                m.units += units;
                m.cost += cost;
                m.samples += 1;
                m.updated = now;
            }
            _ => {
                all.rates.insert(
                    unit.to_string(),
                    Measured {
                        currency: currency.to_string(),
                        units,
                        cost,
                        samples: 1,
                        updated: now,
                    },
                );
            }
        }
        fsx::mkdir(user_dir)?;
        fsx::write_toml(&Self::path(user_dir), &all)?;
        Ok(true)
    }
}

fn ledger_err(e: shotline_ledger::Error) -> Error {
    Error::Invalid(e.to_string())
}

impl Project {
    pub fn ledger(&self) -> Ledger {
        Ledger::new(self.root.join(".shotline").join("ledger.jsonl"))
    }

    /// Measured rates, then plugins', then the user's config, then the film's; later ones win.
    pub fn rates(
        &self,
        user: &UserConfig,
        measured: &MeasuredRates,
        plugin_rates: &[Rate],
    ) -> Result<Rates> {
        let mut r = Rates::default();
        for (unit, m) in &measured.rates {
            r.add(unit, m.rate(), &m.currency, "measured");
        }
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
            rates: self.rates(user, &MeasuredRates::load(user_dir)?, plugin_rates)?,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn measured_rates_average_and_skip_outliers() {
        let dir = tempfile::tempdir().unwrap();
        let now: jiff::Timestamp = "2026-10-30T10:00:00Z".parse().unwrap();
        let add = |units, cost| MeasuredRates::add(dir.path(), "x-sec", units, cost, "USD", now);
        assert!(add(4.0, 0.12).unwrap());
        assert!(add(6.0, 0.20).unwrap());
        assert!(
            !add(4.0, 1.0).unwrap(),
            "0.25 a second is someone else spending"
        );
        assert!(!add(0.0, 1.0).unwrap());
        let m = &MeasuredRates::load(dir.path()).unwrap().rates["x-sec"];
        assert_eq!(m.samples, 2);
        assert!((m.rate() - 0.032).abs() < 1e-12);
    }
}
