// Date's local time over a captured zone, the toLocaleTimeString and toLocaleDateString patterns, and the
// dateTimeFormat chain of parser.svelte.ts. Times are Date time values: milliseconds since the epoch.
use super::processor::{LocalePart, LocalePattern, TimeZone};
use super::{number_string, replace_literal, units, Units};

const DAY: f64 = 86_400_000.0;
const MONTHS: [&str; 12] =
    ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const MONTHS_SHORT: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const WEEKDAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
const WEEKDAYS_SHORT: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];

// TimeClip: NaN past 8.64e15 either way, else the integer part with -0 read as +0.
pub(super) fn time_clip(t: f64) -> f64 {
    if !t.is_finite() || t.abs() > 8.64e15 {
        return f64::NAN;
    }
    t.trunc() + 0.0
}

// The offset in effect at instant `t`; an instant outside the captured range declines.
fn offset(zone: &TimeZone, t: f64) -> Result<f64, String> {
    let first = zone.transitions.first().ok_or("time-zone-range")?;
    if !(t >= first.0 && t < zone.end) {
        return Err("time-zone-range".into());
    }
    let i = zone.transitions.partition_point(|(start, _)| *start <= t);
    Ok(zone.transitions[i - 1].1)
}

// Year, month and day of a day count from the epoch in the proleptic Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(month <= 2), month, day)
}
// The day count of the first of January of `year`.
fn new_year_day(year: i64) -> i64 {
    let y = year - 1;
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    // 1 January is day 306 of the March-based year that starts the previous March.
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + 306;
    era * 146_097 + doe - 719_468
}

pub(super) struct Fields {
    pub year: f64,
    // 0 for January.
    pub month: f64,
    pub day: f64,
    // 0 for Sunday.
    pub weekday: f64,
    pub hours: f64,
    pub minutes: f64,
    pub seconds: f64,
}
// The calendar fields of a finite time value.
fn fields(t: f64) -> Fields {
    let days = (t / DAY).floor();
    let ms = t - days * DAY;
    let (year, month, day) = civil_from_days(days as i64);
    Fields {
        year: year as f64,
        month: (month - 1) as f64,
        day: day as f64,
        weekday: (days + 4.0).rem_euclid(7.0),
        hours: (ms / 3_600_000.0).floor(),
        minutes: (ms / 60_000.0).floor() % 60.0,
        seconds: (ms / 1000.0).floor() % 60.0,
    }
}
// The local getters at `t`; None for an invalid Date, whose getters read NaN.
pub(super) fn local_fields(zone: &TimeZone, t: f64) -> Result<Option<Fields>, String> {
    if t.is_nan() {
        return Ok(None);
    }
    Ok(Some(fields(t + offset(zone, t)?)))
}
// The UTC getters at `t`.
pub(super) fn utc_fields(t: f64) -> Option<Fields> {
    (!t.is_nan()).then(|| fields(t))
}

// UTC(t) of ECMA-262 for local time `local`: the earliest instant whose local time it is, and for a local time a
// transition skips, the instant the offset before the transition gives.
pub(super) fn local_to_utc(zone: &TimeZone, local: f64) -> Result<f64, String> {
    if !local.is_finite() {
        return Ok(f64::NAN);
    }
    let (Some(first), low, high) = (zone.transitions.first(), local - 2.0 * DAY, local + 2.0 * DAY) else {
        return Err("time-zone-range".into());
    };
    // Offsets stay within a day, so the instant lies within a day of `local` either way.
    if !(low >= first.0 && high < zone.end) {
        return Err("time-zone-range".into());
    }
    let start = zone.transitions.partition_point(|(s, _)| *s <= low) - 1;
    let stop = zone.transitions.partition_point(|(s, _)| *s <= high);
    let next = |k: usize| zone.transitions.get(k + 1).map_or(zone.end, |t| t.0);
    let earliest = (start..stop)
        .map(|k| (k, local - zone.transitions[k].1))
        .filter(|&(k, u)| u >= zone.transitions[k].0 && u < next(k))
        .map(|(_, u)| u)
        .reduce(f64::min);
    if let Some(u) = earliest {
        return Ok(u);
    }
    for k in start + 1..stop {
        let ((at, after), before) = (zone.transitions[k], zone.transitions[k - 1].1);
        if local >= at + before && local < at + after {
            return Ok(local - before);
        }
    }
    Err("time-zone-range".into())
}

