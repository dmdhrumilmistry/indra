//! Workflow schedules: standard five-field cron (`minute hour day month
//! weekday`), in the hub's local time.
//!
//! Each field takes `*`, a number, a range `a-b`, a list `a,b,c`, and a step
//! `*/n` or `a-b/n`. Weekdays are 0-6 from Sunday (7 is Sunday too); months
//! and weekdays also take names (`jan`, `mon`). As in cron, when both the day
//! of the month and the weekday are restricted, either one matching is
//! enough. Shortcuts: `@hourly`, `@daily`, `@weekly`, `@monthly`, `@yearly`.

use std::collections::BTreeSet;

/// A parsed schedule.
#[derive(Debug, Clone, PartialEq)]
pub struct Cron {
    minute: BTreeSet<u32>,
    hour: BTreeSet<u32>,
    day: BTreeSet<u32>,
    month: BTreeSet<u32>,
    weekday: BTreeSet<u32>,
    day_any: bool,
    weekday_any: bool,
}

/// A local date and time, as far as a schedule cares.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct When {
    pub minute: u32,
    pub hour: u32,
    pub day: u32,
    pub month: u32,
    /// 0 = Sunday.
    pub weekday: u32,
}

const MONTHS: [&str; 12] = [
    "jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec",
];
const DAYS: [&str; 7] = ["sun", "mon", "tue", "wed", "thu", "fri", "sat"];

fn field(
    s: &str,
    lo: u32,
    hi: u32,
    names: &[&str],
    what: &str,
) -> Result<(BTreeSet<u32>, bool), String> {
    let mut out = BTreeSet::new();
    let any = s == "*";
    let num = |x: &str| -> Result<u32, String> {
        let x = x.to_ascii_lowercase();
        if let Some(i) = names.iter().position(|n| *n == x) {
            return Ok(i as u32 + if lo == 1 { 1 } else { 0 });
        }
        x.parse::<u32>()
            .map_err(|_| format!("{}: '{}' is not a number", what, x))
    };
    for part in s.split(',') {
        let (range, step) = match part.split_once('/') {
            Some((r, st)) => (
                r,
                st.parse::<u32>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| format!("{}: bad step '{}'", what, st))?,
            ),
            None => (part, 1),
        };
        let (a, b) = if range == "*" {
            (lo, hi)
        } else if let Some((a, b)) = range.split_once('-') {
            (num(a)?, num(b)?)
        } else {
            let v = num(range)?;
            (v, if part.contains('/') { hi } else { v })
        };
        // Sunday may be written as 7.
        let (a, b) = if what == "weekday" {
            (a.min(7), b.min(7))
        } else {
            (a, b)
        };
        let top = if what == "weekday" { 7 } else { hi };
        if a < lo || b > top || a > b {
            return Err(format!("{}: {} is outside {}-{}", what, range, lo, hi));
        }
        let mut v = a;
        while v <= b {
            out.insert(if what == "weekday" { v % 7 } else { v });
            v += step;
        }
    }
    if out.is_empty() {
        return Err(format!("{}: nothing matches", what));
    }
    Ok((out, any))
}

impl Cron {
    pub fn parse(spec: &str) -> Result<Cron, String> {
        let spec = spec.trim();
        let expanded = match spec {
            "@hourly" => "0 * * * *",
            "@daily" | "@midnight" => "0 0 * * *",
            "@weekly" => "0 0 * * 0",
            "@monthly" => "0 0 1 * *",
            "@yearly" | "@annually" => "0 0 1 1 *",
            s => s,
        };
        let f: Vec<&str> = expanded.split_whitespace().collect();
        if f.len() != 5 {
            return Err(format!(
                "a schedule has five fields (minute hour day month weekday), e.g. \"0 9 * * 1-5\"; got {:?}",
                spec
            ));
        }
        let (minute, _) = field(f[0], 0, 59, &[], "minute")?;
        let (hour, _) = field(f[1], 0, 23, &[], "hour")?;
        let (day, day_any) = field(f[2], 1, 31, &[], "day")?;
        let (month, _) = field(f[3], 1, 12, &MONTHS, "month")?;
        let (weekday, weekday_any) = field(f[4], 0, 6, &DAYS, "weekday")?;
        Ok(Cron {
            minute,
            hour,
            day,
            month,
            weekday,
            day_any,
            weekday_any,
        })
    }

    pub fn matches(&self, w: &When) -> bool {
        if !self.minute.contains(&w.minute)
            || !self.hour.contains(&w.hour)
            || !self.month.contains(&w.month)
        {
            return false;
        }
        let d = self.day.contains(&w.day);
        let wd = self.weekday.contains(&w.weekday);
        match (self.day_any, self.weekday_any) {
            (true, true) => true,
            (false, true) => d,
            (true, false) => wd,
            (false, false) => d || wd,
        }
    }
}

