// data/schedules.json の読み書きと発火判定(docs/roadmap.md v0.4)。
// cron 式は使わず、業務向けの単純な周期モデル(daily/weekly/monthly)のみを扱う。
// スケジューラ本体(30秒ポーリング・キュー投入)は main.rs 側。

use chrono::{DateTime, Datelike, Local, NaiveDate, NaiveDateTime, NaiveTime, TimeZone};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SchedulesFile {
    pub version: u32,
    pub schedules: Vec<Schedule>,
}

impl Default for SchedulesFile {
    fn default() -> Self {
        Self { version: 1, schedules: Vec::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Schedule {
    pub id: String,
    pub agent_id: String,
    pub prompt: String,
    pub recurrence: Recurrence,
    pub enabled: bool,
    /// 発火時にアプリが書き戻す実際の発火時刻(RFC3339)。未実行なら None。
    #[serde(default)]
    pub last_run_at: Option<String>,
}

/// 反復指定(docs/roadmap.md v0.4: cron 式ではなく業務向けの単純モデル)。
/// weekday は 0=日〜6=土。day は 29-31 を指定した場合、その月の月末に丸める。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum Recurrence {
    Daily { time: String },
    Weekly { weekday: u32, time: String },
    Monthly { day: u32, time: String },
}

pub fn load(data_dir: &Path) -> Result<SchedulesFile, String> {
    let path = data_dir.join("schedules.json");
    if !path.exists() {
        return Ok(SchedulesFile::default());
    }
    let text = std::fs::read_to_string(&path).map_err(|e| format!("{} を読めません: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{} の形式が不正です: {e}", path.display()))
}

pub fn save(data_dir: &Path, file: &SchedulesFile) -> Result<(), String> {
    let path = data_dir.join("schedules.json");
    let json = serde_json::to_string_pretty(file).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| format!("{} に保存できません: {e}", path.display()))
}

/// `now` 時点で発火すべきか。「直近の予定発火時刻(now 以前で最も新しいもの)」を1つだけ求め、
/// それが `last_run_at` より新しければ due とする。アプリが長時間停止していて複数回分の
/// 予定時刻を過ぎていても、直近の1回分しか見ないため「まとめ焚き」にはならない。
pub fn is_due(schedule: &Schedule, now: DateTime<Local>) -> Result<bool, String> {
    if !schedule.enabled {
        return Ok(false);
    }
    let occurrence = most_recent_occurrence(&schedule.recurrence, now)?;
    match &schedule.last_run_at {
        None => Ok(true),
        Some(s) => {
            let last = DateTime::parse_from_rfc3339(s)
                .map_err(|e| format!("last_run_at の形式が不正です: {e}: {s}"))?
                .with_timezone(&Local);
            Ok(last < occurrence)
        }
    }
}

/// `now` 以前で最も新しい予定発火時刻を求める。
fn most_recent_occurrence(rec: &Recurrence, now: DateTime<Local>) -> Result<DateTime<Local>, String> {
    match rec {
        Recurrence::Daily { time } => {
            let (h, m) = parse_hhmm(time)?;
            let today = build_local(now.date_naive(), h, m)?;
            if today <= now {
                Ok(today)
            } else {
                build_local(now.date_naive() - chrono::Duration::days(1), h, m)
            }
        }
        Recurrence::Weekly { weekday, time } => {
            let (h, m) = parse_hhmm(time)?;
            if *weekday > 6 {
                return Err(format!("不正な weekday です(0-6): {weekday}"));
            }
            for back in 0..7 {
                let date = now.date_naive() - chrono::Duration::days(back);
                if date.weekday().num_days_from_sunday() == *weekday {
                    let occurrence = build_local(date, h, m)?;
                    if occurrence <= now {
                        return Ok(occurrence);
                    }
                }
            }
            Err("直近の発火時刻を計算できません".to_string())
        }
        Recurrence::Monthly { day, time } => {
            let (h, m) = parse_hhmm(time)?;
            if *day < 1 || *day > 31 {
                return Err(format!("不正な day です(1-31): {day}"));
            }
            // 今月・先月の順に、月末丸め込みした発火時刻が now 以前かを調べる。
            for months_back in 0..2 {
                let (y, mo) = add_months(now.year(), now.month(), -months_back);
                let clamped_day = (*day).min(days_in_month(y, mo));
                let date = NaiveDate::from_ymd_opt(y, mo, clamped_day)
                    .ok_or_else(|| format!("日付を計算できません: {y}-{mo:02}-{clamped_day:02}"))?;
                let occurrence = build_local(date, h, m)?;
                if occurrence <= now {
                    return Ok(occurrence);
                }
            }
            Err("直近の発火時刻を計算できません".to_string())
        }
    }
}

/// "HH:MM" をパースする。範囲外・書式不正はエラー。
fn parse_hhmm(time: &str) -> Result<(u32, u32), String> {
    let (h, m) = time
        .split_once(':')
        .ok_or_else(|| format!("不正な time 形式です(HH:MM ではありません): {time}"))?;
    let h: u32 = h.parse().map_err(|_| format!("不正な time 形式です: {time}"))?;
    let m: u32 = m.parse().map_err(|_| format!("不正な time 形式です: {time}"))?;
    if h > 23 || m > 59 {
        return Err(format!("不正な time 形式です(範囲外): {time}"));
    }
    Ok((h, m))
}

fn build_local(date: NaiveDate, hour: u32, minute: u32) -> Result<DateTime<Local>, String> {
    let time = NaiveTime::from_hms_opt(hour, minute, 0)
        .ok_or_else(|| format!("不正な time 形式です: {hour:02}:{minute:02}"))?;
    let naive = NaiveDateTime::new(date, time);
    Local
        .from_local_datetime(&naive)
        .single()
        .or_else(|| Local.from_local_datetime(&naive).earliest())
        .ok_or_else(|| format!("ローカル時刻を解決できません: {naive}"))
}

/// `year`/`month` に `delta` ヶ月を加算する(0 - 1 のごく小さい範囲の呼び出しのみ想定)。
fn add_months(year: i32, month: u32, delta: i32) -> (i32, u32) {
    let m0 = (month as i32 - 1) + delta;
    let y = year + m0.div_euclid(12);
    let m = (m0.rem_euclid(12) + 1) as u32;
    (y, m)
}

fn days_in_month(year: i32, month: u32) -> u32 {
    let (ny, nm) = add_months(year, month, 1);
    let first_of_next = NaiveDate::from_ymd_opt(ny, nm, 1).expect("valid date");
    let first_of_this = NaiveDate::from_ymd_opt(year, month, 1).expect("valid date");
    (first_of_next - first_of_this).num_days() as u32
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Local> {
        Local.with_ymd_and_hms(y, mo, d, h, mi, 0).single().expect("valid local datetime")
    }

    fn daily_schedule(last_run_at: Option<&str>) -> Schedule {
        Schedule {
            id: "s1".into(),
            agent_id: "agent".into(),
            prompt: "p".into(),
            recurrence: Recurrence::Daily { time: "09:00".into() },
            enabled: true,
            last_run_at: last_run_at.map(str::to_string),
        }
    }

    #[test]
    fn daily_due_at_exact_scheduled_time_when_never_run() {
        let s = daily_schedule(None);
        assert!(is_due(&s, dt(2026, 8, 12, 9, 0)).unwrap());
    }

    #[test]
    fn daily_not_due_before_scheduled_time() {
        // 前日分は既に実行済み。今日の09:00にまだ到達していないので発火しない。
        let s = daily_schedule(Some(&dt(2026, 8, 11, 9, 0).to_rfc3339()));
        assert!(!is_due(&s, dt(2026, 8, 12, 8, 59)).unwrap());
    }

    #[test]
    fn daily_not_due_again_same_day_after_run() {
        let s = daily_schedule(Some(&dt(2026, 8, 12, 9, 0).to_rfc3339()));
        assert!(!is_due(&s, dt(2026, 8, 12, 12, 0)).unwrap());
    }

    #[test]
    fn daily_fires_once_after_app_was_stopped_for_days() {
        // 3日前から止まっていても、直近1回分の判定だけで due になる(まとめ焚きしない)。
        let s = daily_schedule(Some(&dt(2026, 8, 9, 9, 0).to_rfc3339()));
        assert!(is_due(&s, dt(2026, 8, 12, 10, 0)).unwrap());
    }

    #[test]
    fn disabled_schedule_is_never_due() {
        let mut s = daily_schedule(None);
        s.enabled = false;
        assert!(!is_due(&s, dt(2026, 8, 12, 9, 0)).unwrap());
    }

    #[test]
    fn weekly_matches_correct_weekday_only() {
        // 2026-08-12 と 2026-08-05 はどちらも水曜日(weekday=3)。
        let s = Schedule {
            id: "s2".into(),
            agent_id: "agent".into(),
            prompt: "p".into(),
            recurrence: Recurrence::Weekly { weekday: 3, time: "09:00".into() },
            enabled: true,
            last_run_at: Some(dt(2026, 8, 5, 9, 0).to_rfc3339()), // 前週水曜に実行済み
        };
        // 今週水曜09:00 → 前回実行(前週水曜)より新しい予定なので発火する。
        assert!(is_due(&s, dt(2026, 8, 12, 9, 0)).unwrap());

        let s2 = Schedule { last_run_at: Some(dt(2026, 8, 12, 9, 0).to_rfc3339()), ..s };
        // 同じ週の木曜 → 直近の予定は同じ水曜のままなので発火しない。
        assert!(!is_due(&s2, dt(2026, 8, 13, 9, 0)).unwrap());
        // 翌週水曜 → 発火する。
        assert!(is_due(&s2, dt(2026, 8, 19, 9, 0)).unwrap());
    }

    #[test]
    fn monthly_31_clamps_to_30_in_april() {
        let s = Schedule {
            id: "s3".into(),
            agent_id: "agent".into(),
            prompt: "p".into(),
            recurrence: Recurrence::Monthly { day: 31, time: "09:00".into() },
            enabled: true,
            last_run_at: None,
        };
        // 4月は30日までしかないので、4/30 09:00 が発火時刻として扱われる。
        assert!(is_due(&s, dt(2026, 4, 30, 9, 0)).unwrap());
        let s2 = Schedule { last_run_at: Some(dt(2026, 4, 30, 9, 0).to_rfc3339()), ..s };
        assert!(!is_due(&s2, dt(2026, 4, 30, 23, 0)).unwrap());
    }

    #[test]
    fn monthly_31_clamps_to_month_end_in_february() {
        let s = Schedule {
            id: "s4".into(),
            agent_id: "agent".into(),
            prompt: "p".into(),
            recurrence: Recurrence::Monthly { day: 31, time: "09:00".into() },
            enabled: true,
            last_run_at: None,
        };
        // 2027年は閏年ではないので2月は28日まで。
        assert!(is_due(&s, dt(2027, 2, 28, 9, 0)).unwrap());
    }

    #[test]
    fn invalid_time_format_is_an_error() {
        let s = Schedule {
            id: "s5".into(),
            agent_id: "agent".into(),
            prompt: "p".into(),
            recurrence: Recurrence::Daily { time: "9時".into() },
            enabled: true,
            last_run_at: None,
        };
        assert!(is_due(&s, dt(2026, 8, 12, 9, 0)).is_err());
    }

    #[test]
    fn schedules_file_roundtrips_through_json() {
        let dir = std::env::temp_dir().join("agent_deck_test_schedule");
        std::fs::create_dir_all(&dir).unwrap();
        let file = SchedulesFile {
            version: 1,
            schedules: vec![daily_schedule(None)],
        };
        save(&dir, &file).unwrap();
        let loaded = load(&dir).unwrap();
        assert_eq!(loaded.schedules.len(), 1);
        assert_eq!(loaded.schedules[0].id, "s1");
        std::fs::remove_dir_all(&dir).ok();
    }
}
