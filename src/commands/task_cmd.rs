//! /任务 命令（对应原版 StandaloneTaskCommand）

use arona::framework::Framework;
use arona::runtime::message::OutgoingMessage;

/// /任务 [list|trigger <任务名>]
pub fn handle(framework: &Framework, arguments: &[String]) -> OutgoingMessage {
    let sub = arguments
        .first()
        .map(|s| s.to_lowercase())
        .unwrap_or_default();
    match sub.as_str() {
        "" | "list" | "列表" => list(framework),
        "trigger" | "触发" => trigger(framework, arguments.get(1).map(|s| s.as_str())),
        _ => usage(),
    }
}

fn usage() -> OutgoingMessage {
    OutgoingMessage::text("用法: /任务 查看全部定时任务; /任务 触发 <任务名> 手动触发")
}

fn list(framework: &Framework) -> OutgoingMessage {
    let tasks = framework.jobs().list();
    if tasks.is_empty() {
        return OutgoingMessage::text("当前没有定时任务");
    }
    let mut text = format!("定时任务({}):\n", tasks.len());
    for task in tasks {
        text.push_str(&format!("· {} [{}]", task.name, task.group));
        if let Some(next) = task.next_fire_ms {
            text.push_str(&format!(" 下次:{}", format_time(next)));
        }
        if let Some(last) = task.last_fire_ms {
            text.push_str(&format!(" 上次:{}", format_time(last)));
        }
        text.push('\n');
    }
    text.pop();
    OutgoingMessage::text(text)
}

fn trigger(framework: &Framework, name: Option<&str>) -> OutgoingMessage {
    let Some(name) = name else {
        return OutgoingMessage::text("用法: /任务 触发 <任务名>");
    };
    let name = name.trim();
    // 手动触发当天推送前先让出「今天已推过」的名额，否则会被入口那道去重挡下
    if name == crate::activity::notify::DAILY_JOB {
        crate::activity::notify::allow_manual_repush();
    }
    match framework.jobs().trigger(name) {
        Ok(()) => OutgoingMessage::text(format!("已触发任务: {name}")),
        Err(err) => OutgoingMessage::text(err),
    }
}

fn format_time(ms: i64) -> String {
    match chrono::DateTime::from_timestamp_millis(ms) {
        Some(time) => time
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        None => "未知".to_string(),
    }
}
