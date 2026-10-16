//! 每日活动推送与结束预警服务（对应原版 standalone/commands/StandaloneActivityNotify）
//! 每天定时向配置的群推送 国服/国际服/日服 活动日历图片（纯 Rust 渲染, 失败回退文本）；
//! 启动与每日运行时查询活动结束预警(提前1小时/双倍掉落提前5小时)，正负10分钟内立即发送，
//! 过期抛弃，未来添加定时任务(去重)。
//! 1 小时预警的同时安排「到期后 5 分钟」刷新本地资源图片任务；另有每日 0 点的全量刷新任务。

use crate::config::{self, NotifyConfig};
use crate::data;
use crate::entity::{Activity, ActivityType, ServerLocale};
use arona::framework::Framework;
use arona::plugin::PluginContext;
use arona::quartz::JobFn;
use arona::runtime::message::{MessageTarget, OutgoingMessage};
use arona::services;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

/// 每日活动推送任务名（`on_config_reload` 换推送小时时要按它撤旧建新）
pub(crate) const DAILY_JOB: &str = "ActivityNotify";
/// 最近一次「每日日历推送」归属的本地日期键（yyyymmdd）。
///
/// 框架的 Daily 任务一次触发只回调一次，可插件这一层没有「今天就推过」的记忆：启动 20 秒
/// 后的测试推送（`--test-notify`）与配置的定时任务撞在一起、定时任务被按名替换重建、系统
/// 时钟回拨让整点又走一遍，都会把同一份日历再发一次。这里记下当天日期，同一天的第二次
/// 触发直接被吃掉——「每日推送」本来就该一天只出一份。
static LAST_DAILY_PUSH_DAY: AtomicI64 = AtomicI64::new(0);
/// 启动后的预警初始化任务名（延后 20 秒，只调度不推送）
pub(crate) const INIT_JOB: &str = "ActivityNotifyInit";

const NORMAL_ACTIVITY_NOTIFY_BEFORE_HOURS: i64 = 1;
const DROP_ACTIVITY_NOTIFY_BEFORE_HOURS: i64 = 5;
/// 正负 10 分钟窗口
const ALERT_IMMEDIATE_WINDOW_MILLIS: i64 = 10 * 60 * 1000;
/// 活动到期后刷新本地资源图片的延迟: 1 小时预警 + 1 小时 5 分钟
const IMAGE_REFRESH_AFTER_END_MILLIS: i64 = 5 * 60 * 1000;

/// 启用每日推送：登记可开关的服务 + 建每日任务与启动初始化任务（start 阶段调用）
pub fn enable_service(ctx: &PluginContext) {
    ctx.register_service(services::service_info(12, "活动推送", false, false));
    let hour = config::notify(ctx.framework()).every_day_hour.clamp(0, 23) as u32;
    enable_daily_job(ctx, hour);
    arona::runtime::log::info(format!("活动推送已启用, 每天 {hour} 点推送"));
}

/// 创建每天固定小时的活动推送任务与启动 20 秒后的预警初始化任务
/// （推送小时变更时由 `BluearchivePlugin::on_config_reload` 重建）。
pub fn enable_daily_job(ctx: &PluginContext, hour: u32) {
    // JobFn 是同步闭包：真要推送就得在这里把异步工作丢进本插件的任务作用域，
    // 用 ctx.spawn_as 而不是 tokio::spawn，否则插件被停用时任务收不回去。
    let scheduled = ctx.clone();
    let notify: JobFn = {
        let scoped = scheduled.clone();
        Arc::new(move || {
            // spawn_as 要借用 context，async 块又要把整份 ctx 搬进去：一份发起、一份搬走
            let inner = scoped.clone();
            scoped.spawn_as("定时推送", async move {
                push(&inner, false).await;
            });
        })
    };
    let init: JobFn = {
        let scoped = scheduled;
        Arc::new(move || {
            let inner = scoped.clone();
            scoped.spawn_as("预警调度", async move {
                push(&inner, true).await;
            });
        })
    };
    schedule_jobs(ctx.framework(), hour, notify, init);
}

/// 两个任务的排期：每日任务同名替换，启动初始化任务已存在就不重建
/// （`enable_daily_job` 会随每次配置热重载重跑一遍）。
/// 任务组一律填本插件的 id：插件被停用时框架按组一次收干净，不必逐个记名字。
fn schedule_jobs(framework: &Framework, hour: u32, notify: JobFn, init: JobFn) {
    framework
        .jobs()
        .create_daily(hour, DAILY_JOB, crate::PLUGIN_ID, notify);
    if !framework.jobs().exists(INIT_JOB) {
        framework
            .jobs()
            .create_delay(20, INIT_JOB, crate::PLUGIN_ID, init);
    }
}

