use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// Money held for a job that's about to be sent.
    Reserve,
    /// What a job actually cost. Closes its reservation.
    Charge,
    /// A job that cost nothing after all. Closes its reservation.
    Release,
    /// Money back for a job, as a positive amount.
    Refund,
    /// Any other correction, signed.
    Adjust,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.pad(match self {
            Kind::Reserve => "reserve",
            Kind::Charge => "charge",
            Kind::Release => "release",
            Kind::Refund => "refund",
            Kind::Adjust => "adjust",
        })
    }
}

/// One line of the ledger. Ids are plain strings so the ledger doesn't depend on the project
/// model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub seq: u64,
    pub time: jiff::Timestamp,
    #[serde(rename = "type")]
    pub kind: Kind,
    /// Only in the cross-project ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub take: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shot: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chapter: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub op: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// The plugin's own job id, to match against provider usage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_job: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unit_name: Option<String>,
    /// In the provider's unit.
    pub amount: f64,
    pub unit: String,
    /// In the film's currency, at the rate of the day.
    pub base: f64,
    pub base_currency: String,
    pub rate: f64,
    pub rate_source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// sha256 of the previous line, empty for the first.
    pub prev: String,
}

impl Entry {
    /// Effect on money spent, in the base currency. Reservations don't count here.
    pub fn spent(&self) -> f64 {
        match self.kind {
            Kind::Charge | Kind::Adjust => self.base,
            Kind::Refund => -self.base,
            Kind::Reserve | Kind::Release => 0.0,
        }
    }

    /// Same, in the provider's unit.
    pub fn spent_original(&self) -> f64 {
        match self.kind {
            Kind::Charge | Kind::Adjust => self.amount,
            Kind::Refund => -self.amount,
            Kind::Reserve | Kind::Release => 0.0,
        }
    }

    /// Key for matching a reservation to what closes it.
    pub(crate) fn job_key(&self) -> Option<(Option<&str>, &str)> {
        Some((self.project.as_deref(), self.job.as_deref()?))
    }
}
