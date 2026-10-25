// Calendar dates without a library: the ISO forms of `Date.parse` that
// ErsatzTV and EPGs use, and `Date.toISOString()` for the log.

use std::time::{SystemTime, UNIX_EPOCH};

/// Milliseconds since the epoch, now.
pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Proleptic Gregorian date of a day count since 1970-01-01 (H. Hinnant's algorithm).
pub fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Days since 1970-01-01 of the first day of `month` (1–12) in `year`.
pub fn days_from_civil(year: i64, month: u32) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let yoe = year.rem_euclid(400);
    let mp = i64::from((month + 9) % 12);
    let doy = (153 * mp + 2) / 5;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// `2026-10-24T20:15:00.123Z` for milliseconds since the epoch, as `Date.toISOString()`.
pub fn iso_time(epoch_ms: i64) -> String {
    let days = epoch_ms.div_euclid(86_400_000);
    let ms_of_day = epoch_ms.rem_euclid(86_400_000);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        ms_of_day / 3_600_000,
        ms_of_day / 60_000 % 60,
        ms_of_day / 1000 % 60,
        ms_of_day % 1000
    )
}

struct Cursor<'a> {
    bytes: &'a [u8],
    pos: usize,
}

impl Cursor<'_> {
    fn digits(&mut self, n: usize) -> Option<u32> {
        let part = self.bytes.get(self.pos..self.pos + n)?;
        if !part.iter().all(u8::is_ascii_digit) {
            return None;
        }
        self.pos += n;
        Some(part.iter().fold(0, |acc, b| acc * 10 + u32::from(b - b'0')))
    }

    fn eat(&mut self, byte: u8) -> bool {
        let found = self.bytes.get(self.pos) == Some(&byte);
        self.pos += usize::from(found);
        found
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }
}

/// Milliseconds since the epoch of an ISO date as `Date.parse` reads it:
/// `YYYY-MM-DD`, then `T`, `t` or a space and `HH:MM[:SS[.fraction]]`, then
/// `Z` or an offset `±HH:MM` / `±HHMM`. A day past the month's end rolls over
/// and `24:00` is the next midnight, as in V8. Unlike JavaScript, a date-time
/// without an offset is UTC, not the host's local time (ErsatzTV always
/// sends the offset). `None` where `Date.parse` gives NaN.
pub fn parse_iso(text: &str) -> Option<i64> {
    let mut c = Cursor { bytes: text.as_bytes(), pos: 0 };
    let year = i64::from(c.digits(4)?);
    if !c.eat(b'-') {
        return None;
    }
    let month = c.digits(2)?;
    if !c.eat(b'-') {
        return None;
    }
    let day = c.digits(2)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let date = (days_from_civil(year, month) + i64::from(day) - 1) * 86_400_000;
    if c.peek().is_none() {
        return Some(date);
    }

    if !matches!(c.peek(), Some(b'T' | b't' | b' ')) {
        return None;
    }
    c.pos += 1;
    let hour = c.digits(2)?;
    if !c.eat(b':') {
        return None;
    }
    let minute = c.digits(2)?;
    let (mut second, mut millis) = (0, 0);
    if c.eat(b':') {
        second = c.digits(2)?;
        if c.eat(b'.') {
            // Milliseconds from the first three digits; the rest is cut off.
            let start = c.pos;
            while c.peek().is_some_and(|b| b.is_ascii_digit()) {
                c.pos += 1;
            }
            let fraction = &c.bytes[start..c.pos];
            if fraction.is_empty() {
                return None;
            }
            millis = (0..3).fold(0, |acc, i| acc * 10 + fraction.get(i).map_or(0, |b| u32::from(b - b'0')));
        }
    }
    if minute > 59 || second > 59 || hour > 24 || (hour == 24 && (minute, second, millis) != (0, 0, 0)) {
        return None;
    }

    let offset_minutes = match c.peek() {
        None => 0,
        Some(b'Z' | b'z') => {
            c.pos += 1;
            0
        }
        Some(sign @ (b'+' | b'-')) => {
            c.pos += 1;
            let hours = c.digits(2)?;
            c.eat(b':');
            let minutes = c.digits(2)?;
            if hours > 23 || minutes > 59 {
                return None;
            }
            let total = i64::from(hours * 60 + minutes);
            if sign == b'-' { -total } else { total }
        }
        Some(_) => return None,
    };
    if c.peek().is_some() {
        return None;
    }

    let time = ((i64::from(hour) * 60 + i64::from(minute) - offset_minutes) * 60 + i64::from(second)) * 1000 + i64::from(millis);
    Some(date + time)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_time_like_to_iso_string() {
        assert_eq!(iso_time(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(iso_time(1_792_865_700_123), "2026-10-24T18:15:00.123Z");
        assert_eq!(iso_time(951_782_400_000), "2000-02-29T00:00:00.000Z");
        assert_eq!(iso_time(-1), "1969-12-31T23:59:59.999Z");
    }

    #[test]
    fn days_round_trip() {
        for days in [-800_000, -1, 0, 59, 60, 11_016, 20_750, 2_000_000] {
            let (year, month, day) = civil_from_days(days);
            assert_eq!(days_from_civil(year, month) + i64::from(day) - 1, days);
        }
    }

    #[test]
    fn date_time_without_offset_is_utc() {
        assert_eq!(parse_iso("2026-10-24T20:00:00"), parse_iso("2026-10-24T20:00:00Z"));
    }
}
