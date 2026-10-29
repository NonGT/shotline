use jiff::Timestamp;
use jiff::tz::TimeZone;
use shotline_protocol::Quote;

use crate::budget::{Scope, check, exposure};
use crate::{Budgets, Entry, Error, Kind, Ledger, Rates, Result};

/// What a job is, for the ledger lines about it.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub job: String,
    pub take: Option<String>,
    pub shot: Option<String>,
    pub chapter: Option<u32>,
    pub op: String,
    pub plugin: String,
    pub model: String,
}

/// The user's cross-project ledger and its daily limit, in its own currency.
#[derive(Debug, Clone)]
pub struct GlobalLimit {
    pub ledger: Ledger,
    pub currency: String,
    pub per_day: Option<f64>,
}

/// A film's ledger with everything needed to write to it: currency, rates, budgets.
#[derive(Debug, Clone)]
pub struct Books {
    pub ledger: Ledger,
    pub currency: String,
    pub rates: Rates,
    pub budgets: Budgets,
    /// For deciding which day a line falls on.
    pub tz: TimeZone,
    /// Name used for this film in the global ledger.
    pub project: String,
    pub global: Option<GlobalLimit>,
}

fn line(now: Timestamp, kind: Kind, from: &Entry) -> Entry {
    Entry {
        seq: 0,
        time: now,
        kind,
        note: None,
        prev: String::new(),
        ..from.clone()
    }
}

impl Books {
    fn entry(&self, now: Timestamp, kind: Kind, ctx: &Context, q: &Quote) -> Result<Entry> {
        let c = self.rates.convert(q.amount, &q.unit, &self.currency)?;
        Ok(Entry {
            seq: 0,
            time: now,
            kind,
            project: None,
            job: Some(ctx.job.clone()).filter(|j| !j.is_empty()),
            take: ctx.take.clone(),
            shot: ctx.shot.clone(),
            chapter: ctx.chapter,
            op: Some(ctx.op.clone()).filter(|s| !s.is_empty()),
            plugin: Some(ctx.plugin.clone()).filter(|s| !s.is_empty()),
            model: Some(ctx.model.clone()).filter(|s| !s.is_empty()),
            plugin_job: None,
            units: q.units,
            unit_name: q.unit_name.clone(),
            amount: q.amount,
            unit: q.unit.clone(),
            base: c.amount,
            base_currency: self.currency.clone(),
            rate: c.rate,
            rate_source: c.source,
            note: None,
            prev: String::new(),
        })
    }

    /// The same line for the global ledger, converted to its currency.
    fn mirror(&self, g: &GlobalLimit, e: &Entry) -> Result<Entry> {
        let c = self.rates.convert(e.amount, &e.unit, &g.currency)?;
        Ok(Entry {
            project: Some(self.project.clone()),
            base: c.amount,
            base_currency: g.currency.clone(),
            rate: c.rate,
            rate_source: c.source,
            ..e.clone()
        })
    }

