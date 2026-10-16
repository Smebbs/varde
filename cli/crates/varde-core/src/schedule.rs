//! When a scheduled task is next due. A schedule is a rule, as typed,
//! read here into the first time it is due and, for one that repeats,
//! the time after each one it fired at. Pure functions over local time;
//! `varde tick` fires what is due.
//!
//! ```text
//! at [DATE|DAY|today|tomorrow] [HH:MM]     once
//! every [day|weekday|weekend|DAY,DAY…] [HH:MM]
//! every N(m|h|d|w)                         from when it last fired
//! <2026-10-19 Mon 09:00 ++1w>              an org timestamp, as org-varde passes it
//! ```
//!
//! A time left out is midnight: the first moment of that day the machine
//! is on. Missed slots coalesce: the next one is always after now.

use chrono::{Datelike, Duration, Local, Months, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Weekday};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rule {
    /// Once, at this time.
    At(NaiveDateTime),
    /// On these weekdays (bit 0 is Monday) at this time.
    Days { days: u8, time: NaiveTime },
    /// This long after it last fired.
    Interval(Step),
    /// An org repeater from `base`: `+` and `++` keep to the slots from
    /// the base, which coalescing makes the same; `.+` counts from when it
    /// last fired, at the base's time of day.
    Org { base: NaiveDateTime, step: Step, from_fired: bool },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub n: u32,
    pub unit: Unit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Minute,
    Hour,
    Day,
    Week,
    Month,
    Year,
}

impl Step {
    fn after(self, t: NaiveDateTime) -> Option<NaiveDateTime> {
        let n = self.n as i64;
        match self.unit {
            Unit::Minute => t.checked_add_signed(Duration::minutes(n)),
            Unit::Hour => t.checked_add_signed(Duration::hours(n)),
            Unit::Day => t.checked_add_signed(Duration::days(n)),
            Unit::Week => t.checked_add_signed(Duration::weeks(n)),
            Unit::Month => t.checked_add_months(Months::new(self.n)),
            Unit::Year => t.checked_add_months(Months::new(self.n.checked_mul(12)?)),
        }
    }
}

impl Rule {
    pub fn repeats(&self) -> bool {
        !matches!(self, Rule::At(_))
    }

    /// The first time it is due, from `now`. An `at` or org time already
    /// past is due at once.
    pub fn first(&self, now: NaiveDateTime) -> Option<NaiveDateTime> {
        match self {
            Rule::At(t) => Some(*t),
            Rule::Days { .. } => self.next(now, now),
            Rule::Interval(step) => step.after(now),
            Rule::Org { base, .. } => Some(*base),
        }
    }

    /// The time after `fired`, the one it fired at, that is after `now`
    /// too; `None` once it does not repeat.
    pub fn next(&self, fired: NaiveDateTime, now: NaiveDateTime) -> Option<NaiveDateTime> {
        let floor = fired.max(now);
        match *self {
            Rule::At(_) => None,
            Rule::Days { days, time } => (0..=7).find_map(|d| {
                let t = floor.date().checked_add_signed(Duration::days(d))?.and_time(time);
                (t > floor && days & bit(t.weekday()) != 0).then_some(t)
            }),
            Rule::Interval(step) => step.after(floor),
            Rule::Org { base, step, from_fired: true } => {
                let at = match step.unit {
                    Unit::Minute | Unit::Hour => floor,
                    _ => floor.date().and_time(base.time()),
                };
                let mut t = step.after(at)?;
                while t <= floor {
                    t = step.after(t)?;
                }
                Some(t)
            }
            Rule::Org { base, step, from_fired: false } => {
                // Each slot counted from the base, so a month from the 31st
                // does not drift to the 28th. A gap of fixed-length steps is
                // crossed in one jump.
                let mut k = match step.unit {
                    Unit::Month | Unit::Year => 0,
                    _ => {
                        let len = step.after(base)? - base;
                        (floor - base).num_seconds().max(0) / len.num_seconds().max(1)
                    }
                };
                loop {
                    let t = Step { n: step.n.checked_mul(u32::try_from(k).ok()?)?, unit: step.unit }
                        .after(base)?;
                    if t > floor {
                        return Some(t);
                    }
                    k += 1;
                }
            }
        }
    }
}

fn bit(d: Weekday) -> u8 {
    1 << d.num_days_from_monday()
}

const WEEKDAYS: u8 = 0b0011111;
const WEEKEND: u8 = 0b1100000;
const EVERY_DAY: u8 = 0b1111111;

