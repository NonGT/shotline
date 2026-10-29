use std::collections::{BTreeMap, VecDeque};

use crate::{Error, Result};

/// What one unit of something is worth in another: `venice-credit = 0.004 USD`, `USD = 36.5 THB`.
/// Rates chain both ways, so with those two a credit converts to THB and THB to USD.
#[derive(Debug, Clone, Default)]
pub struct Rates {
    /// unit -> (amount, currency, source)
    edges: BTreeMap<String, (f64, String, String)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Converted {
    pub amount: f64,
    /// Effective rate, `amount / original`.
    pub rate: f64,
    /// Where the rates used came from: `same`, `config`, `plugin`, or several joined by `+`.
    pub source: String,
}

/// Parses `"0.004 USD"`.
pub fn parse_rate(s: &str) -> Result<(f64, String)> {
    let bad = || {
        Error::Invalid(format!(
            "bad rate {s:?}, expected something like \"0.004 USD\""
        ))
    };
    let (n, cur) = s.trim().split_once(char::is_whitespace).ok_or_else(bad)?;
    let n: f64 = n.parse().map_err(|_| bad())?;
    let cur = cur.trim();
    if !(n.is_finite() && n > 0.0) || cur.is_empty() {
        return Err(bad());
    }
    Ok((n, cur.to_string()))
}

impl Rates {
    /// Adds rates, replacing any for the same unit. Later calls win, so add plugin rates first
    /// and the user's own last.
    pub fn add(&mut self, unit: &str, amount: f64, currency: &str, source: &str) {
        self.edges.insert(
            unit.to_string(),
            (amount, currency.to_string(), source.to_string()),
        );
    }

    /// From a config table of `unit = "amount CURRENCY"`.
    pub fn add_table<'a>(
        &mut self,
        table: impl IntoIterator<Item = (&'a String, &'a String)>,
        source: &str,
    ) -> Result<()> {
        for (unit, v) in table {
            let (n, cur) =
                parse_rate(v).map_err(|e| Error::Invalid(format!("rates.{unit}: {e}")))?;
            self.add(unit, n, &cur, source);
        }
        Ok(())
    }

    pub fn convert(&self, amount: f64, from: &str, to: &str) -> Result<Converted> {
        if from == to {
            return Ok(Converted {
                amount,
                rate: 1.0,
                source: "same".into(),
            });
        }
        // Breadth-first over the rate graph, both directions.
        let mut seen = vec![from.to_string()];
        let mut queue = VecDeque::from([(from.to_string(), 1.0, Vec::<String>::new())]);
        while let Some((unit, factor, sources)) = queue.pop_front() {
            let mut next = vec![];
            if let Some((n, cur, src)) = self.edges.get(&unit) {
                next.push((cur.clone(), factor * n, src.clone()));
            }
            for (u, (n, cur, src)) in &self.edges {
                if *cur == unit {
                    next.push((u.clone(), factor / n, src.clone()));
                }
            }
            for (u, f, src) in next {
                if seen.contains(&u) {
                    continue;
                }
                let mut s = sources.clone();
                if !s.contains(&src) {
                    s.push(src);
                }
                if u == to {
                    return Ok(Converted {
                        amount: amount * f,
                        rate: f,
                        source: s.join("+"),
                    });
                }
                seen.push(u.clone());
                queue.push_back((u, f, s));
            }
        }
        Err(Error::NoRate(format!(
            "no rate from {from} to {to}; add one under [rates], e.g. {from} = \"0.01 {to}\""
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rates() -> Rates {
        let mut r = Rates::default();
        r.add("venice-credit", 0.004, "USD", "plugin");
        r.add("USD", 36.5, "THB", "config");
        r
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn same_unit() {
        let c = rates().convert(2.0, "USD", "USD").unwrap();
        assert_eq!((c.amount, c.rate, c.source.as_str()), (2.0, 1.0, "same"));
    }

    #[test]
    fn direct_and_reverse() {
        let r = rates();
        assert!(close(
            r.convert(100.0, "venice-credit", "USD").unwrap().amount,
            0.4
        ));
        assert!(close(r.convert(73.0, "THB", "USD").unwrap().amount, 2.0));
    }

    #[test]
    fn chains() {
        let c = rates().convert(100.0, "venice-credit", "THB").unwrap();
        assert!(close(c.amount, 14.6));
        assert!(close(c.rate, 0.146));
        assert_eq!(c.source, "plugin+config");
    }

    #[test]
    fn later_rates_win() {
        let mut r = rates();
        r.add("venice-credit", 0.003, "USD", "config");
        let c = r.convert(100.0, "venice-credit", "USD").unwrap();
        assert!(close(c.amount, 0.3));
        assert_eq!(c.source, "config");
    }

    #[test]
    fn missing_rate() {
        let e = rates()
            .convert(1.0, "fal-credit", "USD")
            .unwrap_err()
            .to_string();
        assert!(e.contains("no rate from fal-credit to USD"), "{e}");
    }

    #[test]
    fn parses_rates() {
        assert_eq!(parse_rate(" 0.004  USD ").unwrap(), (0.004, "USD".into()));
        assert!(parse_rate("USD").is_err());
        assert!(parse_rate("-1 USD").is_err());
        assert!(parse_rate("1").is_err());
    }
}