fn pad2(mut value: Units) -> Units {
    while value.len() < 2 {
        value.insert(0, 48);
    }
    value
}
fn digits(n: f64, two_digit: bool) -> Units {
    let text = number_string(n);
    if two_digit {
        pad2(text)
    } else {
        text
    }
}

// toLocaleTimeString() or toLocaleDateString() of `new Date(t)` in the captured pattern; a pattern the capture
// could not express declines.
pub(super) fn locale_string(zone: &TimeZone, pattern: Option<&LocalePattern>, t: f64) -> Result<Units, String> {
    let Some(f) = local_fields(zone, time_clip(t))? else { return Ok(units("Invalid Date")) };
    let pattern = pattern.ok_or("locale-format")?;
    let hour = f.hours as usize;
    let mut out = Vec::new();
    for part in &pattern.parts {
        match part {
            LocalePart::Literal(text) => out.extend_from_slice(text),
            LocalePart::Year => out.extend(number_string(f.year)),
            LocalePart::Month { two_digit } => out.extend(digits(f.month + 1.0, *two_digit)),
            LocalePart::Day { two_digit } => out.extend(digits(f.day, *two_digit)),
            LocalePart::Hour => out.extend_from_slice(pattern.hours.get(hour).ok_or("locale-format")?),
            LocalePart::Minute { two_digit } => out.extend(digits(f.minutes, *two_digit)),
            LocalePart::Second { two_digit } => out.extend(digits(f.seconds, *two_digit)),
            LocalePart::DayPeriod => out.extend_from_slice(pattern.day_periods.get(hour).ok_or("locale-format")?),
        }
    }
    Ok(out)
}

// dateTimeFormat(main, time) at `t`, the Date it builds: every replacement value is computed before the chain
// runs, and the Intl names of an invalid Date throw, which None reports.
pub(super) fn date_time_format(zone: &TimeZone, main: &[u16], t: f64) -> Result<Option<Units>, String> {
    if main.is_empty() {
        return Ok(Some(Vec::new()));
    }
    let main = main.strip_prefix(&[58]).unwrap_or(main);
    if main.len() > 300 {
        return Ok(Some(Vec::new()));
    }
    let Some(f) = local_fields(zone, t)? else { return Ok(None) };
    let year = number_string(f.year);
    // new Date(getFullYear(), 0, 0): local midnight before 1 January, with years 0 to 99 read as 1900 to 1999.
    let constructor_year = if (0.0..=99.0).contains(&f.year) { f.year + 1900.0 } else { f.year };
    let new_year_eve = time_clip(local_to_utc(zone, (new_year_day(constructor_year as i64) - 1) as f64 * DAY)?);
    let (month, weekday, hours) = (f.month as usize, f.weekday as usize, f.hours);
    let twelve = if hours % 12.0 == 0.0 { 12.0 } else { hours % 12.0 };
    let replacements: [(&str, Units); 16] = [
        ("YYYY", year.clone()),
        ("YY", year.get(2..).unwrap_or_default().to_vec()),
        ("MMMM", units(MONTHS[month])),
        ("MMM", units(MONTHS_SHORT[month])),
        ("MM", digits(f.month + 1.0, true)),
        ("DDDD", number_string(((t - new_year_eve) / DAY).floor())),
        ("DD", digits(f.day, true)),
        ("dddd", units(WEEKDAYS[weekday])),
        ("ddd", units(WEEKDAYS_SHORT[weekday])),
        ("HH", digits(hours, true)),
        ("hh", digits(twelve, true)),
        ("mm", digits(f.minutes, true)),
        ("ss", digits(f.seconds, true)),
        ("X", number_string((t / 1000.0).floor())),
        ("x", number_string(t)),
        ("A", units(if hours >= 12.0 { "PM" } else { "AM" })),
    ];
    let mut out = main.to_vec();
    for (token, value) in replacements {
        out = replace_literal(&out, &units(token), &value);
    }
    Ok(Some(out))
}