/// Read a rule as typed. `now` places a relative one (`at mon`).
pub fn parse(text: &str, now: NaiveDateTime) -> Result<Rule, String> {
    let text = text.trim();
    if text.starts_with('<') || text.starts_with('[') {
        return org(text);
    }
    let lower = text.to_lowercase();
    let mut words = lower.split_whitespace();
    match words.next() {
        Some("at") => at(&words.collect::<Vec<_>>(), now),
        Some("every") => every(&words.collect::<Vec<_>>()),
        _ => Err(format!("{text:?}: a schedule starts with `at` or `every`, or is an org timestamp")),
    }
}

fn at(words: &[&str], now: NaiveDateTime) -> Result<Rule, String> {
    let (time, rest) = split_time(words)?;
    let day = match rest[..] {
        [] => None,
        [w] => Some(w),
        _ => return Err(format!("`at {}`: one day and a time, like `at mon 09:00`", words.join(" "))),
    };
    let today = now.date();
    let date = match day {
        None => {
            let t = time.ok_or("`at` needs a day or a time")?;
            if today.and_time(t) > now {
                today
            } else {
                today.succ_opt().ok_or("out of range")?
            }
        }
        Some("today") => today,
        Some("tomorrow") => today.succ_opt().ok_or("out of range")?,
        Some(w) => match NaiveDate::parse_from_str(w, "%Y-%m-%d") {
            Ok(d) => d,
            Err(_) => {
                let wd = weekday(w)
                    .ok_or_else(|| format!("{w:?} is not a day: mon…sun, today, tomorrow or 2026-10-19"))?;
                let t = time.unwrap_or(NaiveTime::MIN);
                (0..=7)
                    .map(|d| today + Duration::days(d))
                    .find(|d| d.weekday() == wd && d.and_time(t) > now)
                    .ok_or("out of range")?
            }
        },
    };
    Ok(Rule::At(date.and_time(time.unwrap_or(NaiveTime::MIN))))
}

fn every(words: &[&str]) -> Result<Rule, String> {
    if let [w] = words {
        if let Some(step) = step(w) {
            return Ok(Rule::Interval(step));
        }
    }
    let (time, rest) = split_time(words)?;
    let mut days = 0;
    for w in rest.iter().flat_map(|w| w.split(',')).filter(|w| !w.is_empty()) {
        days |= match w {
            "day" | "days" | "daily" => EVERY_DAY,
            "weekday" | "weekdays" => WEEKDAYS,
            "weekend" | "weekends" => WEEKEND,
            _ => bit(weekday(w).ok_or_else(|| {
                format!("{w:?}: `every` takes day, weekday, weekend, days like mon,thu, or 30m, 4h, 2d, 1w")
            })?),
        };
    }
    if days == 0 {
        return Err("`every` needs the days, like `every weekday 09:00`, or a length, like `every 4h`".into());
    }
    Ok(Rule::Days { days, time: time.unwrap_or(NaiveTime::MIN) })
}

/// The `HH:MM` among `words`, if any, and the words without it.
fn split_time<'a>(words: &[&'a str]) -> Result<(Option<NaiveTime>, Vec<&'a str>), String> {
    let mut time = None;
    let mut rest = Vec::new();
    for w in words {
        if w.contains(':') {
            if time.is_some() {
                return Err("one time only".into());
            }
            time = Some(
                NaiveTime::parse_from_str(w, "%H:%M")
                    .map_err(|_| format!("{w:?} is not a time like 09:00"))?,
            );
        } else {
            rest.push(*w);
        }
    }
    Ok((time, rest))
}

fn weekday(w: &str) -> Option<Weekday> {
    const NAMES: [(&str, Weekday); 7] = [
        ("monday", Weekday::Mon),
        ("tuesday", Weekday::Tue),
        ("wednesday", Weekday::Wed),
        ("thursday", Weekday::Thu),
        ("friday", Weekday::Fri),
        ("saturday", Weekday::Sat),
        ("sunday", Weekday::Sun),
    ];
    let w = w.to_lowercase();
    NAMES.iter().find(|(name, _)| w.len() >= 3 && name.starts_with(&w)).map(|(_, d)| *d)
}

/// `30m`, `4h`, `2d`, `1w`, and in org's repeaters `1m` is a month and
/// `1y` a year.
fn step(w: &str) -> Option<Step> {
    step_with(w, Unit::Minute)
}

