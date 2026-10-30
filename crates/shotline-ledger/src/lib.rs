//! Money: an append-only, hash-chained ledger per film, rates between currencies and credits,
//! budgets, reports and reconciling against what providers say they charged.

mod books;
mod budget;
mod entry;
mod ledger;
mod rates;
pub mod reconcile;
pub mod report;

pub use books::{Books, Context, GlobalLimit};
pub use budget::Budgets;
pub use entry::{Entry, Kind, is_currency, pricing};
pub use ledger::{Ledger, Problem};
pub use rates::{Converted, Rates, parse_rate};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{}: {source}", path.display())]
    Io {
        path: std::path::PathBuf,
        source: std::io::Error,
    },
    #[error("ledger line {line}: {message}")]
    Corrupt { line: usize, message: String },
    #[error("{0}")]
    NoRate(String),
    #[error("{0}")]
    OverBudget(String),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Rounds for display and comparisons; amounts are floats in the ledger.
pub fn money(amount: f64) -> String {
    let s = format!("{amount:.4}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.into() }
}

/// Rates need more places than money: an ElevenLabs credit is 0.00016 USD.
pub fn rate(r: f64) -> String {
    let s = format!("{r:.8}");
    s.trim_end_matches('0').trim_end_matches('.').into()
}

#[cfg(test)]
mod tests {
    #[test]
    fn rate_formatting() {
        assert_eq!(super::rate(6.42 / 40000.0), "0.0001605");
        assert_eq!(super::rate(0.03), "0.03");
    }

    #[test]
    fn money_formatting() {
        assert_eq!(super::money(0.4), "0.4");
        assert_eq!(super::money(1.0), "1");
        assert_eq!(super::money(0.1 + 0.2), "0.3");
        assert_eq!(super::money(12.34567), "12.3457");
        assert_eq!(super::money(-0.00001), "0");
    }
}