/// What a schedule means, in words, for the console.
pub fn describe(spec: &str) -> String {
    let s = spec.trim();
    let f: Vec<&str> = s.split_whitespace().collect();
    match s {
        "@hourly" => return "every hour".into(),
        "@daily" | "@midnight" => return "every day at 00:00".into(),
        "@weekly" => return "every Sunday at 00:00".into(),
        "@monthly" => return "on the 1st of every month at 00:00".into(),
        _ => {}
    }
    if f.len() == 5 {
        let at = |m: &str, h: &str| -> Option<String> {
            Some(format!(
                "{:02}:{:02}",
                h.parse::<u32>().ok()?,
                m.parse::<u32>().ok()?
            ))
        };
        match (f[0], f[1], f[2], f[3], f[4]) {
            (m, "*", "*", "*", "*") if m.parse::<u32>().is_ok() => {
                return format!("every hour at :{:02}", m.parse::<u32>().unwrap_or(0))
            }
            (m, "*", "*", "*", "*") if m.starts_with("*/") => {
                return format!("every {} minutes", &m[2..])
            }
            (m, h, "*", "*", "*") => {
                if let Some(t) = at(m, h) {
                    return format!("every day at {}", t);
                }
            }
            (m, h, "*", "*", "1-5") => {
                if let Some(t) = at(m, h) {
                    return format!("weekdays at {}", t);
                }
            }
            (m, h, "*", "*", d) if d.len() == 1 => {
                if let (Some(t), Ok(n)) = (at(m, h), d.parse::<usize>()) {
                    let names = [
                        "Sunday",
                        "Monday",
                        "Tuesday",
                        "Wednesday",
                        "Thursday",
                        "Friday",
                        "Saturday",
                        "Sunday",
                    ];
                    if let Some(name) = names.get(n) {
                        return format!("every {} at {}", name, t);
                    }
                }
            }
            _ => {}
        }
    }
    format!("on the schedule \"{}\"", s)
}

/// Now, in the hub's local time.
pub fn now_local() -> When {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    from_utc(secs + local_offset_secs(secs))
}

/// The local time zone's offset from UTC at `secs`.
#[cfg(unix)]
fn local_offset_secs(secs: i64) -> i64 {
    let t = secs as libc::time_t;
    // SAFETY: localtime_r writes into the tm we own; it is thread-safe.
    unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        if libc::localtime_r(&t, &mut tm).is_null() {
            return 0;
        }
        tm.tm_gmtoff
    }
}

#[cfg(windows)]
fn local_offset_secs(secs: i64) -> i64 {
    time::OffsetDateTime::from_unix_timestamp(secs)
        .ok()
        .and_then(|t| time::UtcOffset::local_offset_at(t).ok())
        .map(|o| o.whole_seconds() as i64)
        .unwrap_or(0)
}

#[cfg(not(any(unix, windows)))]
fn local_offset_secs(_secs: i64) -> i64 {
    0
}

/// A Unix time (already shifted to local) as a date and time.
pub fn from_utc(secs: i64) -> When {
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // 1970-01-01 was a Thursday.
    let weekday = (days + 4).rem_euclid(7) as u32;
    // Civil from days (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let _year = 400 * era + yoe + if month <= 2 { 1 } else { 0 };
    When {
        minute: (rem / 60 % 60) as u32,
        hour: (rem / 3600) as u32,
        day,
        month,
        weekday,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(minute: u32, hour: u32, day: u32, month: u32, weekday: u32) -> When {
        When {
            minute,
            hour,
            day,
            month,
            weekday,
        }
    }

    #[test]
    fn schedules_match_like_cron() {
        let c = Cron::parse("0 9 * * 1-5").unwrap();
        assert!(c.matches(&w(0, 9, 17, 11, 1)));
        assert!(!c.matches(&w(0, 9, 15, 11, 6)), "Saturday");
        assert!(!c.matches(&w(1, 9, 17, 11, 1)));
        let c = Cron::parse("*/15 * * * *").unwrap();
        assert!(c.matches(&w(45, 3, 1, 1, 0)) && !c.matches(&w(10, 3, 1, 1, 0)));
        let c = Cron::parse("30 2 1,15 * *").unwrap();
        assert!(c.matches(&w(30, 2, 15, 6, 3)) && !c.matches(&w(30, 2, 14, 6, 3)));
        // Day and weekday both set: either.
        let c = Cron::parse("0 0 13 * fri").unwrap();
        assert!(c.matches(&w(0, 0, 13, 2, 2)) && c.matches(&w(0, 0, 9, 2, 5)));
        assert!(
            Cron::parse("0 0 * * 7").unwrap().matches(&w(0, 0, 3, 3, 0)),
            "7 is Sunday"
        );
        assert!(Cron::parse("0 0 * jan-mar *")
            .unwrap()
            .matches(&w(0, 0, 3, 2, 0)));
        assert_eq!(
            Cron::parse("@daily").unwrap(),
            Cron::parse("0 0 * * *").unwrap()
        );
        for bad in [
            "",
            "* * * *",
            "61 * * * *",
            "* 24 * * *",
            "*/0 * * * *",
            "5-1 * * * *",
            "x * * * *",
        ] {
            assert!(Cron::parse(bad).is_err(), "{:?}", bad);
        }
    }

    #[test]
    fn dates_and_words() {
        // 2026-11-15 13:30 UTC was a Sunday.
        let t = from_utc(1_794_749_400);
        assert_eq!(t, w(30, 13, 15, 11, 0));
        assert_eq!(
            from_utc(951_782_400),
            w(0, 0, 29, 2, 2),
            "2000-02-29, a Tuesday"
        );
        assert_eq!(describe("0 9 * * 1-5"), "weekdays at 09:00");
        assert_eq!(describe("30 18 * * *"), "every day at 18:30");
        assert_eq!(describe("0 * * * *"), "every hour at :00");
        assert_eq!(describe("*/10 * * * *"), "every 10 minutes");
        assert_eq!(describe("0 8 * * 1"), "every Monday at 08:00");
        assert_eq!(describe("@hourly"), "every hour");
    }
}
