//! Pure schedule helpers shared by the scheduler tick (engine), the DB layer
//! (recompute `next_run_at` when a schedule is edited) and the command / MCP
//! layers (validation). No I/O here, so everything is unit-testable.

use chrono::{DateTime, Datelike, Duration as ChronoDuration, Local, NaiveDateTime, NaiveTime, TimeZone, Timelike, Weekday};

use crate::models::ScheduleConfig;

/// Storage format for run timestamps (`last_run_at` / `next_run_at`).
pub fn fmt_time(t: DateTime<Local>) -> String {
    t.format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Parse a local datetime. Accepts the storage format (`YYYY-MM-DD HH:MM[:SS]`)
/// **and** the HTML `<input type="datetime-local">` format (`YYYY-MM-DDTHH:MM[:SS]`)
/// that the workflow editor produces.
pub fn parse_time(s: &str) -> Option<DateTime<Local>> {
    const FORMATS: [&str; 4] = ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M", "%Y-%m-%dT%H:%M:%S", "%Y-%m-%dT%H:%M"];
    let s = s.trim();
    let naive = FORMATS.iter().find_map(|f| NaiveDateTime::parse_from_str(s, f).ok())?;
    Some(
        Local
            .from_local_datetime(&naive)
            .single()
            .or_else(|| Local.from_local_datetime(&naive).earliest())
            .unwrap_or_else(|| Local.from_utc_datetime(&naive)),
    )
}

/// Canonical form of a one-shot time as stored in the DB and shown in the UI: `YYYY-MM-DD HH:MM`.
pub fn canonical_once(at: &str) -> Option<String> {
    parse_time(at).map(|t| t.format("%Y-%m-%d %H:%M").to_string())
}

/// Strict `HH:MM` (or `HH:MM:SS`) parser used for validation. Unlike the lenient
/// clamp in `parse_hhmm`, garbage is rejected instead of silently becoming 00:00.
pub fn parse_hhmm_strict(s: &str) -> Option<(u32, u32)> {
    let s = s.trim();
    let t = NaiveTime::parse_from_str(s, "%H:%M")
        .or_else(|_| NaiveTime::parse_from_str(s, "%H:%M:%S"))
        .ok()?;
    Some((t.hour(), t.minute()))
}

fn parse_hhmm(s: &str) -> (u32, u32) {
    let mut it = s.trim().split(':');
    let h = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    let m = it.next().and_then(|v| v.parse().ok()).unwrap_or(0);
    (h.min(23), m.min(59))
}

/// Validate a schedule as entered by the user / an agent. Invalid or incomplete
/// input is rejected at save time instead of being silently turned into
/// "one minute from now" by the scheduler.
pub fn validate(sched: &ScheduleConfig) -> Result<(), String> {
    match sched {
        ScheduleConfig::Interval { every_minutes } => {
            if *every_minutes < 1 {
                return Err("间隔分钟数必须 ≥ 1".into());
            }
        }
        ScheduleConfig::Daily { time } => {
            parse_hhmm_strict(time).ok_or_else(|| format!("每日时间格式无效：「{time}」，应为 HH:MM"))?;
        }
        ScheduleConfig::Weekly { weekdays, time } => {
            if weekdays.is_empty() {
                return Err("每周定时至少选择一个星期".into());
            }
            if weekdays.iter().any(|d| !(1..=7).contains(d)) {
                return Err("星期取值应在 1（周一）到 7（周日）之间".into());
            }
            parse_hhmm_strict(time).ok_or_else(|| format!("每周时间格式无效：「{time}」，应为 HH:MM"))?;
        }
        ScheduleConfig::Once { at } => {
            if at.trim().is_empty() {
                return Err("请填写单次执行时间".into());
            }
            parse_time(at).ok_or_else(|| format!("单次执行时间格式无效：「{at}」，应为 YYYY-MM-DD HH:MM"))?;
        }
    }
    Ok(())
}

/// Normalise user input into the canonical stored form (currently only `Once.at`,
/// which may arrive as `YYYY-MM-DDTHH:MM` from the browser).
pub fn normalize(sched: ScheduleConfig) -> ScheduleConfig {
    match sched {
        ScheduleConfig::Once { at } => {
            let canonical = canonical_once(&at).unwrap_or(at);
            ScheduleConfig::Once { at: canonical }
        }
        other => other,
    }
}

/// Next occurrence strictly after `from`. `None` means the schedule has no future
/// occurrence (a one-shot whose time has passed or cannot be parsed, a weekly
/// schedule without weekdays) — callers must treat that as "do not run", never
/// as "run soon".
pub fn compute_next(sched: &ScheduleConfig, from: DateTime<Local>) -> Option<DateTime<Local>> {
    match sched {
        ScheduleConfig::Interval { every_minutes } => Some(from + ChronoDuration::minutes((*every_minutes).max(1))),
        ScheduleConfig::Daily { time } => {
            let (h, mi) = parse_hhmm(time);
            let today = from.date_naive().and_hms_opt(h, mi, 0).unwrap_or_default();
            let today = Local.from_local_datetime(&today).single().unwrap_or(from);
            Some(if today > from { today } else { today + ChronoDuration::days(1) })
        }
        ScheduleConfig::Weekly { weekdays, time } => {
            if weekdays.is_empty() {
                return None;
            }
            let (h, mi) = parse_hhmm(time);
            for add in 0..8 {
                let day = from.date_naive() + ChronoDuration::days(add);
                let wd = num_weekday(day.weekday());
                if weekdays.contains(&wd) {
                    if let Some(t) = day.and_hms_opt(h, mi, 0) {
                        if let Some(t) = Local.from_local_datetime(&t).single() {
                            if t > from {
                                return Some(t);
                            }
                        }
                    }
                }
            }
            Some(from + ChronoDuration::weeks(1))
        }
        ScheduleConfig::Once { at } => parse_time(at).filter(|t| *t > from),
    }
}

fn num_weekday(w: Weekday) -> u32 {
    match w {
        Weekday::Mon => 1,
        Weekday::Tue => 2,
        Weekday::Wed => 3,
        Weekday::Thu => 4,
        Weekday::Fri => 5,
        Weekday::Sat => 6,
        Weekday::Sun => 7,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local(s: &str) -> DateTime<Local> {
        parse_time(s).unwrap()
    }

    #[test]
    fn parse_time_accepts_storage_and_datetime_local_formats() {
        let expected = local("2026-09-27 09:30");
        assert_eq!(parse_time("2026-09-27T09:30"), Some(expected));
        assert_eq!(parse_time("2026-09-27T09:30:00"), Some(expected));
        assert_eq!(parse_time("2026-09-27 09:30:00"), Some(expected));
        assert_eq!(parse_time("  2026-09-27T09:30  "), Some(expected));
        assert!(parse_time("").is_none());
        assert!(parse_time("tomorrow").is_none());
        assert!(parse_time("2026-13-01T09:30").is_none());
    }

    #[test]
    fn once_from_datetime_local_input_fires_at_the_chosen_time_not_in_one_minute() {
        // Regression: the UI sends `YYYY-MM-DDTHH:MM`; the old parser failed on the
        // `T` and silently scheduled the run one minute from now.
        let now = local("2026-09-26 10:00");
        let sched = ScheduleConfig::Once { at: "2026-09-27T09:30".into() };
        assert_eq!(compute_next(&sched, now), Some(local("2026-09-27 09:30")));
        assert_eq!(normalize(sched), ScheduleConfig::Once { at: "2026-09-27 09:30".into() });
    }

    #[test]
    fn once_has_no_next_occurrence_after_it_is_due() {
        // Regression: after firing (or when the time has passed / is unparsable)
        // a one-shot must not keep re-arming itself every scheduler tick.
        let now = local("2026-09-27 09:31");
        assert_eq!(compute_next(&ScheduleConfig::Once { at: "2026-09-27 09:30".into() }, now), None);
        assert_eq!(compute_next(&ScheduleConfig::Once { at: "2026-09-27 09:31".into() }, now), None);
        assert_eq!(compute_next(&ScheduleConfig::Once { at: "".into() }, now), None);
        assert_eq!(compute_next(&ScheduleConfig::Once { at: "garbage".into() }, now), None);
    }

    #[test]
    fn validation_rejects_empty_or_malformed_schedules() {
        assert!(validate(&ScheduleConfig::Once { at: "".into() }).is_err());
        assert!(validate(&ScheduleConfig::Once { at: "27/09/2026".into() }).is_err());
        assert!(validate(&ScheduleConfig::Once { at: "2026-09-27T09:30".into() }).is_ok());
        assert!(validate(&ScheduleConfig::Interval { every_minutes: 0 }).is_err());
        assert!(validate(&ScheduleConfig::Interval { every_minutes: 1 }).is_ok());
        assert!(validate(&ScheduleConfig::Daily { time: "25:00".into() }).is_err());
        assert!(validate(&ScheduleConfig::Daily { time: "09:05".into() }).is_ok());
        assert!(validate(&ScheduleConfig::Weekly { weekdays: vec![], time: "09:00".into() }).is_err());
        assert!(validate(&ScheduleConfig::Weekly { weekdays: vec![8], time: "09:00".into() }).is_err());
        assert!(validate(&ScheduleConfig::Weekly { weekdays: vec![1, 7], time: "09:00".into() }).is_ok());
    }

    #[test]
    fn daily_weekly_and_interval_still_compute_the_next_slot() {
        let now = local("2026-09-26 10:00"); // a Saturday
        assert_eq!(compute_next(&ScheduleConfig::Interval { every_minutes: 15 }, now), Some(local("2026-09-26 10:15")));
        assert_eq!(compute_next(&ScheduleConfig::Daily { time: "09:00".into() }, now), Some(local("2026-09-27 09:00")));
        assert_eq!(compute_next(&ScheduleConfig::Daily { time: "10:30".into() }, now), Some(local("2026-09-26 10:30")));
        assert_eq!(
            compute_next(&ScheduleConfig::Weekly { weekdays: vec![1], time: "08:00".into() }, now),
            Some(local("2026-09-28 08:00"))
        );
        assert_eq!(compute_next(&ScheduleConfig::Weekly { weekdays: vec![], time: "08:00".into() }, now), None);
    }
}