// Number.prototype.toFixed(0): the nearest integer, the larger magnitude on a tie, with the sign of a negative x.
pub(super) fn to_fixed0(x: f64) -> Units {
    if x.is_nan() {
        return units("NaN");
    }
    if x.abs() >= 1e21 {
        return number_string(x);
    }
    let sign = if x < 0.0 { "-" } else { "" };
    units(&format!("{sign}{:.0}", x.abs().round()))
}

// The h:mm:ss text of messageidleduration and idleduration for `duration` milliseconds.
pub(super) fn duration(duration: f64) -> Units {
    let seconds = (duration / 1000.0).floor();
    let minutes = (seconds / 60.0).floor();
    let hours = (minutes / 60.0).floor();
    [number_string(hours), units(":"), digits(minutes % 60.0, true), units(":"), digits(seconds % 60.0, true)].concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn zone(transitions: &[(f64, f64)]) -> TimeZone {
        TimeZone { transitions: transitions.to_vec(), end: 4e12 }
    }
    const HOUR: f64 = 3_600_000.0;

    #[test]
    fn calendar_fields_follow_the_proleptic_gregorian_calendar() {
        for (t, ymd, weekday) in [
            (0.0, (1970, 0, 1), 4.0),
            (-1.0, (1969, 11, 31), 3.0),
            (951_782_400_000.0, (2000, 1, 29), 2.0),
            (-2_208_988_800_000.0, (1900, 0, 1), 1.0),
            (4_107_542_399_999.0, (2100, 1, 28), 0.0),
        ] {
            let f = utc_fields(t).unwrap();
            assert_eq!((f.year, f.month, f.day, f.weekday), (ymd.0 as f64, ymd.1 as f64, ymd.2 as f64, weekday), "{t}");
            let first = new_year_day(ymd.0) as f64 * DAY;
            assert_eq!(utc_fields(first).map(|f| (f.month, f.day)), Some((0.0, 1.0)), "{t}");
        }
    }

    // A gap reads the offset before the transition and an overlap the earlier instant.
    #[test]
    fn local_times_a_transition_skips_or_repeats() {
        let spring = zone(&[(0.0, -5.0 * HOUR), (1e12, -4.0 * HOUR), (2e12, -5.0 * HOUR)]);
        let gap = 1e12 - 5.0 * HOUR + 0.5 * HOUR;
        assert_eq!(local_to_utc(&spring, gap), Ok(gap + 5.0 * HOUR));
        let overlap = 2e12 - 4.0 * HOUR - 0.5 * HOUR;
        assert_eq!(local_to_utc(&spring, overlap), Ok(overlap + 4.0 * HOUR));
        assert_eq!(local_to_utc(&spring, 1.5e12), Ok(1.5e12 + 4.0 * HOUR));
        assert_eq!(local_to_utc(&spring, 100.0), Err("time-zone-range".into()));
        assert_eq!(offset(&spring, 4e12), Err("time-zone-range".into()));
    }

    #[test]
    fn numbers_format_as_the_parser_writes_them() {
        assert_eq!(to_fixed0(-1.5), units("-2"));
        assert_eq!(to_fixed0(-0.4), units("-0"));
        assert_eq!(to_fixed0(-0.0), units("0"));
        assert_eq!(to_fixed0(2.5), units("3"));
        assert_eq!(to_fixed0(1e20), units("100000000000000000000"));
        assert_eq!(duration(-8_089_740_000.0), units("-2248:-9:00"));
        assert_eq!(duration(f64::NAN), units("NaN:NaN:NaN"));
        assert_eq!(time_clip(-0.5), 0.0);
        assert!(time_clip(-0.5).is_sign_positive() && time_clip(8.64e15 + 1.0).is_nan());
    }
}
