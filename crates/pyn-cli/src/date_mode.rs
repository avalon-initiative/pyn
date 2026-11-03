//! Git's `--date=<mode>` for the commands that take every mode: `human`, the `-local` forms, `format:` and `auto:`.

use anyhow::{Result, bail};
use chrono::{DateTime, Datelike, FixedOffset, Local, TimeZone, Timelike};
use pyn_object::Signature;

use crate::commit_format::{DateStyle, relative};

#[derive(Clone, Debug, PartialEq, Eq)]
enum Kind {
    Plain(DateStyle),
    Human,
    Strftime(String),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DateMode {
    kind: Kind,
    local: bool,
}

const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Offset in the `[-+]HHMM` decimal form git passes around.
fn hhmm(offset_minutes: i32) -> i32 {
    let abs = offset_minutes.abs();
    offset_minutes.signum() * (abs / 60 * 100 + abs % 60)
}

fn at(secs: i64, offset_minutes: i32) -> DateTime<FixedOffset> {
    let zone = FixedOffset::east_opt(offset_minutes * 60)
        .unwrap_or_else(|| FixedOffset::east_opt(0).expect("zero is a valid offset"));
    DateTime::from_timestamp(secs, 0)
        .unwrap_or_default()
        .with_timezone(&zone)
}

fn local_offset(secs: i64) -> i32 {
    let t = Local
        .timestamp_opt(secs, 0)
        .single()
        .unwrap_or_else(|| Local.timestamp_opt(0, 0).unwrap());
    t.offset().local_minus_utc() / 60
}

/// The current day as `human` compares against: the year since 1900, month and day of month, and offset.
struct Today {
    year: i32,
    month0: u32,
    day: u32,
    tz: i32,
}

impl DateMode {
    pub fn plain(style: DateStyle) -> Self {
        Self {
            kind: Kind::Plain(style),
            local: false,
        }
    }

    pub fn is_local(&self) -> bool {
        self.local
    }

    pub fn localized(mut self, local: bool) -> Self {
        self.local = local;
        self
    }

    /// `auto:<mode>` is the mode only when standard output is a terminal, else `default`.
    pub fn parse(spec: &str, terminal: bool) -> Result<Self> {
        let mut format = spec;
        if let Some(rest) = format.strip_prefix("auto:") {
            format = if terminal { rest } else { "default" };
        }
        if format == "local" {
            format = "default-local";
        }
        const NAMES: [(&str, Option<DateStyle>); 11] = [
            ("relative", Some(DateStyle::Relative)),
            ("iso8601-strict", Some(DateStyle::IsoStrict)),
            ("iso-strict", Some(DateStyle::IsoStrict)),
            ("iso8601", Some(DateStyle::Iso)),
            ("iso", Some(DateStyle::Iso)),
            ("rfc2822", Some(DateStyle::Rfc)),
            ("rfc", Some(DateStyle::Rfc)),
            ("short", Some(DateStyle::Short)),
            ("default", Some(DateStyle::Default)),
            ("raw", Some(DateStyle::Raw)),
            ("unix", Some(DateStyle::Unix)),
        ];
        let (kind, rest) = if let Some((n, s)) = NAMES
            .iter()
            .find(|(n, _)| format.starts_with(n))
            .map(|(n, s)| (n, s.expect("every listed name has a style")))
        {
            (Kind::Plain(s), &format[n.len()..])
        } else if let Some(rest) = format.strip_prefix("human") {
            (Kind::Human, rest)
        } else if let Some(rest) = format.strip_prefix("format") {
            (Kind::Strftime(String::new()), rest)
        } else {
            bail!("unknown date format {format}");
        };
        let (local, rest) = match rest.strip_prefix("-local") {
            Some(r) => (true, r),
            None => (false, rest),
        };
        let kind = match kind {
            Kind::Strftime(_) => match rest.strip_prefix(':') {
                Some(f) => Kind::Strftime(f.to_string()),
                None => bail!("date format missing colon separator: {format}"),
            },
            k if rest.is_empty() => k,
            _ => bail!("unknown date format {format}"),
        };
        Ok(Self { kind, local })
    }