fn step_with(w: &str, m: Unit) -> Option<Step> {
    let unit = match w.chars().last()? {
        'm' => m,
        'h' => Unit::Hour,
        'd' => Unit::Day,
        'w' => Unit::Week,
        'y' if m == Unit::Month => Unit::Year,
        _ => return None,
    };
    let n: u32 = w[..w.len() - 1].parse().ok().filter(|n| *n > 0)?;
    Some(Step { n, unit })
}

/// `<2026-10-19 Mon 09:00 ++1w>`: org's date, weekday name, time (a
/// range's start), repeater and warning period. Only the date is needed.
fn org(text: &str) -> Result<Rule, String> {
    let bad = || format!("{text:?} is not an org timestamp like <2026-10-19 Mon 09:00 ++1w>");
    let inner =
        text.get(1..text.len() - 1).filter(|_| text.ends_with('>') || text.ends_with(']')).ok_or_else(bad)?;
    let mut words = inner.split_whitespace();
    let date = NaiveDate::parse_from_str(words.next().ok_or_else(bad)?, "%Y-%m-%d").map_err(|_| bad())?;
    let mut time = NaiveTime::MIN;
    let mut repeat = None;
    for w in words {
        if let Some(r) = w.strip_prefix(".+") {
            repeat = Some((step_with(r, Unit::Month).ok_or_else(bad)?, true));
        } else if let Some(r) = w.strip_prefix("++").or_else(|| w.strip_prefix('+')) {
            repeat = Some((step_with(r, Unit::Month).ok_or_else(bad)?, false));
        } else if w.starts_with(|c: char| c.is_ascii_digit()) {
            let start = w.split('-').next().unwrap_or(w);
            time = NaiveTime::parse_from_str(start, "%H:%M").map_err(|_| bad())?;
        }
        // A weekday's name, and a warning period (`-2d`), say nothing more.
    }
    let base = date.and_time(time);
    Ok(match repeat {
        None => Rule::At(base),
        Some((step, from_fired)) => Rule::Org { base, step, from_fired },
    })
}

/// Unix ms of a local time. A time skipped by a DST change is the
/// first one after it.
pub fn to_ms(t: NaiveDateTime) -> i64 {
    (0..4)
        .find_map(|h| Local.from_local_datetime(&(t + Duration::hours(h))).earliest())
        .map_or_else(|| t.and_utc().timestamp_millis(), |t| t.timestamp_millis())
}

pub fn from_ms(ms: i64) -> NaiveDateTime {
    Local.timestamp_millis_opt(ms).single().map(|t| t.naive_local()).unwrap_or_default()
}