    fn same_day(&self, now: Timestamp) -> impl Fn(&Entry) -> bool + '_ {
        let today = now.to_zoned(self.tz.clone()).date();
        move |e: &Entry| e.time.to_zoned(self.tz.clone()).date() == today
    }

    /// Writes `e` to this film's ledger (and the global one), after `checks` passes on the
    /// current entries.
    fn write(
        &self,
        e: Entry,
        checks: impl FnOnce(&[Entry]) -> Result<()>,
        global_check: bool,
    ) -> Result<Entry> {
        let (mut written, ()) = self.ledger.transact(|entries| {
            checks(entries)?;
            if let Some(g) = &self.global {
                let m = self.mirror(g, &e)?;
                g.ledger.transact(|ges| {
                    if global_check {
                        let current = exposure(ges, self.same_day(e.time));
                        check(
                            m.base,
                            &g.currency,
                            &[Scope {
                                name: "daily (all films)",
                                limit: g.per_day,
                                current,
                            }],
                        )?;
                    }
                    Ok((vec![m], ()))
                })?;
            }
            Ok((vec![e], ()))
        })?;
        Ok(written.remove(0))
    }

    /// Holds the quoted amount for a job. Fails if any budget would be passed or the quote
    /// can't be converted to the film's currency.
    pub fn reserve(&self, now: Timestamp, ctx: &Context, quote: &Quote) -> Result<Entry> {
        if ctx.job.is_empty() {
            return Err(Error::Invalid("a reservation needs a job id".into()));
        }
        let e = self.entry(now, Kind::Reserve, ctx, quote)?;
        let amount = e.base;
        let b = self.budgets.clone();
        self.write(
            e,
            |entries| {
                if open_reserve(entries, &ctx.job).is_some() {
                    return Err(Error::Invalid(format!(
                        "job {} is already reserved",
                        ctx.job
                    )));
                }
                let take = |x: &Entry| ctx.take.is_some() && x.take == ctx.take;
                let chapter = |x: &Entry| ctx.chapter.is_some() && x.chapter == ctx.chapter;
                let scopes = [
                    Scope {
                        name: "per-job",
                        limit: b.per_job,
                        current: 0.0,
                    },
                    Scope {
                        name: "per-take",
                        limit: b.per_take.filter(|_| ctx.take.is_some()),
                        current: exposure(entries, take),
                    },
                    Scope {
                        name: "per-chapter",
                        limit: b.per_chapter.filter(|_| ctx.chapter.is_some()),
                        current: exposure(entries, chapter),
                    },
                    Scope {
                        name: "daily",
                        limit: b.per_day,
                        current: exposure(entries, self.same_day(now)),
                    },
                    Scope {
                        name: "total",
                        limit: b.total,
                        current: exposure(entries, |_| true),
                    },
                ];
                check(amount, &self.currency, &scopes)
            },
            true,
        )
    }

    /// Books what a job cost and closes its reservation. Without `actual` the reserved amount is
    /// used. Never refused for budget reasons: the money is already gone.
    pub fn charge(
        &self,
        now: Timestamp,
        job: &str,
        plugin_job: Option<&str>,
        actual: Option<&Quote>,
    ) -> Result<Entry> {
        let reserve = self.reservation(job)?;
        let mut e = line(now, Kind::Charge, &reserve);
        e.plugin_job = plugin_job.map(str::to_string);
        if let Some(q) = actual {
            e.amount = q.amount;
            e.unit = q.unit.clone();
            e.units = q.units.or(e.units);
            e.unit_name = q.unit_name.clone().or(e.unit_name);
        }
        // At the rate of the day it was charged, not when it was reserved.
        let c = self.rates.convert(e.amount, &e.unit, &self.currency)?;
        e.base = c.amount;
        e.rate = c.rate;
        e.rate_source = c.source;
        self.write(e, |entries| still_open(entries, job), false)
    }

    /// Closes a reservation for a job that cost nothing.
    pub fn release(&self, now: Timestamp, job: &str) -> Result<Entry> {
        let reserve = self.reservation(job)?;
        let mut e = line(now, Kind::Release, &reserve);
        e.amount = 0.0;
        e.base = 0.0;
        self.write(e, |entries| still_open(entries, job), false)
    }

    /// Money back for a charged job.
    pub fn refund(&self, now: Timestamp, job: &str, q: &Quote, note: &str) -> Result<Entry> {
        let entries = self.ledger.entries()?;
        let Some(charge) = entries
            .iter()
            .rev()
            .find(|e| e.kind == Kind::Charge && e.job.as_deref() == Some(job))
        else {
            return Err(Error::Invalid(format!("job {job} was never charged")));
        };
        let c = self.rates.convert(q.amount, &q.unit, &self.currency)?;
        let mut e = line(now, Kind::Refund, charge);
        e.amount = q.amount.abs();
        e.unit = q.unit.clone();
        e.units = None;
        e.unit_name = None;
        e.base = c.amount.abs();
        e.rate = c.rate;
        e.rate_source = c.source;
        e.note = Some(note.to_string()).filter(|n| !n.is_empty());
        self.write(e, |_| Ok(()), false)
    }

    /// Any other correction. Positive costs more, negative less.
    pub fn adjust(&self, now: Timestamp, q: &Quote, note: &str) -> Result<Entry> {
        if note.trim().is_empty() {
            return Err(Error::Invalid(
                "an adjustment needs a note saying why".into(),
            ));
        }
        let mut e = self.entry(now, Kind::Adjust, &Context::default(), q)?;
        e.note = Some(note.to_string());
        self.write(e, |_| Ok(()), false)
    }

    fn reservation(&self, job: &str) -> Result<Entry> {
        open_reserve(&self.ledger.entries()?, job)
            .cloned()
            .ok_or_else(|| Error::Invalid(format!("job {job} has no open reservation")))
    }
}

fn open_reserve<'a>(entries: &'a [Entry], job: &str) -> Option<&'a Entry> {
    let closed = entries
        .iter()
        .any(|e| matches!(e.kind, Kind::Charge | Kind::Release) && e.job.as_deref() == Some(job));
    if closed {
        return None;
    }
    entries
        .iter()
        .find(|e| e.kind == Kind::Reserve && e.job.as_deref() == Some(job))
}

/// Guards against two processes closing the same reservation.
fn still_open(entries: &[Entry], job: &str) -> Result<()> {
    match open_reserve(entries, job) {
        Some(_) => Ok(()),
        None => Err(Error::Invalid(format!("job {job} has no open reservation"))),
    }
}