/// 本地日期编码成「今天」的键（yyyymmdd）：同一天算同一个槽位
fn daily_push_day(now: chrono::DateTime<chrono::Local>) -> i64 {
    now.format("%Y%m%d").to_string().parse().unwrap_or(0)
}

/// 认领当天的日历推送名额：把旧值换成今天的那一次才算抢到。
/// 一发原子的 swap，重复触发（哪怕并发）只有一个能拿到 true。
fn claim_daily_push_day(state: &AtomicI64, day: i64) -> bool {
    state.swap(day, Ordering::SeqCst) != day
}

/// 让出当天的名额（清回「未推」）
fn release_daily_push_day(state: &AtomicI64) {
    state.store(0, Ordering::SeqCst);
}

/// 手动重推入口：`/任务 触发` 当天推送前先让出名额，免得被入口那道去重挡下
pub(crate) fn allow_manual_repush() {
    release_daily_push_day(&LAST_DAILY_PUSH_DAY);
}

/// 推送目标群：全局允许的群去掉黑名单，并去重
/// （配置里同一个群出现两次时，不该把同一条日历往那个群发两遍）
fn resolve_targets(config: &NotifyConfig, global_groups: Vec<i64>) -> Vec<i64> {
    let mut seen = std::collections::HashSet::new();
    global_groups
        .into_iter()
        .filter(|group| !config.black_groups.contains(group))
        .filter(|group| seen.insert(*group))
        .collect()
}

/// 配置里启用的服务器（日服 → 国际服 → 国服，与推送次序一致）
fn enabled_servers(config: &NotifyConfig) -> Vec<ServerLocale> {
    let mut servers: Vec<ServerLocale> = Vec::new();
    if config.jp {
        servers.push(ServerLocale::JP);
    }
    if config.global {
        servers.push(ServerLocale::GLOBAL);
    }
    if config.cn {
        servers.push(ServerLocale::CN);
    }
    servers
}

/// 一个服务器的活动数据：(进行中, 即将开始)
type Activities = (Vec<Activity>, Vec<Activity>);

/// 按日服 → 国际服 → 国服的顺序各拉一次，日历与预警共用这一份
/// （原先预警阶段和日历阶段各自拉一遍，一次推送要发 6 次活动查询）
///
/// 这里是故意串行而不是并发：活动查询本身只要 0.2 秒，而它顺带拉的学生生日有 1.75 MB，
/// 并发会让那一份被同时下三遍；顺序跑第一服就把缓存喂热了，后两服不再联网。
async fn fetch_all(servers: &[ServerLocale]) -> Vec<(ServerLocale, Activities)> {
    let mut pairs: Vec<(ServerLocale, Activities)> = Vec::new();
    for &server in servers {
        match data::activity::fetch(server).await {
            Ok(pair) => pairs.push((server, pair)),
            Err(err) => {
                arona::runtime::log::warning(format!("拉取{}活动失败: {err}", server.server_name()))
            }
        }
    }
    pairs
}

/// 每日推送与预警调度入口
/// skip_image=true 表示启动时的初始化运行: 只做 5小时/1小时 预警调度, 不推送日历
pub async fn push(ctx: &PluginContext, skip_image: bool) {
    let framework = ctx.framework();
    let config = config::notify(framework);
    if !config.enable {
        return;
    }
    if !framework.runtime_services().sender_ready() {
        arona::runtime::log::warning("活动推送未执行: 消息发送器尚未就绪");
        return;
    }
    let targets = resolve_targets(&config, framework.gating().groups());
    if targets.is_empty() {
        arona::runtime::log::warning("活动推送未执行: 未配置推送目标群");
        return;
    }
    let pairs = fetch_all(&enabled_servers(&config)).await;
    // 日历一天只推一次：定时任务被重复触发/重建、系统时钟回拨让整点又走一遍、启动 20 秒
    // 后的测试推送与定时任务撞在一起，第二条都在这里被吃掉，免得群里出现两份日历。
    // 认领只吃掉日历那一步，预警调度照旧每轮都跑（它与日历共用这份数据，不重复发）。
    let mut push_calendar = !skip_image;
    if push_calendar {
        let day = daily_push_day(chrono::Local::now());
        push_calendar = claim_daily_push_day(&LAST_DAILY_PUSH_DAY, day);
        if !push_calendar {
            arona::runtime::log::info("今日活动日历已推送过, 跳过本次重复触发");
        }
    }
    // 日历排在预警之前：预警阶段可能有几条正落在 ±10 分钟窗口里的活动要就地发送，
    // 让它们挡在整点日历前面，等于用户看到的推送又晚一轮。
    if push_calendar {
        for (server, pair) in &pairs {
            let prefix = format!("{}({})", config.notify_text, server.server_name());
            push_server(framework, *server, pair, &prefix, &targets).await;
        }
    }
    schedule_alerts(ctx, &pairs).await;
}

