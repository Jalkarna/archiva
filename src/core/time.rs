use std::time::{SystemTime, SystemTimeError, UNIX_EPOCH};

pub fn now_utc_millis() -> Result<String, SystemTimeError> {
    format_utc_millis(SystemTime::now())
}

pub fn format_utc_millis(time: SystemTime) -> Result<String, SystemTimeError> {
    let duration = time.duration_since(UNIX_EPOCH)?;
    let total_millis = duration.as_millis();
    let total_seconds = (total_millis / 1000) as i64;
    let millis = (total_millis % 1000) as u32;
    let days = total_seconds.div_euclid(86_400);
    let seconds_of_day = total_seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = seconds_of_day / 3_600;
    let minute = (seconds_of_day % 3_600) / 60;
    let second = seconds_of_day % 60;

    Ok(format!(
        "{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z"
    ))
}

pub fn parse_utc_millis(value: &str) -> Option<i128> {
    let bytes = value.as_bytes();
    if bytes.len() != 24
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
        || bytes[19] != b'.'
        || bytes[23] != b'Z'
    {
        return None;
    }

    let year = parse_digits(bytes, 0, 4)? as i32;
    let month = parse_digits(bytes, 5, 7)? as u32;
    let day = parse_digits(bytes, 8, 10)? as u32;
    let hour = parse_digits(bytes, 11, 13)?;
    let minute = parse_digits(bytes, 14, 16)?;
    let second = parse_digits(bytes, 17, 19)?;
    let millis = parse_digits(bytes, 20, 23)?;

    if !(1..=12).contains(&month)
        || day == 0
        || day > days_in_month(year, month)
        || hour >= 24
        || minute >= 60
        || second >= 60
    {
        return None;
    }

    let days = days_from_civil(year, month, day) as i128;
    Some(
        (((days * 86_400) + (hour * 3_600 + minute * 60 + second) as i128) * 1000) + millis as i128,
    )
}

fn parse_digits(bytes: &[u8], start: usize, end: usize) -> Option<i64> {
    let mut value = 0_i64;
    for byte in bytes.get(start..end)? {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value * 10 + i64::from(byte - b'0');
    }
    Some(value)
}

fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 }.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096).div_euclid(365);
    let year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2).div_euclid(153);
    let day = doy - (153 * mp + 2).div_euclid(5) + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = year + if month <= 2 { 1 } else { 0 };
    (year as i32, month as u32, day as u32)
}

fn days_from_civil(year: i32, month: u32, day: u32) -> i64 {
    let year = i64::from(year) - if month <= 2 { 1 } else { 0 };
    let era = if year >= 0 { year } else { year - 399 }.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(month);
    let day = i64::from(day);
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2).div_euclid(5) + day - 1;
    let day_of_era =
        year_of_era * 365 + year_of_era.div_euclid(4) - year_of_era.div_euclid(100) + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn days_in_month(year: i32, month: u32) -> u32 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: i32) -> bool {
    (year % 4 == 0 && year % 100 != 0) || year % 400 == 0
}

#[cfg(test)]
mod tests {
    use super::{format_utc_millis, parse_utc_millis};
    use std::time::{Duration, UNIX_EPOCH};

    #[test]
    fn formats_unix_epoch_with_millisecond_precision() {
        assert_eq!(
            format_utc_millis(UNIX_EPOCH).unwrap(),
            "1970-01-01T00:00:00.000Z"
        );
        assert_eq!(
            format_utc_millis(UNIX_EPOCH + Duration::from_millis(1_782_505_878_340)).unwrap(),
            "2026-06-26T20:31:18.340Z"
        );
    }

    #[test]
    fn parses_fixed_utc_millisecond_timestamps() {
        assert_eq!(parse_utc_millis("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(
            parse_utc_millis("2026-06-26T20:31:18.340Z"),
            Some(1_782_505_878_340)
        );
        assert_eq!(
            parse_utc_millis("2024-02-29T00:00:00.000Z"),
            Some(1_709_164_800_000)
        );
        assert_eq!(parse_utc_millis("2023-02-29T00:00:00.000Z"), None);
        assert_eq!(parse_utc_millis("2026-06-26 20:31:18.340Z"), None);
        assert_eq!(parse_utc_millis("2026-06-26T20:31:60.340Z"), None);
    }
}
