//! 时间工具（对应原版 TimeUtil）
use chrono::{Datelike, Local, Timelike};

/// 今天的“日”，用于每日重置逻辑（对应 Calendar.DAY_OF_MONTH）
pub fn today() -> i64 {
    Local::now().day() as i64
}

/// 生成活动展示时间文本，分钟为 59 时按原版行为向上取整到下一小时
pub fn calc_time(ts_ms: i64, future: bool) -> String {
    let naive = chrono::DateTime::from_timestamp_millis(ts_ms)
        .map(|d| d.with_timezone(&chrono::Local))
        .unwrap_or_else(Local::now);
    let mut minutes = naive.minute();
    let mut hour = naive.hour();
    if minutes == 59 {
        minutes = 0;
        hour = (hour + 1) % 24;
    }
    let head = format!(
        "{:02}月{:02}日 {:02}:{:02}",
        naive.month(),
        naive.day(),
        hour,
        minutes
    );
    let suffix = if future { "开始" } else { "结束" };
    format!("{head}{suffix}")
}