/// 向目标群推送某个服务器的活动日历（文本前缀 + 活动图片, 图片渲染失败时回退文本）
async fn push_server(
    framework: &Framework,
    server: ServerLocale,
    pair: &Activities,
    prefix: &str,
    targets: &[i64],
) {
    let message = match crate::image::activity::render_async(pair.clone(), server).await {
        Ok(file) => {
            OutgoingMessage::text(format!("{prefix}\n"))
                + OutgoingMessage::image_file(file.to_string_lossy())
        }
        Err(err) => {
            arona::runtime::log::warning(format!(
                "{}活动图片生成失败, 回退文本: {err}",
                server.server_name()
            ));
            let calendar = data::activity::format_calendar(pair, server);
            OutgoingMessage::text(format!("{prefix}\n{calendar}"))
        }
    };
    for &group_id in targets {
        let receipt = arona::plugin::action_async(
            "发送消息",
            framework
                .runtime_services()
                .send_message(MessageTarget::Group(group_id), message.clone()),
        )
        .await;
        if receipt.message_id.is_none() {
            arona::runtime::log::warning(format!(
                "推送{}活动到群 {group_id} 失败",
                server.server_name()
            ));
        }
    }
}

/// 安排各服务器的活动结束预警（数据由 `fetch_all` 一次拉好，这里不再联网）
async fn schedule_alerts(ctx: &PluginContext, pairs: &[(ServerLocale, Activities)]) {
    for (server, pair) in pairs {
        for (activities, hours) in alert_plan(&pair.0) {
            schedule_alert_group(ctx, &activities, *server, hours).await;
        }
    }
}

/// 计算某服务器活动的预警安排: 除生日外的所有活动在结束前 5 小时与 1 小时各提醒一次;
/// 维护单列只在 1 小时提醒(特殊文案)
fn alert_plan(active: &[Activity]) -> Vec<(Vec<Activity>, i64)> {
    let alertable: Vec<Activity> = active
        .iter()
        .filter(|it| it.activity_type != ActivityType::Birthday)
        .cloned()
        .collect();
    let maintenance: Vec<Activity> = alertable
        .iter()
        .filter(|it| is_maintenance(it))
        .cloned()
        .collect();
    let normal: Vec<Activity> = alertable
        .iter()
        .filter(|it| !is_maintenance(it))
        .cloned()
        .collect();
    vec![
        (normal.clone(), DROP_ACTIVITY_NOTIFY_BEFORE_HOURS),
        (normal, NORMAL_ACTIVITY_NOTIFY_BEFORE_HOURS),
        (maintenance, NORMAL_ACTIVITY_NOTIFY_BEFORE_HOURS),
    ]
}

/// 一批活动的处置安排: 现在就发, 还是排到某个时刻
#[derive(Debug)]
enum AlertAction {
    Now(Vec<Activity>),
    At(i64, Vec<Activity>),
}

/// 纯计算（不碰框架，所以分组与窗口边界可以用表驱动用例逐个钉住）:
/// 按「结束时刻 - 提前量」分组，正负 10 分钟内立即发送，已过时的丢掉，未来的留给排期。
fn plan_alerts(activities: &[Activity], now_ms: i64, before_hours: i64) -> Vec<AlertAction> {
    let mut groups: std::collections::BTreeMap<i64, Vec<Activity>> =
        std::collections::BTreeMap::new();
    for activity in activities {
        if activity.end_time <= 0 || activity.activity_type == ActivityType::Birthday {
            continue;
        }
        groups
            .entry(activity.end_time - before_hours * 60 * 60 * 1000)
            .or_default()
            .push(activity.clone());
    }
    let window = ALERT_IMMEDIATE_WINDOW_MILLIS;
    groups
        .into_iter()
        .filter_map(|(notify_at, group)| {
            if notify_at < now_ms - window {
                None
            } else if notify_at <= now_ms + window {
                Some(AlertAction::Now(group))
            } else {
                Some(AlertAction::At(notify_at, group))
            }
        })
        .collect()
}

/// 按提醒时间分组处理: 窗口内立即发送, 过时抛弃, 未来添加定时任务(去重)
async fn schedule_alert_group(
    ctx: &PluginContext,
    activities: &[Activity],
    locale: ServerLocale,
    before_hours: i64,
) {
    if activities.is_empty() {
        return;
    }
    let now = chrono::Utc::now().timestamp_millis();
    for action in plan_alerts(activities, now, before_hours) {
        match action {
            AlertAction::Now(group) => {
                send_alert(ctx.framework(), &group, locale, before_hours).await
            }
            AlertAction::At(notify_at, group) => {
                insert_alert(ctx, &group, notify_at, locale, before_hours)
            }
        }
    }
    if before_hours == NORMAL_ACTIVITY_NOTIFY_BEFORE_HOURS {
        // 1 小时预警同时安排「到期后 5 分钟」刷新本地资源图片
        schedule_image_refresh(ctx, activities, locale);
    }
}

