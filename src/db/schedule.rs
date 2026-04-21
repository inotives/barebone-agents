use chrono::{Datelike, NaiveTime, Timelike, Utc, Weekday};

/// Determine if a scheduled task is due for execution.
///
/// Schedule patterns:
/// - `hourly` — due if last_run is >= 3600s ago
/// - `daily@HH:MM` — due if past today's target and last_run before target
/// - `weekly@DAY@HH:MM` — due if past this week's target
/// - `every:Nh` — due if last_run >= N hours ago
/// - `every:Nm` — due if last_run >= N minutes ago
pub fn is_due(schedule: &str, last_run_at: Option<&str>) -> bool {
    let now = Utc::now();

    if schedule == "hourly" {
        return match parse_last_run(last_run_at) {
            Some(last) => (now - last).num_seconds() >= 3600,
            None => true,
        };
    }

    if let Some(time_str) = schedule.strip_prefix("daily@") {
        if let Some(target_time) = parse_time(time_str) {
            let today_target = now
                .date_naive()
                .and_time(target_time)
                .and_utc();
            let past_target = now >= today_target;
            let ran_after_target = match parse_last_run(last_run_at) {
                Some(last) => last >= today_target,
                None => false,
            };
            return past_target && !ran_after_target;
        }
        return false;
    }

    if let Some(rest) = schedule.strip_prefix("weekly@") {
        let parts: Vec<&str> = rest.splitn(2, '@').collect();
        if parts.len() == 2 {
            if let (Some(day), Some(target_time)) = (parse_weekday(parts[0]), parse_time(parts[1]))
            {
                let today = now.weekday();
                let days_since = (today.num_days_from_monday() as i64
                    - day.num_days_from_monday() as i64
                    + 7)
                    % 7;
                let target_date = now.date_naive() - chrono::Duration::days(days_since);
                let week_target = target_date.and_time(target_time).and_utc();

                let past_target = now >= week_target;
                let ran_after_target = match parse_last_run(last_run_at) {
                    Some(last) => last >= week_target,
                    None => false,
                };
                return past_target && !ran_after_target;
            }
        }
        return false;
    }

    if let Some(interval_str) = schedule.strip_prefix("every:") {
        if let Some(seconds) = parse_interval(interval_str) {
            return match parse_last_run(last_run_at) {
                Some(last) => (now - last).num_seconds() >= seconds,
                None => true,
            };
        }
        return false;
    }

    false
}

fn parse_last_run(last_run_at: Option<&str>) -> Option<chrono::DateTime<Utc>> {
    last_run_at.and_then(|s| {
        // Try common SQLite datetime formats
        chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M:%S")
            .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S"))
            .or_else(|_| chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%SZ"))
            .ok()
            .map(|dt| dt.and_utc())
    })
}

fn parse_time(s: &str) -> Option<NaiveTime> {
    NaiveTime::parse_from_str(s, "%H:%M").ok()
}

fn parse_weekday(s: &str) -> Option<Weekday> {
    match s.to_lowercase().as_str() {
        "mon" | "monday" => Some(Weekday::Mon),
        "tue" | "tuesday" => Some(Weekday::Tue),
        "wed" | "wednesday" => Some(Weekday::Wed),
        "thu" | "thursday" => Some(Weekday::Thu),
        "fri" | "friday" => Some(Weekday::Fri),
        "sat" | "saturday" => Some(Weekday::Sat),
        "sun" | "sunday" => Some(Weekday::Sun),
        _ => None,
    }
}

fn parse_interval(s: &str) -> Option<i64> {
    if let Some(hours) = s.strip_suffix('h') {
        return hours.parse::<i64>().ok().map(|h| h * 3600);
    }
    if let Some(mins) = s.strip_suffix('m') {
        return mins.parse::<i64>().ok().map(|m| m * 60);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn ago(seconds: i64) -> String {
        (Utc::now() - Duration::seconds(seconds))
            .format("%Y-%m-%d %H:%M:%S")
            .to_string()
    }

    #[test]
    fn test_hourly_never_run() {
        assert!(is_due("hourly", None));
    }

    #[test]
    fn test_hourly_ran_recently() {
        let last = ago(1800); // 30 min ago
        assert!(!is_due("hourly", Some(&last)));
    }

    #[test]
    fn test_hourly_ran_long_ago() {
        let last = ago(7200); // 2 hours ago
        assert!(is_due("hourly", Some(&last)));
    }

    #[test]
    fn test_every_4h_never_run() {
        assert!(is_due("every:4h", None));
    }

    #[test]
    fn test_every_4h_ran_recently() {
        let last = ago(3600); // 1 hour ago
        assert!(!is_due("every:4h", Some(&last)));
    }

    #[test]
    fn test_every_4h_ran_long_ago() {
        let last = ago(5 * 3600); // 5 hours ago
        assert!(is_due("every:4h", Some(&last)));
    }

    #[test]
    fn test_every_30m_never_run() {
        assert!(is_due("every:30m", None));
    }

    #[test]
    fn test_every_30m_ran_recently() {
        let last = ago(600); // 10 min ago
        assert!(!is_due("every:30m", Some(&last)));
    }

    #[test]
    fn test_every_30m_due() {
        let last = ago(2400); // 40 min ago
        assert!(is_due("every:30m", Some(&last)));
    }

    #[test]
    fn test_daily_due() {
        // Use a time in the past today
        let now = Utc::now();
        let past_time = if now.hour() > 0 {
            format!("{:02}:00", now.hour() - 1)
        } else {
            // Edge case: midnight — skip with a far-past last_run
            "23:59".to_string()
        };
        let schedule = format!("daily@{}", past_time);
        // Never run → should be due (if time has passed)
        if now.hour() > 0 {
            assert!(is_due(&schedule, None));
        }
    }

    #[test]
    fn test_daily_not_yet() {
        let now = Utc::now();
        let future_time = if now.hour() < 23 {
            format!("{:02}:00", now.hour() + 1)
        } else {
            "00:00".to_string()
        };
        let schedule = format!("daily@{}", future_time);
        if now.hour() < 23 {
            assert!(!is_due(&schedule, None));
        }
    }

    #[test]
    fn test_daily_already_ran_today() {
        let now = Utc::now();
        if now.hour() > 1 {
            let target = format!("{:02}:00", now.hour() - 1);
            let schedule = format!("daily@{}", target);
            let last = ago(60); // ran 1 min ago
            assert!(!is_due(&schedule, Some(&last)));
        }
    }

    #[test]
    fn test_invalid_schedule() {
        assert!(!is_due("garbage", None));
        assert!(!is_due("every:", None));
        assert!(!is_due("daily@invalid", None));
        assert!(!is_due("weekly@invalid", None));
    }

    #[test]
    fn test_parse_interval() {
        assert_eq!(parse_interval("4h"), Some(14400));
        assert_eq!(parse_interval("30m"), Some(1800));
        assert_eq!(parse_interval("1h"), Some(3600));
        assert_eq!(parse_interval("abc"), None);
        assert_eq!(parse_interval(""), None);
    }

    #[test]
    fn test_parse_weekday() {
        assert_eq!(parse_weekday("mon"), Some(Weekday::Mon));
        assert_eq!(parse_weekday("Friday"), Some(Weekday::Fri));
        assert_eq!(parse_weekday("SUN"), Some(Weekday::Sun));
        assert_eq!(parse_weekday("xyz"), None);
    }
}