/// When it is next due, short, for a board row: `09:00` today, `Mon
/// 09:00` this week, `Oct 21 09:00` after; a midnight shows no time.
pub fn when(due: NaiveDateTime, now: NaiveDateTime) -> String {
    let time = if due.time() == NaiveTime::MIN { String::new() } else { due.format(" %H:%M").to_string() };
    let days = (due.date() - now.date()).num_days();
    if days == 0 && !time.is_empty() {
        time.trim().to_string()
    } else if days == 0 {
        "today".into()
    } else if (1..7).contains(&days) || (days < 0 && days > -7) {
        format!("{}{time}", due.format("%a"))
    } else {
        format!("{}{time}", due.format("%b %-d"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> NaiveDateTime {
        NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M").unwrap()
    }

    // 2026-10-14 is a Wednesday.
    const NOW: &str = "2026-10-14 10:30";

    fn first(rule: &str) -> NaiveDateTime {
        let now = t(NOW);
        parse(rule, now).unwrap().first(now).unwrap()
    }

    #[test]
    fn at_finds_the_next_such_time() {
        assert_eq!(first("at 11:00"), t("2026-10-14 11:00"));
        assert_eq!(first("at 09:00"), t("2026-10-15 09:00"), "past today: tomorrow");
        assert_eq!(first("at mon 09:00"), t("2026-10-19 09:00"));
        assert_eq!(first("at Monday"), t("2026-10-19 00:00"));
        assert_eq!(first("at wed 09:00"), t("2026-10-21 09:00"), "today's has passed");
        assert_eq!(first("at wed 11:00"), t("2026-10-14 11:00"));
        assert_eq!(first("at tomorrow 08:15"), t("2026-10-15 08:15"));
        assert_eq!(first("at 2026-11-02 07:00"), t("2026-11-02 07:00"));
        assert!(!parse("at mon", t(NOW)).unwrap().repeats());
    }

    #[test]
    fn every_day_rules_come_round() {
        let now = t(NOW);
        assert_eq!(first("every day 06:00"), t("2026-10-15 06:00"));
        assert_eq!(first("every day 11:00"), t("2026-10-14 11:00"));
        assert_eq!(first("every weekday 09:00"), t("2026-10-15 09:00"));
        assert_eq!(first("every mon,thu 09:00"), t("2026-10-15 09:00"));
        assert_eq!(first("every mon thu"), t("2026-10-15 00:00"));
        assert_eq!(first("every weekend"), t("2026-10-17 00:00"));
        let fri = parse("every weekday 09:00", now).unwrap();
        assert_eq!(fri.next(t("2026-10-16 09:00"), t("2026-10-16 09:30")), Some(t("2026-10-19 09:00")));
    }

    #[test]
    fn missed_slots_coalesce_into_one() {
        let now = t(NOW);
        let daily = parse("every day 06:00", now).unwrap();
        // Due Friday, the lid opened Monday 09:30: it fires then, once, and
        // next on Tuesday.
        assert_eq!(daily.next(t("2026-10-16 06:00"), t("2026-10-19 09:30")), Some(t("2026-10-20 06:00")));
        let org = parse("<2026-10-01 Thu 09:00 +1d>", now).unwrap();
        assert_eq!(org.first(now), Some(t("2026-10-01 09:00")), "overdue fires at once");
        assert_eq!(org.next(t("2026-10-01 09:00"), now), Some(t("2026-10-15 09:00")));
        let hourly = parse("<2026-01-01 Thu 00:00 ++1h>", now).unwrap();
        assert_eq!(hourly.next(t("2026-01-01 00:00"), now), Some(t("2026-10-14 11:00")));
    }

    #[test]
    fn an_interval_counts_from_when_it_fired() {
        let now = t(NOW);
        let r = parse("every 4h", now).unwrap();
        assert_eq!(r.first(now), Some(t("2026-10-14 14:30")));
        assert_eq!(r.next(t("2026-10-14 14:31"), t("2026-10-14 14:31")), Some(t("2026-10-14 18:31")));
        assert_eq!(parse("every 30m", now).unwrap(), Rule::Interval(Step { n: 30, unit: Unit::Minute }));
    }

    #[test]
    fn org_timestamps_are_read_as_org_writes_them() {
        let now = t(NOW);
        assert_eq!(parse("<2026-10-19 Mon 09:00>", now).unwrap(), Rule::At(t("2026-10-19 09:00")));
        assert_eq!(parse("<2026-10-19 Mon>", now).unwrap(), Rule::At(t("2026-10-19 00:00")));
        assert_eq!(parse("[2026-10-19 Mon 09:00-10:00 -2d]", now).unwrap(), Rule::At(t("2026-10-19 09:00")));
        let weekly = parse("<2026-10-19 Mon 09:00 ++1w>", now).unwrap();
        assert_eq!(weekly.first(now), Some(t("2026-10-19 09:00")));
        assert_eq!(weekly.next(t("2026-10-19 09:00"), t("2026-10-19 09:30")), Some(t("2026-10-26 09:00")));
        let monthly = parse("<2026-01-31 Sat 08:00 +1m>", now).unwrap();
        assert_eq!(monthly.next(t("2026-01-31 08:00"), now), Some(t("2026-10-31 08:00")));
        let dot = parse("<2026-10-01 Thu 09:00 .+2d>", now).unwrap();
        assert_eq!(dot.next(t("2026-10-14 10:30"), now), Some(t("2026-10-16 09:00")));
        let yearly = parse("<2026-03-01 Sun +1y>", now).unwrap();
        assert_eq!(yearly.next(t("2026-03-01 00:00"), now), Some(t("2027-03-01 00:00")));
    }

    #[test]
    fn what_is_not_a_schedule_says_why() {
        let now = t(NOW);
        for bad in
            ["tomorrow", "at", "at blursday", "every", "every 0h", "every fortnight", "at 25:00", "<soon>"]
        {
            assert!(parse(bad, now).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_row_says_when_briefly() {
        let now = t(NOW);
        assert_eq!(when(t("2026-10-14 11:00"), now), "11:00");
        assert_eq!(when(t("2026-10-15 00:00"), now), "Thu");
        assert_eq!(when(t("2026-10-19 09:00"), now), "Mon 09:00");
        assert_eq!(when(t("2026-11-02 09:00"), now), "Nov 2 09:00");
        assert_eq!(when(t("2026-10-14 00:00"), now), "today");
    }
}