    /// Columns `blame` reserves for a date in this mode.
    pub fn width(&self) -> usize {
        match &self.kind {
            Kind::Plain(DateStyle::Default) => 30,
            Kind::Plain(DateStyle::Iso | DateStyle::IsoStrict) => 25,
            Kind::Plain(DateStyle::Rfc) => 31,
            Kind::Plain(DateStyle::Short | DateStyle::Unix) => 10,
            Kind::Plain(DateStyle::Relative) => 22,
            Kind::Plain(DateStyle::Raw) => 16,
            Kind::Human => 16,
            Kind::Strftime(_) => {
                let epoch = Signature {
                    name: Vec::new(),
                    email: Vec::new(),
                    time: 0,
                    offset_minutes: 0,
                };
                self.format(&epoch, 0).chars().count()
            }
        }
    }

    pub fn format(&self, who: &Signature, now: i64) -> String {
        let offset = if self.local {
            local_offset(who.time)
        } else {
            who.offset_minutes
        };
        let shifted = Signature {
            offset_minutes: offset,
            ..who.clone()
        };
        match &self.kind {
            Kind::Plain(DateStyle::Default) => self.normal(who.time, offset, None, now),
            Kind::Plain(style) => style.format(&shifted, now),
            Kind::Human => {
                let here = local_offset(now);
                let today = at(now, here);
                let today = Today {
                    year: today.year() - 1900,
                    month0: today.month0(),
                    day: today.day(),
                    tz: hhmm(here),
                };
                self.normal(who.time, offset, Some(today), now)
            }
            Kind::Strftime(fmt) => self.strftime(fmt, who.time, offset),
        }
    }

    fn normal(&self, secs: i64, offset: i32, today: Option<Today>, now: i64) -> String {
        let tm = at(secs, offset);
        let tz = hhmm(offset);
        let today = today.unwrap_or(Today {
            year: 0,
            month0: 0,
            day: 0,
            tz: -1,
        });
        let (mut date, mut wday, mut time, mut seconds) = (false, false, false, false);
        let mut tz_hidden = self.local || tz == today.tz;
        let year = tm.year() - 1900 == today.year;
        if year && tm.month0() == today.month0 {
            if tm.day() == today.day {
                date = true;
                wday = true;
            } else if tm.day() < today.day && tm.day() + 5 > today.day {
                date = true;
            }
        }
        if wday {
            return relative(now - secs);
        }
        if today.year != 0 {
            seconds = true;
            tz_hidden |= !date;
            wday = !year;
            time = wday;
        }
        let mut out = String::new();
        if !wday {
            out.push_str(&format!(
                "{} ",
                WEEKDAYS[tm.weekday().num_days_from_sunday() as usize]
            ));
        }
        if !date {
            out.push_str(&format!("{} {} ", MONTHS[tm.month0() as usize], tm.day()));
        }
        if !time {
            out.push_str(&format!("{:02}:{:02}", tm.hour(), tm.minute()));
            if !seconds {
                out.push_str(&format!(":{:02}", tm.second()));
            }
        } else {
            out.truncate(out.trim_end().len());
        }
        if !year {
            out.push_str(&format!(" {}", tm.year()));
        }
        if !tz_hidden {
            out.push_str(&format!(
                " {}{:04}",
                if tz < 0 { '-' } else { '+' },
                tz.abs()
            ));
        }
        out
    }

    fn strftime(&self, fmt: &str, secs: i64, offset: i32) -> String {
        let mut munged = String::new();
        let mut chars = fmt.chars();
        while let Some(c) = chars.next() {
            if c != '%' {
                munged.push(c);
                continue;
            }
            match chars.next() {
                Some('%') => munged.push_str("%%"),
                Some('s') => munged.push_str(&secs.to_string()),
                Some('z') => {
                    let tz = hhmm(offset);
                    munged.push_str(&format!(
                        "{}{:04}",
                        if tz < 0 { '-' } else { '+' },
                        tz.abs()
                    ));
                }
                Some('Z') if !self.local => {}
                Some(other) => {
                    munged.push('%');
                    munged.push(other);
                }
                None => munged.push('%'),
            }
        }
        let tm = at(secs, offset);
        let items: Vec<_> = chrono::format::StrftimeItems::new(&munged).collect();
        if items
            .iter()
            .any(|i| matches!(i, chrono::format::Item::Error))
        {
            return munged;
        }
        tm.format_with_items(items.into_iter()).to_string()
    }
}