/// 预警任务的 key: 同一活动、同一提前量只会排出一个任务
fn alert_key(locale: ServerLocale, expected_ms: i64, before_hours: i64) -> String {
    format!(
        "ActivityNotifyOneHour-{}-{expected_ms}-{before_hours}",
        locale.command_name()
    )
}

/// 图片刷新任务的 key
fn image_refresh_key(locale: ServerLocale, expected_ms: i64) -> String {
    format!(
        "AronaActivityImageRefresh-{}-{expected_ms}",
        locale.command_name()
    )
}

/// 创建单次定时任务用于活动结束预警; 已存在同 key 任务则跳过, 避免重复
fn insert_alert(
    ctx: &PluginContext,
    activities: &[Activity],
    expected_ms: i64,
    locale: ServerLocale,
    before_hours: i64,
) {
    let group = activities.to_vec();
    let scheduled = ctx.clone();
    insert_alert_job(
        ctx.framework(),
        expected_ms,
        locale,
        before_hours,
        Arc::new(move || {
            let activities = group.clone();
            let inner = scheduled.clone();
            // 排定时刻与实际触发时刻的差，是这一族「推送晚了」问题唯一能落地的现场证据。
            // 调度器本身毫秒级准（框架侧 single_at_fires_on_the_scheduled_moment 钉着），
            // 所以这个差值只可能来自「派生出去的任务在排队」或「发送链路里被阻塞」——
            // 下面两个量分别量这两段，下次再迟到就能一眼看出落在哪一段，不必再靠猜。
            let drift = chrono::Utc::now().timestamp_millis() - expected_ms;
            if drift > 60_000 {
                arona::runtime::log::warning(format!(
                    "活动预警触发迟到 {} 分（排定时刻 {expected_ms}），{} 条待发送",
                    drift / 60_000,
                    activities.len()
                ));
            }
            let began = chrono::Utc::now().timestamp_millis();
            scheduled.spawn_as("活动预警", async move {
                send_alert(inner.framework(), &activities, locale, before_hours).await;
                let took = chrono::Utc::now().timestamp_millis() - began;
                if took > 60_000 {
                    arona::runtime::log::warning(format!(
                        "活动预警从触发到发完耗时 {} 分：触发是准点的，卡在发送链路上",
                        took / 60_000
                    ));
                }
            });
        }),
    );
}

/// 排期那一层（收 `&Framework`，任务体由调用方给）：重复调度时同 key 直接跳过
fn insert_alert_job(
    framework: &Framework,
    expected_ms: i64,
    locale: ServerLocale,
    before_hours: i64,
    body: JobFn,
) {
    let key = alert_key(locale, expected_ms, before_hours);
    let jobs = framework.jobs();
    if jobs.exists(&key) {
        return;
    }
    jobs.create_single_at(expected_ms, &key, crate::PLUGIN_ID, body);
}

/// 活动到期后 5 分钟刷新本地资源图片: 对每个尚未到期的结束时刻各安排一个单次任务
fn schedule_image_refresh(ctx: &PluginContext, activities: &[Activity], locale: ServerLocale) {
    let now = chrono::Utc::now().timestamp_millis();
    let mut refresh_times: Vec<i64> = Vec::new();
    for activity in activities {
        if activity.end_time <= 0 || activity.activity_type == ActivityType::Birthday {
            continue;
        }
        let refresh_at = activity.end_time + IMAGE_REFRESH_AFTER_END_MILLIS;
        if refresh_at > now && !refresh_times.contains(&refresh_at) {
            refresh_times.push(refresh_at);
        }
    }
    for refresh_at in refresh_times {
        insert_image_refresh(ctx, refresh_at, locale);
    }
}

/// 创建单次定时任务用于刷新本地资源图片; 已存在同 key 任务则跳过, 避免重复
fn insert_image_refresh(ctx: &PluginContext, expected_ms: i64, locale: ServerLocale) {
    let scheduled = ctx.clone();
    insert_image_refresh_job(
        ctx.framework(),
        expected_ms,
        locale,
        Arc::new(move || {
            scheduled.spawn_as("刷新活动图", async move {
                crate::commands::activity::refresh_image(locale).await;
            });
        }),
    );
}

/// 排期那一层（收 `&Framework`，任务体由调用方给）：重复调度时同 key 直接跳过
fn insert_image_refresh_job(
    framework: &Framework,
    expected_ms: i64,
    locale: ServerLocale,
    body: JobFn,
) {
    let key = image_refresh_key(locale, expected_ms);
    let jobs = framework.jobs();
    if jobs.exists(&key) {
        return;
    }
    jobs.create_single_at(expected_ms, &key, crate::PLUGIN_ID, body);
}

