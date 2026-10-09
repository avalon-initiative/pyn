//! The one date and time format every command prints: `Jun 09 2026 14:05` in the operating system's timezone.

use chrono::{DateTime, Local, NaiveDateTime, TimeZone, Utc};

const FORMAT: &str = "%b %d %Y %H:%M";
const EMBEDDED: &str = "%Y-%m-%d %H:%M:%S%.f UTC";

/// A UTC instant as `Mon DD YYYY HH:MM` in the local timezone (`TZ` is honoured).
pub fn local(t: DateTime<Utc>) -> String {
    format_in(t, &Local)
}

/// Like [`local`] in an explicit timezone.
pub fn format_in<Tz: TimeZone>(t: DateTime<Utc>, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    t.with_timezone(tz).format(FORMAT).to_string()
}

/// Rewrites raw server timestamps (`2026-10-09 11:57:49.652435863 UTC`) embedded in `text` as [`local`] does.
pub fn localize(text: &str) -> String {
    localize_in(text, &Local)
}

/// Like [`localize`] in an explicit timezone.
pub fn localize_in<Tz: TimeZone>(text: &str, tz: &Tz) -> String
where
    Tz::Offset: std::fmt::Display,
{
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find(|c: char| c.is_ascii_digit()) {
        out.push_str(&rest[..start]);
        rest = &rest[start..];
        match NaiveDateTime::parse_and_remainder(rest, EMBEDDED) {
            Ok((naive, after)) => {
                out.push_str(&format_in(naive.and_utc(), tz));
                rest = after;
            }
            Err(_) => {
                let digits = rest
                    .find(|c: char| !c.is_ascii_digit())
                    .unwrap_or(rest.len());
                out.push_str(&rest[..digits]);
                rest = &rest[digits..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::FixedOffset;

    fn utc(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    fn east(hours: i32) -> FixedOffset {
        FixedOffset::east_opt(hours * 3600).unwrap()
    }

    #[test]
    fn utc_prints_month_day_year_and_24_hour_time() {
        assert_eq!(
            format_in(utc("2026-06-09T14:05:59Z"), &Utc),
            "Jun 09 2026 14:05"
        );
        assert_eq!(
            format_in(utc("2026-01-02T00:07:00Z"), &Utc),
            "Jan 02 2026 00:07"
        );
    }

    #[test]
    fn fractions_and_seconds_are_dropped() {
        assert_eq!(
            format_in(utc("2026-10-09T11:57:49.652435863Z"), &Utc),
            "Oct 09 2026 11:57"
        );
    }

    #[test]
    fn conversion_can_cross_the_date_line() {
        assert_eq!(
            format_in(utc("2026-06-09T02:30:00Z"), &east(-4)),
            "Jun 08 2026 22:30"
        );
        assert_eq!(
            format_in(utc("2026-12-31T23:30:00Z"), &east(9)),
            "Jan 01 2027 08:30"
        );
    }

    #[test]
    fn embedded_server_timestamps_are_reformatted() {
        let text =
            "src/a.cpp is locked by alice until 2026-10-09 11:57:49.652435863 UTC. Ask alice.";
        assert_eq!(
            localize_in(text, &east(-4)),
            "src/a.cpp is locked by alice until Oct 09 2026 07:57. Ask alice."
        );
        assert_eq!(
            localize_in("until 2026-10-09 11:57:49 UTC", &Utc),
            "until Oct 09 2026 11:57"
        );
    }

    #[test]
    fn text_without_a_timestamp_is_unchanged() {
        for text in [
            "",
            "r12 and 2026-10-09 are not timestamps",
            "path 2026-10-09 11:57:49 without zone",
            "123 4567",
        ] {
            assert_eq!(localize_in(text, &Utc), text);
        }
    }

    #[test]
    fn several_timestamps_in_one_message_are_all_rewritten() {
        let text = "2026-01-01 00:00:00 UTC to 2026-01-02 00:00:00.5 UTC";
        assert_eq!(
            localize_in(text, &Utc),
            "Jan 01 2026 00:00 to Jan 02 2026 00:00"
        );
    }
}
