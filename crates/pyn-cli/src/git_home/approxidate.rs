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
        *num = number;
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
            update_tm(tm, now, length * *num);
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
            update_tm(tm, now, diff * 86_400);
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
    fn a_year_beyond_2099_is_not_a_date_and_reads_as_numbers() {
        let at = |d| localtime(approxidate(d, NOW).0);
        assert!(at("9999-12-31").year + 1900 < 2100);
        assert_eq!(match_string(b"Janu", "january"), 4);
        assert_eq!(match_string(b"jun5", "january"), 0);
    }
}