/// 发送活动结束预警(定时任务与立即发送共用)
async fn send_alert(
    framework: &Framework,
    activity: &[Activity],
    locale: ServerLocale,
    before_hours: i64,
) {
    if !framework.runtime_services().sender_ready() {
        return;
    }
    let config = config::notify(framework);
    let targets = resolve_targets(&config, framework.gating().groups());
    if targets.is_empty() {
        return;
    }
    let mut remaining: Vec<Activity> = activity.to_vec();
    let maintenance: Option<Activity> = {
        let found: Vec<Activity> = remaining
            .iter()
            .filter(|it| is_maintenance(it))
            .cloned()
            .collect();
        if found.is_empty() {
            None
        } else {
            remaining.retain(|it| !is_maintenance(it));
            found.first().cloned()
        }
    };
    let server_name = locale.server_name();
    let mut texts: Vec<String> = Vec::new();
    if let Some(_maintenance) = maintenance {
        texts.push(format!("距离{server_name}维护还有{before_hours}小时"));
    }
    if !remaining.is_empty() {
        let mut body = String::new();
        for item in &remaining {
            body.push_str(&format!("{}\n", item.content));
        }
        texts.push(format!(
            "{}({server_name})\n{body}将会在{before_hours}小时后结束",
            config.notify_text
        ));
    }
    for text in texts {
        let message = OutgoingMessage::text(text);
        for &group_id in &targets {
            let receipt = arona::plugin::action_async(
                "发送消息",
                framework
                    .runtime_services()
                    .send_message(MessageTarget::Group(group_id), message.clone()),
            )
            .await;
            if receipt.message_id.is_none() {
                arona::runtime::log::warning(format!("预警发送到群 {group_id} 失败"));
            }
        }
    }
}

fn is_maintenance(activity: &Activity) -> bool {
    activity.activity_type == ActivityType::Maintenance
}

#[cfg(test)]
mod tests {
    use super::*;
    use arona::config::arona::typed_section;
    use arona::runtime::message::{
        BoxFuture, MessageReceipt, MessageSegment, MessageSender, MessageTarget,
    };
    use chrono::{Local, Timelike};
    use std::sync::Mutex;
    use std::time::Duration;

    /// 捕获用的假发送器：记下每条本该发出去的消息，不碰网络
    struct CaptureSender {
        sent: Mutex<Vec<(MessageTarget, OutgoingMessage)>>,
    }

    impl CaptureSender {
        fn new() -> CaptureSender {
            CaptureSender {
                sent: Mutex::new(Vec::new()),
            }
        }

        fn take(&self) -> Vec<(MessageTarget, OutgoingMessage)> {
            self.sent.lock().unwrap().drain(..).collect()
        }
    }

