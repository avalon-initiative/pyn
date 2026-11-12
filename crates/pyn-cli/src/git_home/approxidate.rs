//! Git's `approxidate` (`date.c`): the forgiving reading of `2 days ago`, `noon yesterday`, `Dec 25` and the like.

use chrono::{DateTime, Datelike, Local, NaiveDate, TimeZone, Timelike};

const MONTHS: [&str; 12] = [
    "january",
    "february",
    "march",
    "april",
    "may",
    "june",
    "july",
    "august",
    "september",
    "october",
    "november",
    "december",
];
const WEEKDAYS: [&str; 7] = [
    "sunday",
    "monday",
    "tuesday",
    "wednesday",
    "thursday",
    "friday",
    "saturday",
];
const NUMBERS: [&str; 11] = [
    "zero", "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
];
const UNITS: [(&str, i64); 5] = [
    ("seconds", 1),
    ("minutes", 60),
    ("hours", 3600),
    ("days", 86_400),
    ("weeks", 7 * 86_400),
];

/// A broken-down local time; `year` counts from 1900 and `mon` from 0, as in C.
#[derive(Clone, Copy)]
struct Tm {
    sec: i64,
    min: i64,
    hour: i64,
    mday: i64,
    mon: i64,
    year: i64,
    wday: i64,
}

fn localtime(ts: i64) -> Tm {
    let t: DateTime<Local> = Local
        .timestamp_opt(ts, 0)
        .earliest()
        .unwrap_or_else(|| Local.timestamp_opt(0, 0).unwrap());
    Tm {
        sec: t.second() as i64,
        min: t.minute() as i64,
        hour: t.hour() as i64,
        mday: t.day() as i64,
        mon: t.month0() as i64,
        year: t.year() as i64 - 1900,
        wday: t.weekday().num_days_from_sunday() as i64,
    }
}

/// `mktime`: fields out of range carry into the next ones.
fn mktime(tm: &Tm) -> i64 {
    let months = (tm.year + 1900) * 12 + tm.mon;
    let (year, mon) = (months.div_euclid(12), months.rem_euclid(12));
    let first = NaiveDate::from_ymd_opt(year as i32, mon as u32 + 1, 1)
        .or_else(|| NaiveDate::from_ymd_opt(if year < 0 { -262_000 } else { 262_000 }, 1, 1))
        .unwrap();
    let days = first.signed_duration_since(NaiveDate::from_ymd_opt(1970, 1, 1).unwrap());
    let naive_secs =
        (days.num_days() + tm.mday - 1) * 86_400 + tm.hour * 3600 + tm.min * 60 + tm.sec;
    let naive = chrono::DateTime::from_timestamp(naive_secs, 0)
        .unwrap_or_default()
        .naive_utc();
    Local
        .from_local_datetime(&naive)
        .earliest()
        .or_else(|| Local.from_local_datetime(&naive).latest())
        .map_or(naive_secs, |t| t.timestamp())
}

/// C's `int` multiplication wraps in the arithmetic on a pending count.
fn wrap(v: i64) -> i64 {
    i64::from(v as i32)
}

fn update_tm(tm: &mut Tm, now: &Tm, sec: i64) -> i64 {
    if tm.mday < 0 {
        tm.mday = now.mday;
    }
    if tm.mon < 0 {
        tm.mon = now.mon;
    }
    if tm.year < 0 {
        tm.year = now.year;
        if tm.mon > now.mon {
            tm.year -= 1;
        }
    }
    let n = mktime(tm) - sec;
    *tm = localtime(n);
    n
}

/// How many leading characters of `date` match `word` ignoring case; 0 on a clash inside the word.
fn match_string(date: &[u8], word: &str) -> usize {
    let word = word.as_bytes();
    let mut i = 0;
    while i < date.len() {
        let w = word.get(i).copied().unwrap_or(0);
        if date[i] != w && !date[i].eq_ignore_ascii_case(&w) {
            if !date[i].is_ascii_alphanumeric() {
                break;
            }
            return 0;
        }
        i += 1;
    }
    i
}