    impl MessageSender for CaptureSender {
        fn send<'a>(
            &'a self,
            target: MessageTarget,
            message: OutgoingMessage,
        ) -> BoxFuture<'a, MessageReceipt> {
            Box::pin(async move {
                self.sent.lock().unwrap().push((target, message));
                MessageReceipt::new(Some(1))
            })
        }
    }

    fn message_text(message: &OutgoingMessage) -> String {
        message
            .segments
            .iter()
            .filter_map(|segment| match segment {
                MessageSegment::Text(text) => Some(text.clone()),
                _ => None,
            })
            .collect::<Vec<String>>()
            .join("")
    }

    fn activity(kind: ActivityType, end_ms: i64, content: &str) -> Activity {
        Activity {
            content: content.to_string(),
            time: String::new(),
            activity_type: kind,
            server: ServerLocale::JP,
            start_time: end_ms - 7 * 24 * 3600 * 1000,
            end_time: end_ms,
        }
    }

    /// 任务体占位：这些用例验的是分组、排期与发送，不验任务体真去干什么
    fn no_op() -> JobFn {
        Arc::new(|| {})
    }

    /// 每日推送名额：同一天只放行一次，换一天才重新放行
    #[test]
    fn daily_push_day_is_claimed_once_per_day() {
        let state = AtomicI64::new(0);
        assert!(claim_daily_push_day(&state, 20261009), "当天首触发应放行");
        assert!(
            !claim_daily_push_day(&state, 20261009),
            "同一天第二次触发应被吃掉"
        );
        assert!(claim_daily_push_day(&state, 20261010), "换一天应重新放行");
        assert!(!claim_daily_push_day(&state, 20261010));
    }

    /// 当天名额可以主动让出：手动重推（/任务 触发）靠的就是这一下
    #[test]
    fn released_day_can_be_claimed_again() {
        let state = AtomicI64::new(0);
        assert!(claim_daily_push_day(&state, 20261009));
        assert!(!claim_daily_push_day(&state, 20261009));
        release_daily_push_day(&state);
        assert!(
            claim_daily_push_day(&state, 20261009),
            "让出名额后当天应能再推一次"
        );
    }

    /// 目标群去重：同一个群在名单里出现两次也只发一遍，黑名单照旧剔除
    #[test]
    fn targets_are_deduplicated_and_blacklisted() {
        let config = NotifyConfig {
            black_groups: vec![3],
            ..NotifyConfig::default()
        };
        assert_eq!(
            resolve_targets(&config, vec![1, 2, 2, 3, 4, 1]),
            vec![1, 2, 4],
            "重复目标应去重、黑名单应剔除、次序保持"
        );
    }

    /// 造一套**独立的框架实例**给用例用：配置区登记表、插件配置文件、允许群名单、
    /// 消息发送器全挂在它身上，不碰进程默认实例——所以用例之间真并行，不必抢串行锁。
    ///
    /// `install` 阶段由插件自己登记配置区，这里替它登记：不登记的话 `notify` 段会被
    /// 当成未知键丢掉。配置文件落在 `config/bluearchive/arona.yml`（仓库里已 gitignore）。
    fn isolated() -> (Arc<Framework>, Arc<CaptureSender>) {
        let framework = arona::framework::Framework::new();
        framework
            .sections()
            .register(crate::PLUGIN_ID, typed_section::<NotifyConfig>("notify"));
        framework.sections().register(
            crate::PLUGIN_ID,
            typed_section::<crate::config::TrainerConfig>("trainer"),
        );
        let file = framework.configs().config_file(crate::PLUGIN_ID);
        std::fs::create_dir_all(file.parent().expect("插件配置目录")).unwrap();
        std::fs::write(
            &file,
            "notify:\n\
             \x20 enable: true\n\
             \x20 every_day_hour: 8\n\
             \x20 jp: true\n\
             \x20 global: true\n\
             \x20 cn: true\n\
             \x20 black_groups: [910002]\n\
             \x20 notify_text: \"碧蓝档案预警\"\n",
        )
        .unwrap();
        framework.configs().init();
        framework.gating().set_groups(vec![910001, 910002]);
        let sender = Arc::new(CaptureSender::new());
        framework
            .runtime_services()
            .set_message_sender(sender.clone());
        (framework, sender)
    }

    /// 预警分组: 除生日外的活动各排 5 小时与 1 小时两档, 维护单列只走 1 小时
    #[test]
    fn alert_plan_splits_normal_drop_and_maintenance() {
        let now = chrono::Utc::now().timestamp_millis();
        let normal = activity(ActivityType::Activity, now + 2 * 3_600_000, "普通活动");
        let drop = activity(ActivityType::PickUp, now + 6 * 3_600_000, "双倍掉落活动");
        let maintenance = activity(ActivityType::Maintenance, now + 2 * 3_600_000, "维护预告");
        let birthday = activity(ActivityType::Birthday, now + 2 * 3_600_000, "角色生日");
        let plan = alert_plan(&[normal, drop, maintenance, birthday]);

        assert_eq!(plan.len(), 3, "预警计划应有 3 组");
        // 第 1 组：除维护/生日外的活动，提前 5 小时
        assert_eq!(plan[0].1, DROP_ACTIVITY_NOTIFY_BEFORE_HOURS);
        assert!(plan[0].0.iter().any(|a| a.content == "普通活动"));
        assert!(plan[0].0.iter().any(|a| a.content == "双倍掉落活动"));
        assert!(
            !plan[0].0.iter().any(|a| a.content == "角色生日"),
            "生日不应预警"
        );
        assert!(
            !plan[0].0.iter().any(|a| a.content == "维护预告"),
            "维护不走 5 小时"
        );
        // 第 2 组：普通活动提前 1 小时
        assert_eq!(plan[1].1, NORMAL_ACTIVITY_NOTIFY_BEFORE_HOURS);
        assert_eq!(plan[1].0.len(), 2);
        // 第 3 组：维护单列，只提前 1 小时
        assert_eq!(plan[2].1, NORMAL_ACTIVITY_NOTIFY_BEFORE_HOURS);
        assert_eq!(plan[2].0.len(), 1);
        assert_eq!(plan[2].0[0].content, "维护预告");
    }

    /// 处置判定: 正负 10 分钟内立即发送, 未来排期, 已过时的丢掉; 同一时刻的一起发
    #[test]
    fn plan_alerts_classifies_now_later_and_expired() {
        let now = chrono::Utc::now().timestamp_millis();
        // 结束前 1 小时正好是 10 分钟后 -> 落在窗口内，立即发
        let soon = activity(ActivityType::Activity, now + 3_600_000 + 60_000, "即将结束");
        // 同一结束时刻的第二条: 与上一条并成一组，不必各发一条
        let same = activity(
            ActivityType::PickUp,
            now + 3_600_000 + 60_000,
            "同时结束的双倍",
        );
        // 100 小时后结束 -> 排期
        let far = activity(
            ActivityType::Activity,
            now + 100 * 3_600_000,
            "很久以后的活动",
        );
        // 早已结束 -> 抛弃
        let expired = activity(
            ActivityType::Activity,
            now - 10 * 3_600_000,
            "早就结束的活动",
        );
        // 生日与没有结束时刻的活动都不该出现在结果里
        let birthday = activity(ActivityType::Birthday, now + 3_600_000 + 60_000, "角色生日");
        let timeless = activity(ActivityType::Activity, 0, "没有结束时刻");

        let plan = plan_alerts(
            &[soon, same, far.clone(), expired, birthday, timeless],
            now,
            NORMAL_ACTIVITY_NOTIFY_BEFORE_HOURS,
        );
        assert_eq!(
            plan.len(),
            2,
            "过期/生日/无结束时刻都不该留下安排: {plan:?}"
        );
        let now_group = plan
            .iter()
            .find_map(|action| match action {
                AlertAction::Now(group) => Some(group.clone()),
                _ => None,
            })
            .expect("窗口内的活动没有判成立即发送");
        assert_eq!(now_group.len(), 2, "同一时刻的活动应并成一组");
        let at = plan
            .iter()
            .find_map(|action| match action {
                AlertAction::At(at, group) => Some((*at, group.len())),
                _ => None,
            })
            .expect("未来的活动没有判成排期");
        assert_eq!(at.0, far.end_time - 3_600_000, "排期时刻应为结束前 1 小时");
        assert_eq!(at.1, 1);
    }

    /// 推送目标: 全局允许名单去掉黑名单
    #[test]
    fn targets_drop_blacklisted_groups() {
        let (framework, _sender) = isolated();
        let config = config::notify(&framework);
        assert_eq!(
            resolve_targets(&config, framework.gating().groups()),
            vec![910001],
            "910002 在黑名单里，应当被排除"
        );
    }

    /// 发送侧: 三种文案与目标群（含黑名单、维护单列）
    #[tokio::test]
    async fn alert_texts_go_to_allowed_groups() {
        let (framework, sender) = isolated();
        let now = chrono::Utc::now().timestamp_millis();

        // 普通活动：带推送前缀文案与「将会在 N 小时后结束」
        let soon = activity(ActivityType::Activity, now + 3_600_000 + 60_000, "即将结束");
        send_alert(
            &framework,
            &[soon],
            ServerLocale::JP,
            NORMAL_ACTIVITY_NOTIFY_BEFORE_HOURS,
        )
        .await;
        let sent = sender.take();
        let texts: Vec<String> = sent.iter().map(|(_, m)| message_text(m)).collect();
        assert!(
            texts.iter().any(|t| t.contains("碧蓝档案预警(日服)")
                && t.contains("即将结束")
                && t.contains("将会在1小时后结束")),
            "1 小时预警文案异常: {texts:?}"
        );
        assert!(
            sent.iter()
                .all(|(target, _)| *target == MessageTarget::Group(910001)),
            "预警发送到了黑名单群: {sent:?}"
        );

        // 双倍掉落：提前 5 小时那一档
        let drop_soon = activity(
            ActivityType::PickUp,
            now + 5 * 3_600_000 + 60_000,
            "双倍掉落即将结束",
        );
        send_alert(
            &framework,
            &[drop_soon],
            ServerLocale::JP,
            DROP_ACTIVITY_NOTIFY_BEFORE_HOURS,
        )
        .await;
        let texts: Vec<String> = sender.take().iter().map(|(_, m)| message_text(m)).collect();
        assert!(
            texts
                .iter()
                .any(|t| t.contains("双倍掉落即将结束") && t.contains("将会在5小时后结束")),
            "5 小时预警文案异常: {texts:?}"
        );

        // 维护：单列文案，不带活动清单
        let maintenance_soon = activity(
            ActivityType::Maintenance,
            now + 3_600_000 + 60_000,
            "维护安排",
        );
        send_alert(
            &framework,
            &[maintenance_soon],
            ServerLocale::CN,
            NORMAL_ACTIVITY_NOTIFY_BEFORE_HOURS,
        )
        .await;
        let texts: Vec<String> = sender.take().iter().map(|(_, m)| message_text(m)).collect();
        assert!(
            texts.iter().any(|t| t == "距离国服维护还有1小时"),
            "维护预警文案异常: {texts:?}"
        );
    }

    /// 排期侧: 预警任务与图片刷新任务各自去重，触发时刻分别落在结束前 1 小时与结束后 5 分钟；
    /// 每日推送整点触发，启动初始化任务不重建
    #[tokio::test]
    async fn jobs_dedup_and_fire_at_planned_time() {
        let (framework, _sender) = isolated();
        let jobs = framework.jobs();
        let now = chrono::Utc::now().timestamp_millis();
        let far = activity(
            ActivityType::Activity,
            now + 100 * 3_600_000,
            "很久以后的活动",
        );
        let notify_at = far.end_time - NORMAL_ACTIVITY_NOTIFY_BEFORE_HOURS * 3_600_000;
        let refresh_at = far.end_time + IMAGE_REFRESH_AFTER_END_MILLIS;

        // 同一活动重复调度两次: 两个任务都只留一个
        for _ in 0..2 {
            insert_alert_job(
                &framework,
                notify_at,
                ServerLocale::JP,
                NORMAL_ACTIVITY_NOTIFY_BEFORE_HOURS,
                no_op(),
            );
            insert_image_refresh_job(&framework, refresh_at, ServerLocale::JP, no_op());
        }
        let alert_key = alert_key(
            ServerLocale::JP,
            notify_at,
            NORMAL_ACTIVITY_NOTIFY_BEFORE_HOURS,
        );
        let refresh_key = image_refresh_key(ServerLocale::JP, refresh_at);
        // 单次任务的下次触发时间由任务协程写入, 稍等片刻再读取
        tokio::time::sleep(Duration::from_millis(200)).await;
        let listed = jobs.list();
        assert_eq!(
            listed.iter().filter(|task| task.name == alert_key).count(),
            1,
            "预警定时任务未去重"
        );
        assert!(
            listed.iter().any(|task| task.name == refresh_key),
            "活动到期后未安排本地资源图片刷新任务"
        );
        assert_eq!(
            listed
                .iter()
                .filter(|task| task.name == refresh_key)
                .count(),
            1,
            "本地资源图片刷新任务未去重"
        );
        let refresh = listed
            .into_iter()
            .find(|task| task.name == refresh_key)
            .expect("未找到本地资源图片刷新任务");
        assert_eq!(
            refresh.next_fire_ms,
            Some(refresh_at),
            "本地资源图片刷新时间应为活动到期后 5 分钟"
        );

        // 每日推送 + 启动初始化：重复启用只重建每日任务，初始化任务保持原样
        schedule_jobs(&framework, 8, no_op(), no_op());
        schedule_jobs(&framework, 8, no_op(), no_op());
        assert!(jobs.exists(DAILY_JOB), "每日推送任务未注册");
        tokio::time::sleep(Duration::from_millis(200)).await;
        let info = jobs
            .list()
            .into_iter()
            .find(|task| task.name == DAILY_JOB)
            .expect("未找到每日推送任务");
        let next = info.next_fire_ms.expect("每日推送任务没有下次触发时间");
        let fire_at = chrono::DateTime::from_timestamp_millis(next)
            .expect("时间戳无效")
            .with_timezone(&Local);
        assert_eq!(fire_at.hour(), 8, "每日推送小时不符: {fire_at}");
        assert_eq!(fire_at.minute(), 0);
        assert_eq!(fire_at.second(), 0);
        assert_eq!(
            jobs.list()
                .iter()
                .filter(|task| task.name == INIT_JOB)
                .count(),
            1,
            "启动初始化任务被重复创建"
        );
        println!("[每日推送] 下次触发: {fire_at}");
    }

    /// 端到端：真正执行一次每日推送（联网拉 GameKee 活动 + 渲染日历图 + 发到允许群）。
    /// 推送编排那一层要 `PluginContext`（框架的构造函数是 `pub(crate)`，插件侧造不出来），
    /// 所以这里直接驱动它底下真正发消息的那一步：目标群取实例上的允许名单去掉黑名单。
    #[ignore = "联网自检, 运行: cargo test notify_daily -- --ignored --nocapture --test-threads=1"]
    #[tokio::test]
    async fn notify_daily_push_network() {
        let (framework, sender) = isolated();
        let config = config::notify(&framework);
        let targets = resolve_targets(&config, framework.gating().groups());
        let servers = enabled_servers(&config);
        for (server, pair) in fetch_all(&servers).await {
            let prefix = format!("{}({})", config.notify_text, server.server_name());
            push_server(&framework, server, &pair, &prefix, &targets).await;
        }

        let sent = sender.take();
        println!("[每日推送] 共发送 {} 条", sent.len());
        for (target, message) in &sent {
            println!("[每日推送] -> {target:?}");
            for segment in &message.segments {
                match segment {
                    MessageSegment::Text(text) => println!("   文本: {text}"),
                    MessageSegment::Image { file, .. } => {
                        if let Some(file) = file {
                            assert!(
                                std::path::Path::new(file).exists(),
                                "推送图片不存在: {file}"
                            );
                            println!("   图片: {file}");
                        }
                    }
                    other => println!("   其它: {other:?}"),
                }
            }
        }
        assert!(
            !sent.is_empty(),
            "每日推送未发送任何消息（可能未配置目标群或拉取失败）"
        );
        assert!(
            sent.iter()
                .all(|(target, _)| *target == MessageTarget::Group(910001)),
            "推送发送到了黑名单群"
        );
    }
}