fn pending_number(tm: &mut Tm, num: &mut i64) {
    let number = std::mem::take(num);
    if number == 0 {
        return;
    }
    if tm.mday < 0 && number < 32 {
        tm.mday = number;
    } else if tm.mon < 0 && number < 13 {
        tm.mon = number - 1;
    } else if tm.year < 0 {
        if number > 1969 && number < 2100 {
            tm.year = number - 1900;
        } else if number > 69 && number < 100 {
            tm.year = number;
        } else if number < 38 {
            tm.year = 100 + number;
        }
    }
}

/// `tm_to_time_t`: seconds as if the fields were UTC, for 1970 to 2099 only.
fn tm_to_time_t(tm: &Tm) -> Option<i64> {
    const MDAYS: [i64; 12] = [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let year = tm.year - 70;
    if !(0..=129).contains(&year) || !(0..=11).contains(&tm.mon) {
        return None;
    }
    let mut day = tm.mday;
    if tm.mon < 2 || (year + 2) % 4 != 0 {
        day -= 1;
    }
    if tm.hour < 0 || tm.min < 0 || tm.sec < 0 {
        return None;
    }
    Some(
        (year * 365 + (year + 1) / 4 + MDAYS[tm.mon as usize] + day) * 86_400
            + tm.hour * 3600
            + tm.min * 60
            + tm.sec,
    )
}

/// `set_date`: Err when the triple is not a date; with `future_cap` a date after `now` is refused too.
fn set_date(year: i64, month: i64, day: i64, future: Option<&Tm>, now: i64, tm: &mut Tm) -> bool {
    if !(month > 0 && month < 13 && day > 0 && day < 32) {
        return false;
    }
    let mut check = *tm;
    let r = if future.is_some() {
        &mut check
    } else {
        &mut *tm
    };
    r.mon = month - 1;
    r.mday = day;
    if year == -1 {
        match future {
            Some(f) => r.year = f.year,
            None => return false,
        }
    } else if (1970..2100).contains(&year) {
        r.year = year - 1900;
    } else if year > 70 && year < 100 {
        r.year = year;
    } else if year < 38 {
        r.year = year + 100;
    } else {
        return false;
    }
    if future.is_none() {
        return true;
    }
    if tm_to_time_t(r).is_some_and(|specified| specified > now + 10 * 86_400) {
        return false;
    }
    tm.mon = check.mon;
    tm.mday = check.mday;
    if year != -1 {
        tm.year = check.year;
    }
    true
}

fn digits(date: &[u8], from: usize) -> (i64, usize) {
    let mut end = from;
    let mut value: i64 = 0;
    while end < date.len() && date[end].is_ascii_digit() {
        value = value
            .saturating_mul(10)
            .saturating_add(i64::from(date[end] - b'0'));
        end += 1;
    }
    (value, end)
}

/// `match_multi_number` for `a:b[:c]` times and `a-b-c` style dates; the length consumed, 0 for no match.
fn match_multi_number(
    num: i64,
    sep: u8,
    date: &[u8],
    start: usize,
    end: usize,
    tm: &mut Tm,
    now: i64,
) -> usize {
    let (num2, mut at) = digits(date, end + 1);
    let mut num3 = -1;
    if date.get(at) == Some(&sep) && date.get(at + 1).is_some_and(u8::is_ascii_digit) {
        let (v, e) = digits(date, at + 1);
        num3 = v;
        at = e;
    }
    match sep {
        b':' => {
            if num3 < 0 {
                num3 = 0;
            }
            if num < 25 && (0..60).contains(&num2) && (0..=60).contains(&num3) {
                tm.hour = num;
                tm.min = num2;
                tm.sec = num3;
                return at - start;
            }
            0
        }
        b'-' | b'/' | b'.' => {
            let nowtm = localtime_utc(now);
            let ok = (num > 70
                && (set_date(num, num2, num3, None, now, tm)
                    || set_date(num, num3, num2, None, now, tm)))
                || (sep != b'.' && set_date(num3, num, num2, Some(&nowtm), now, tm))
                || set_date(num3, num2, num, Some(&nowtm), now, tm)
                || (sep == b'.' && set_date(num3, num, num2, Some(&nowtm), now, tm));
            if ok { at - start } else { 0 }
        }
        _ => at - start,
    }
}

/// `gmtime` of `ts` in the same shape.
fn localtime_utc(ts: i64) -> Tm {
    let t = chrono::DateTime::from_timestamp(ts, 0).unwrap_or_default();
    Tm {
        sec: t.second() as i64,
        min: t.minute() as i64,
        hour: t.hour() as i64,
        mday: t.day() as i64,
        mon: t.month0() as i64,
        year: t.year() as i64 - 1900,
        wday: t.weekday().num_days_from_sunday() as i64,
    }
}

fn approxidate_digit(date: &[u8], at: usize, tm: &mut Tm, num: &mut i64, now: i64) -> usize {
    let (number, end) = digits(date, at);
    if let Some(&c) = date.get(end)
        && matches!(c, b':' | b'.' | b'/' | b'-')
        && date.get(end + 1).is_some_and(u8::is_ascii_digit)
    {
        let matched = match_multi_number(number, c, date, at, end, tm, now);
        if matched != 0 {
            return at + matched;
        }
    }
    if date[at] != b'0' || end - at <= 2 {
        *num = i64::from(number as i32);
    }
    end
}

fn time_of_day(tm: &mut Tm, now: &Tm, hour: i64) {
    if tm.hour < hour {
        update_tm(tm, now, 86_400);
    }
    tm.hour = hour;
    tm.min = 0;
    tm.sec = 0;
}

fn meridian(tm: &mut Tm, num: &mut i64, offset: i64) {
    let n = std::mem::take(num);
    let mut hour = tm.hour;
    if n != 0 {
        hour = n;
        tm.min = 0;
        tm.sec = 0;
    }
    tm.hour = hour % 12 + offset;
}

fn special(name: &str, tm: &mut Tm, now: &Tm, num: &mut i64) {
    match name {
        "yesterday" => {
            *num = 0;
            update_tm(tm, now, 86_400);
        }
        "noon" | "midnight" | "tea" => {
            pending_number(tm, num);
            let hour = match name {
                "noon" => 12,
                "midnight" => 0,
                _ => 17,
            };
            time_of_day(tm, now, hour);
        }
        "pm" => meridian(tm, num, 12),
        "am" => meridian(tm, num, 0),
        "never" => *tm = localtime(0),
        _ => {
            *num = 0;
            update_tm(tm, now, 0);
        }
    }
}

fn approxidate_alpha(
    date: &[u8],
    at: usize,
    tm: &mut Tm,
    now: &Tm,
    num: &mut i64,
    touched: &mut bool,
) -> usize {
    let mut end = at + 1;
    while end < date.len() && date[end].is_ascii_alphabetic() {
        end += 1;
    }
    let word = &date[at..];
    for (i, month) in MONTHS.iter().enumerate() {
        if match_string(word, month) >= 3 {
            tm.mon = i as i64;
            *touched = true;
            return end;
        }
    }
    for name in [
        "yesterday",
        "noon",
        "midnight",
        "tea",
        "pm",
        "am",
        "never",
        "now",
    ] {
        if match_string(word, name) == name.len() {
            special(name, tm, now, num);
            *touched = true;
            return end;
        }
    }
    if *num == 0 {
        for (i, name) in NUMBERS.iter().enumerate().skip(1) {
            if match_string(word, name) == name.len() {
                *num = i as i64;
                *touched = true;
                return end;
            }
        }
        if match_string(word, "last") == 4 {
            *num = 1;
            *touched = true;
        }
        return end;
    }
    for (name, length) in UNITS {
        if match_string(word, name) >= name.len() - 1 {
            update_tm(tm, now, wrap(length * *num));
            *num = 0;
            *touched = true;
            return end;
        }
    }
    for (i, name) in WEEKDAYS.iter().enumerate() {
        if match_string(word, name) >= 3 {
            let mut n = *num - 1;
            *num = 0;
            let mut diff = tm.wday - i as i64;
            if diff <= 0 {
                n += 1;
            }
            diff += 7 * n;
            update_tm(tm, now, wrap(diff * 86_400));
            *touched = true;
            return end;
        }
    }
    if match_string(word, "months") >= 5 {
        update_tm(tm, now, 0);
        let mut n = tm.mon - *num;
        *num = 0;
        while n < 0 {
            n += 12;
            tm.year -= 1;
        }
        tm.mon = n;
        *touched = true;
        return end;
    }
    if match_string(word, "years") >= 4 {
        update_tm(tm, now, 0);
        tm.year -= *num;
        *num = 0;
        *touched = true;
        return end;
    }
    end
}

const TIMEZONES: [(&str, i64, i64); 43] = [
    ("IDLW", -12, 0),
    ("NT", -11, 0),
    ("CAT", -10, 0),
    ("HST", -10, 0),
    ("HDT", -10, 1),
    ("YST", -9, 0),
    ("YDT", -9, 1),
    ("PST", -8, 0),
    ("PDT", -8, 1),
    ("MST", -7, 0),
    ("MDT", -7, 1),
    ("CST", -6, 0),
    ("CDT", -6, 1),
    ("EST", -5, 0),
    ("EDT", -5, 1),
    ("AST", -3, 0),
    ("ADT", -3, 1),
    ("WAT", -1, 0),
    ("GMT", 0, 0),
    ("UTC", 0, 0),
    ("Z", 0, 0),
    ("WET", 0, 0),
    ("BST", 0, 1),
    ("CET", 1, 0),
    ("MET", 1, 0),
    ("MEWT", 1, 0),
    ("MEST", 1, 1),
    ("CEST", 1, 1),
    ("MESZ", 1, 1),
    ("FWT", 1, 0),
    ("FST", 1, 1),
    ("EET", 2, 0),
    ("EEST", 2, 1),
    ("WAST", 7, 0),
    ("WADT", 7, 1),
    ("CCT", 8, 0),
    ("JST", 9, 0),
    ("EAST", 10, 0),
    ("EADT", 10, 1),
    ("GST", 10, 0),
    ("NZT", 12, 0),
    ("NZST", 12, 0),
    ("NZDT", 12, 1),
];

/// `parse_date_basic`'s `match_alpha`: month and weekday names, zone names, `am`/`pm` and a `T` before a time.
fn strict_alpha(date: &[u8], at: usize, tm: &mut Tm, offset: &mut i64) -> usize {
    let word = &date[at..];
    for (i, month) in MONTHS.iter().enumerate() {
        let m = match_string(word, month);
        if m >= 3 {
            tm.mon = i as i64;
            return m;
        }
    }
    for (i, day) in WEEKDAYS.iter().enumerate() {
        let m = match_string(word, day);
        if m >= 3 {
            tm.wday = i as i64;
            return m;
        }
    }
    for (name, off, dst) in TIMEZONES {
        let m = match_string(word, name);
        if m >= 3 || m == name.len() {
            if *offset == -1 {
                *offset = 60 * (off + dst);
            }
            return m;
        }
    }
    for (name, add) in [("PM", 12), ("AM", 0)] {
        if match_string(word, name) == 2 {
            tm.hour = tm.hour % 12 + add;
            return 2;
        }
    }
    if word[0] == b'T' && word.get(1).is_some_and(u8::is_ascii_digit) && tm.hour == -1 {
        tm.min = 0;
        tm.sec = 0;
        return 1;
    }
    1 + word[1..]
        .iter()
        .take_while(|c| c.is_ascii_alphabetic())
        .count()
}

/// `match_tz`: a `+hhmm`, `+hh` or `+hh:mm` offset; the length consumed.
fn strict_tz(date: &[u8], at: usize, offset: &mut i64) -> usize {
    let (mut hour, mut end) = digits(date, at + 1);
    let n = end - (at + 1);
    let mut min = 0;
    if n == 4 {
        min = hour % 100;
        hour /= 100;
    } else if n != 2 {
        min = 99;
    } else if date.get(end) == Some(&b':') {
        let (m, e) = digits(date, end + 1);
        (min, end) = (m, e);
        if end - (at + 1) != 5 {
            min = 99;
        }
    }
    if min < 60 && hour < 24 {
        let off = hour * 60 + min;
        *offset = if date[at] == b'-' { -off } else { off };
    }
    end - at
}

fn strict_digit(
    date: &[u8],
    at: usize,
    tm: &mut Tm,
    offset: &mut i64,
    gmt: &mut bool,
    now: i64,
) -> usize {
    let (num, end) = digits(date, at);
    let blank = [tm.year, tm.mon, tm.mday, tm.hour, tm.min, tm.sec]
        .iter()
        .all(|v| *v < 0);
    if num >= 100_000_000 && blank {
        *tm = localtime_utc(num);
        *gmt = true;
        return end - at;
    }
    if let Some(&c) = date.get(end)
        && matches!(c, b':' | b'.' | b'/' | b'-')
        && date.get(end + 1).is_some_and(u8::is_ascii_digit)
    {
        let matched = match_multi_number(num, c, date, at, end, tm, now);
        if matched != 0 {
            return matched;
        }
    }
    let n = end - at;
    if n == 8 || n == 6 {
        let (a, b, c) = (num / 10_000, num % 10_000 / 100, num % 100);
        let mut end = end;
        if n == 8 {
            set_date(a, b, c, None, now, tm);
        } else if (0..=24).contains(&a) && (0..60).contains(&b) && (0..=61).contains(&c) {
            (tm.hour, tm.min, tm.sec) = (a, b, c);
            if date.get(end) == Some(&b'.') && date.get(end + 1).is_some_and(u8::is_ascii_digit) {
                end = digits(date, end + 1).1;
            }
        }
        return end - at;
    }
    if tm.hour < 0 && tm.min == 0 {
        // the first number after a `T` is the time or spoils it
        let (h, m) = if n == 4 {
            (num / 100, num % 100)
        } else {
            (num, 0)
        };
        if (n == 2 || n == 4) && (0..=24).contains(&h) && (0..60).contains(&m) {
            (tm.hour, tm.min) = (h, m);
        } else {
            tm.min = -1;
        }
        return n;
    }
    if n == 4 {
        if num <= 1400 && *offset == -1 {
            *offset = num / 100 * 60 + num % 100;
        } else if num > 1900 && num < 2100 {
            tm.year = num - 1900;
        }
        return n;
    }
    if n > 2 {
        return n;
    }
    if num > 0 && num < 32 && tm.mday < 0 {
        tm.mday = num;
        return n;
    }
    if n == 2 && tm.year < 0 {
        if num < 10 && tm.mday >= 0 {
            tm.year = num + 100;
            return n;
        }
        if num >= 70 {
            tm.year = num;
            return n;
        }
    }
    if num > 0 && num < 13 && tm.mon < 0 {
        tm.mon = num - 1;
    }
    n
}

/// `parse_date_basic`: the strict reading of dates git itself writes, as seconds; None when it does not apply.
pub fn parse_date_basic(date: &str, now: i64) -> Option<i64> {
    let bytes = date.as_bytes();
    let mut tm = Tm {
        sec: -1,
        min: -1,
        hour: -1,
        mday: -1,
        mon: -1,
        year: -1,
        wday: 0,
    };
    let (mut offset, mut gmt) = (-1i64, false);
    let mut i = 0;
    if bytes.first() == Some(&b'@')
        && let Some(found) = object_header_date(&bytes[1..])
    {
        return Some(found);
    }
    while i < bytes.len() && bytes[i] != b'\n' {
        let c = bytes[i];
        let matched = if c.is_ascii_alphabetic() {
            strict_alpha(bytes, i, &mut tm, &mut offset)
        } else if c.is_ascii_digit() {
            strict_digit(bytes, i, &mut tm, &mut offset, &mut gmt, now)
        } else if (c == b'-' || c == b'+') && bytes.get(i + 1).is_some_and(u8::is_ascii_digit) {
            strict_tz(bytes, i, &mut offset)
        } else {
            0
        };
        i += matched.max(1);
    }
    let stamp = tm_to_time_t(&tm)?;
    if offset == -1 {
        let local = mktime(&tm);
        offset = if stamp > local {
            (stamp - local) / 60
        } else {
            -((local - stamp) / 60)
        };
    }
    Some(if gmt { stamp } else { stamp - offset * 60 })
}

/// `match_object_header_date`: `<seconds> +hhmm` as a commit header writes it.
fn object_header_date(date: &[u8]) -> Option<i64> {
    if !date.first()?.is_ascii_digit() {
        return None;
    }
    let (stamp, end) = digits(date, 0);
    if date.get(end) != Some(&b' ') || !matches!(date.get(end + 1), Some(b'+' | b'-')) {
        return None;
    }
    let stop = digits(date, end + 2).1;
    if stop != end + 6 || date.get(stop).is_some_and(|c| *c != b'\n') {
        return None;
    }
    Some(stamp)
}

/// The seconds `date` reads as, and whether any part of it was understood.
pub fn approxidate(date: &str, now: i64) -> (i64, bool) {
    let bytes = date.as_bytes();
    let now_tm = localtime(now);
    let mut tm = now_tm;
    tm.year = -1;
    tm.mon = -1;
    tm.mday = -1;
    let (mut num, mut touched) = (0i64, false);
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_digit() {
            pending_number(&mut tm, &mut num);
            i = approxidate_digit(bytes, i, &mut tm, &mut num, now);
            touched = true;
        } else if c.is_ascii_alphabetic() {
            i = approxidate_alpha(bytes, i, &mut tm, &now_tm, &mut num, &mut touched);
        } else {
            i += 1;
        }
    }
    pending_number(&mut tm, &mut num);
    (update_tm(&mut tm, &now_tm, 0), touched)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: i64 = 1_800_000_000;

    #[test]
    fn relative_forms_count_back_from_now() {
        assert_eq!(approxidate("10 seconds", NOW), (NOW - 10, true));
        assert_eq!(approxidate("in 2 days", NOW), (NOW - 2 * 86_400, true));
        assert!(!approxidate("garbage", NOW).1);
        assert!(!approxidate("", NOW).1);
    }

    #[test]
    fn strict_dates_read_as_git_writes_them() {
        let at = |d: &str| parse_date_basic(d, NOW);
        assert_eq!(at("2023-11-14 22:30:00 +0100"), Some(1_699_997_400));
        assert_eq!(
            at("2023-11-14T22:30 +0100"),
            at("2023-11-14 22:30:00 +0100")
        );
        assert_eq!(at("2023-11-14T2230 +0100"), at("2023-11-14 22:30:00 +0100"));
        assert_eq!(at("1700003500"), Some(1_700_003_500));
        assert_eq!(at("garbage"), None);
        assert_eq!(at("2023-11-14"), None);
    }

    #[test]
    fn a_month_a_year_and_a_time_without_a_day_land_before_the_first() {
        let at = |d: &str| parse_date_basic(d, NOW).unwrap();
        let june_first = parse_date_basic("2026-06-01 00:00:00 +0000", NOW).unwrap();
        assert_eq!(
            at("jun 5:00 2026 +0000"),
            june_first - 2 * 86_400 + 5 * 3600
        );
    }

    #[test]
    fn a_year_beyond_2099_is_not_a_date_and_reads_as_numbers() {
        let at = |d| localtime(approxidate(d, NOW).0);
        assert!(at("9999-12-31").year + 1900 < 2100);
        assert_eq!(match_string(b"Janu", "january"), 4);
        assert_eq!(match_string(b"jun5", "january"), 0);
    }
}
