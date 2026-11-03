//! 活动日历图（网格版）：上部 7 天日期网格，下部两栏详情。
//!
//! 版面参考原神那种「日期条 + 事件」的日历：
//! - 顶部 7 个日期格子，今天粉色高亮，有生日的日子点一个粉点；
//! - 格子里挂**当天开始**的活动（进行中的不重复挂，否则每天都一堵色条墙），
//!   每条最多两行，标签底色取该活动类型色的浅版；
//! - 格子基础高度按 3 个活动预留，超过 3 个就按超出个数往上加（多几个加几格）；
//! - 下面左栏「学生生日」、右栏「进行中 / 即将开始」，
//!   时间写成「开始 —— 结束」，开始粉、结束红。
//!
//! 背景是竖直渐变的天蓝（对齐 bili 推送卡的默认底色 `#d3edfa`），不再是纯白。
//! 字体不可用时渲染返回 Err，由命令层回退为文本日历。
//! 产物仍是 `activity-<服务>.png`，每日 8 点推送与 `/活动` 共用这一份。

use crate::data::activity::type_level;
use crate::entity::{Activity, ActivityType, ServerLocale};
use crate::image::draw;
use crate::image::text;
use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone};
use image::RgbaImage;
use std::path::PathBuf;

// ---------- 版面 ----------
const W: i32 = 1400;
const PAD: i32 = 44;
const DAYS: usize = 7;
/// 每个日期格子基础能放几个活动（超出的按个数加高）
const BASE_SLOTS: usize = 3;
/// 一个格子最多摆几个，多出来的折成 `+N`
const MAX_ITEMS: usize = 8;
/// 一个活动预留的高度（两行 + 间距）
const SLOT_H: i32 = 52;
/// 标签一行的高度
const TAG_LINE: i32 = 21;

const S_TITLE: f32 = 40.0;
const S_SUB: f32 = 21.0;
const S_DAY: f32 = 28.0;
const S_WEEK: f32 = 17.0;
const S_SEC: f32 = 26.0;
const S_ROW: f32 = 22.0;
const S_SMALL: f32 = 18.0;
/// 图例字号：用户要求在 18 的基础上再大一倍；色点与间距按比例跟着走（见 [`draw_legend`]）
const S_LEGEND: f32 = 36.0;
const S_TIME: f32 = 20.0;
const S_TAG: f32 = 15.0;

const TITLE_TOP: i32 = 40;
const GRID_TOP: i32 = 158;

const INK: draw::Color = [40, 44, 52, 255];
const SUB: draw::Color = [122, 126, 134, 255];
const PINK: draw::Color = [255, 122, 168, 255];
const RED: draw::Color = [226, 74, 74, 255];
const WHITE: draw::Color = [255, 255, 255, 255];
/// 背景渐变两端（对齐 bili 推送卡的 `#d3edfa`）
const BG_TOP: draw::Color = [198, 229, 248, 255];
const BG_BOTTOM: draw::Color = [243, 251, 255, 255];

/// 渲染某服务器活动日历并保存 PNG（在阻塞线程池里跑，避免占住 tokio 工作线程）
pub async fn render_async(
    pair: (Vec<Activity>, Vec<Activity>),
    server: ServerLocale,
) -> Result<PathBuf, String> {
    crate::image::cpu_bound(move || render(&pair, server)).await
}

/// 渲染某服务器活动日历并保存 PNG，返回文件路径
pub fn render(
    pair: &(Vec<Activity>, Vec<Activity>),
    server: ServerLocale,
) -> Result<PathBuf, String> {
    // 字面只在这一次渲染期间存在（口径见 text::acquire 的说明）：拿不到就是本机没有中文字体
    let Some(fonts) = text::acquire() else {
        return Err("未找到可用中文字体, 无法生成活动图片".to_string());
    };
    let (active, pending) = pair;

    // ---------- 数据：生日摘出来、活动按「开始那天」落格 ----------
    let mut birthdays: Vec<&Activity> = Vec::new();
    let mut acts: Vec<&Activity> = Vec::new();
    for activity in active.iter().chain(pending.iter()) {
        if activity.activity_type == ActivityType::Birthday {
            birthdays.push(activity);
        } else {
            acts.push(activity);
        }
    }
    birthdays.sort_by_key(|activity| activity.start_time);

    let today = Local::now().date_naive();
    let days: Vec<NaiveDate> = (0..DAYS)
        .map(|offset| today + Duration::days(offset as i64))
        .collect();
    let window_end = local_millis(days[DAYS - 1] + Duration::days(1));
    let now_ms = Local::now().timestamp_millis();

    let mut per_day: Vec<Vec<&Activity>> = vec![Vec::new(); DAYS];
    for activity in &acts {
        if activity.start_time < now_ms || activity.start_time >= window_end {
            continue;
        }
        if let Some(day) = local_date(activity.start_time)
            && let Some(index) = days.iter().position(|candidate| *candidate == day)
        {
            per_day[index].push(activity);
        }
    }
    for bucket in &mut per_day {
        bucket.sort_by_key(|activity| activity.start_time);
    }

    // 生日也按天归位：格子里要写出名字，不只是点个粉点
    let mut birthday_items: Vec<Vec<&Activity>> = vec![Vec::new(); DAYS];
    for activity in &birthdays {
        if let Some(day) = local_date(activity.start_time)
            && let Some(index) = days.iter().position(|candidate| *candidate == day)
        {
            birthday_items[index].push(activity);
        }
    }

    let running: Vec<&Activity> = acts
        .iter()
        .copied()
        .filter(|activity| activity.start_time <= now_ms && now_ms <= activity.end_time)
        .collect();
    let mut upcoming: Vec<&Activity> = acts
        .iter()
        .copied()
        .filter(|activity| activity.start_time > now_ms && activity.start_time < window_end)
        .collect();
    let mut running = running;
    running.sort_by_key(|activity| activity.end_time);
    upcoming.sort_by_key(|activity| activity.start_time);

    // ---------- 版面 ----------
    let cell_w = (W - PAD * 2) / DAYS as i32;
    // 基础高度按 3 个活动预留；超出多少个就加多少格
    // 生日也占格子里的位置，算高度时一起数
    let busiest = (0..DAYS)
        .map(|index| per_day[index].len() + birthday_items[index].len())
        .max()
        .unwrap_or(0);
    let shown = busiest.min(MAX_ITEMS);
    let extra = shown.saturating_sub(BASE_SLOTS) as i32;
    let cell_h = 92 + BASE_SLOTS as i32 * SLOT_H + extra * SLOT_H + 12;
    let grid_top = GRID_TOP;
    let body_top = grid_top + cell_h + 34;

    let left_w = 392;
    let right_x = PAD + left_w + 28;
    let right_w = W - PAD - right_x;

    let left_h = 42 + birthdays.len().max(1) as i32 * 40;
    let right_h =
        42 + running.len().max(1) as i32 * 46 + 22 + 42 + upcoming.len().max(1) as i32 * 46;
    let body_h = left_h.max(right_h);
    let height = body_top + body_h + 26 + 44;

    let mut img = RgbaImage::from_pixel(W as u32, height as u32, image::Rgba(WHITE));
    // 天蓝渐变打底：整幅铺一层，后面的白卡片再从上面盖
    draw::fill_rect_vgrad(&mut img, 0, 0, W, height, BG_TOP, BG_BOTTOM);

    // ---------- 标题 ----------
    draw_text(
        &fonts,
        &mut img,
        &format!("{}活动日历", server.server_name()),
        PAD as f32,
        TITLE_TOP as f32,
        S_TITLE,
        INK,
    );
    draw_text(
        &fonts,
        &mut img,
        &format!("{} · 未来 {} 天", today.format("%Y/%m/%d"), DAYS),
        PAD as f32,
        (TITLE_TOP + 62) as f32,
        S_SUB,
        SUB,
    );
    draw_legend(&fonts, &mut img, W - PAD, TITLE_TOP + 12);

    // ---------- 日期网格 ----------
    for (index, day) in days.iter().enumerate() {
        let x = PAD + index as i32 * cell_w;
        let is_today = *day == today;
        let weekend = day.weekday().num_days_from_monday() >= 5;
        let (fill, outline, thickness) = if is_today {
            (draw::rgb(255, 231, 241), PINK, 2)
        } else if weekend {
            (draw::rgb(247, 251, 255), draw::rgb(255, 255, 255), 1)
        } else {
            (WHITE, draw::rgb(255, 255, 255), 1)
        };
        draw::fill_rounded(&mut img, x, grid_top, cell_w - 6, cell_h, 12, fill);
        draw::stroke_rounded(
            &mut img,
            x,
            grid_top,
            cell_w - 6,
            cell_h,
            12,
            thickness,
            outline,
        );

        // 日期与周几
        let date_text = format!("{:02}", day.day());
        let date_color = if is_today { PINK } else { INK };
        let date_w = text::measure(&fonts, &date_text, S_DAY);
        let center_x = x as f32 + (cell_w - 6) as f32 / 2.0;
        draw_text(
            &fonts,
            &mut img,
            &date_text,
            center_x - date_w / 2.0,
            (grid_top + 12) as f32,
            S_DAY,
            date_color,
        );
        let weekday = "一二三四五六日"
            .chars()
            .nth(day.weekday().num_days_from_monday() as usize)
            .unwrap_or('一');
        let week = format!("周{weekday}");
        let week_w = text::measure(&fonts, &week, S_WEEK);
        draw_text(
            &fonts,
            &mut img,
            &week,
            center_x - week_w / 2.0,
            (grid_top + 48) as f32,
            S_WEEK,
            SUB,
        );

        // 当天：生日排在最前（粉色），后面才是当天开始的活动
        let mut items: Vec<&Activity> = birthday_items[index].clone();
        items.extend(per_day[index].iter().copied());
        let mut y = grid_top + 88;
        for activity in items.iter().take(MAX_ITEMS) {
            y += draw_tag(&fonts, &mut img, x, y, cell_w - 6, activity);
        }
        if items.len() > MAX_ITEMS {
            draw_text(
                &fonts,
                &mut img,
                &format!("+{}", items.len() - MAX_ITEMS),
                (x + 10) as f32,
                (y + 2) as f32,
                S_WEEK,
                SUB,
            );
        }
    }

    // ---------- 左栏：学生生日 ----------
    let mut ly = body_top;
    draw_section_title(&fonts, &mut img, PAD as f32, ly as f32, "学生生日", PINK);
    ly += 42;
    if birthdays.is_empty() {
        draw_text(
            &fonts,
            &mut img,
            "未来一周没有学生生日",
            PAD as f32,
            ly as f32,
            S_ROW,
            SUB,
        );
    }
    for activity in &birthdays {
        draw::fill_rounded(
            &mut img,
            PAD,
            ly - 4,
            left_w,
            38,
            10,
            draw::rgb(255, 236, 244),
        );
        let (month, day) = local_month_day(activity.start_time);
        draw_text(
            &fonts,
            &mut img,
            &format!("{month:02}/{day:02}"),
            (PAD + 14) as f32,
            (ly + 3) as f32,
            S_ROW,
            PINK,
        );
        draw_text(
            &fonts,
            &mut img,
            strip_birthday_suffix(&activity.content),
            (PAD + 88) as f32,
            (ly + 3) as f32,
            S_ROW,
            INK,
        );
        ly += 40;
    }

    // ---------- 右栏：进行中 / 即将开始 ----------
    let mut ry = body_top;
    ry = draw_activity_section(
        &fonts,
        &mut img,
        right_x,
        ry,
        right_w,
        &format!("进行中 · {}", running.len()),
        &running,
    );
    draw_activity_section(
        &fonts,
        &mut img,
        right_x,
        ry + 22,
        right_w,
        &format!("即将开始 · {}", upcoming.len()),
        &upcoming,
    );

    // ---------- 页脚 ----------
    let footer = format!(
        "数据：GameKee 活动查询 + SchaleDB 学生生日 · 生成于 {}",
        Local::now().format("%Y/%m/%d %H:%M")
    );
    draw_text(
        &fonts,
        &mut img,
        &footer,
        PAD as f32,
        (body_top + body_h + 26) as f32,
        S_SMALL,
        SUB,
    );

    let file = image_path(server);
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    img.save(&file)
        .map_err(|err| format!("保存活动图片失败: {err}"))?;
    cleanup_stale_images(server, &file);
    Ok(file)
}

/// 清掉旧版本命名的历史图。
///
/// 路径里编进了插件版本（见 [`image_path`]）：升级后 `cached_image` 按新版本找文件，
/// 旧图命不中——这正是「改了渲染用户却看不到新图」的解法（v0.4.10→v0.4.11 用户实测踩过：
/// 命令层优先发盘上已有的图，旧图不删就永远是它）。清掉旧版文件，纯粹是不让 `data/` 攒垃圾。
/// 只删「本服务器前缀 + .png 结尾 + 不等于当前文件」的，别的服务器互不相干。
fn cleanup_stale_images(server: ServerLocale, keep: &std::path::Path) {
    let Some(dir) = keep.parent() else {
        return;
    };
    let keep_name = keep
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default()
        .to_string();
    let prefix = format!("activity-{}", server.command_name());
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name == keep_name {
                continue;
            }
            if name.starts_with(&prefix) && name.ends_with(".png") {
                let _ = std::fs::remove_file(entry.path());
            }
        }
    }
}

/// 本地活动日历图路径: `data/bluearchive/image/activity/activity-<服务>-v<插件版本>.png`。
///
/// 文件名带版本是**故意的**：命令层（`commands/activity`）对盘上已有的图直接复用、
/// 不重渲染，由每日定时任务刷新。渲染代码一旦改动，同名的旧图就成了「改了也白改」
/// 的死缓存——版本进了文件名，升级即失效，旧文件由 [`cleanup_stale_images`] 收走。
pub fn image_path(server: ServerLocale) -> PathBuf {
    crate::image_dir().join("activity").join(format!(
        "activity-{}-v{}.png",
        server.command_name(),
        env!("CARGO_PKG_VERSION")
    ))
}

/// 本地已刷新好的活动日历图: 存在即直接复用(命令层不再重新渲染), 不存在返回 None。
/// 文件名带插件版本, 升级后旧图天然命不中(见 [`image_path`] 的说明)。
pub fn cached_image(server: ServerLocale) -> Option<PathBuf> {
    let file = image_path(server);
    file.exists().then_some(file)
}

// ---------- 绘制辅助 ----------

/// 顶部对齐地画一行字（内部换算成基线）
fn draw_text(
    fonts: &text::Fonts,
    img: &mut RgbaImage,
    content: &str,
    x: f32,
    top: f32,
    size: f32,
    color: draw::Color,
) {
    let box_h = line_box(fonts, size);
    draw_line_at(fonts, img, content, x, top, box_h, size, color);
}

fn line_box(fonts: &text::Fonts, size: f32) -> i32 {
    text::line_metrics(fonts, size)
        .map(|(_, _, new_line)| new_line)
        .unwrap_or(size * 1.3)
        .ceil() as i32
}

fn draw_line_at(
    fonts: &text::Fonts,
    img: &mut RgbaImage,
    content: &str,
    x: f32,
    top: f32,
    box_h: i32,
    size: f32,
    color: draw::Color,
) {
    let ascent = text::line_metrics(fonts, size)
        .map(|(a, _, _)| a)
        .unwrap_or(size * 0.8);
    let baseline = top + (box_h as f32 - (ascent + size * 0.25)) / 2.0 + ascent;
    text::draw_line(fonts, img, content, x, baseline, size, color);
}

/// 分组标题：左边一条竖色条 + 深色字
fn draw_section_title(
    fonts: &text::Fonts,
    img: &mut RgbaImage,
    x: f32,
    y: f32,
    label: &str,
    accent: draw::Color,
) {
    draw::fill_rounded(img, x as i32 - 13, y as i32 + 6, 6, 24, 3, accent);
    draw_text(fonts, img, label, x, y, S_SEC, INK);
}

/// 右上角图例：生日 / 总力战 / 掉落 / 维护。
///
/// 字号 [`S_LEGEND`]，色点与间距按字号比例缩放——写死的话字号翻倍后
/// 10px 的小点挂在大字旁边会像标点符号。
fn draw_legend(fonts: &text::Fonts, img: &mut RgbaImage, right: i32, top: i32) {
    let entries: [(&str, draw::Color); 4] = [
        ("生日", PINK),
        ("总力战", draw::rgb(16, 126, 247)),
        ("掉落", draw::rgb(255, 140, 0)),
        ("维护", draw::rgb(245, 108, 108)),
    ];
    let dot = S_LEGEND * 0.55; // 色点直径随字号
    let dot_top = (S_LEGEND - dot) * 0.42; // 对着字面重心，不顶到行框上沿
    let mut x = right as f32;
    for (label, color) in entries.iter().rev() {
        let w = text::measure(fonts, label, S_LEGEND);
        x -= w;
        draw_text(fonts, img, label, x, top as f32, S_LEGEND, SUB);
        x -= S_LEGEND * 0.45;
        draw::fill_rounded(
            img,
            (x - dot) as i32,
            (top as f32 + dot_top) as i32,
            dot as i32,
            dot as i32,
            (dot * 0.5) as i32,
            *color,
        );
        x -= dot + S_LEGEND * 0.45;
    }
}

/// 一个活动标签：类型浅色底 + 色点 + 最多两行文字，返回占用高度
fn draw_tag(
    fonts: &text::Fonts,
    img: &mut RgbaImage,
    x: i32,
    y: i32,
    cell_w: i32,
    activity: &Activity,
) -> i32 {
    let max_w = (cell_w - 44) as f32;
    // 生日用粉、文字去掉「的生日」后缀——格子里寸土寸金，标题那一节已经说了是生日
    let is_birthday = activity.activity_type == ActivityType::Birthday;
    let accent = if is_birthday {
        PINK
    } else {
        level_color(type_level(activity.activity_type))
    };
    let label = if is_birthday {
        strip_birthday_suffix(&activity.content)
    } else {
        activity.content.as_str()
    };
    let lines = wrap2(fonts, label, S_TAG, max_w);
    let block_h = lines.len() as i32 * TAG_LINE + 6;
    // 底用类型色的浅版：白底在渐变背景上分不出类型，一眼扫过去全是白块
    let tint = draw::lerp(accent, WHITE, 0.78);
    draw::fill_rounded(img, x + 7, y, cell_w - 19, block_h, 6, tint);
    draw::fill_rounded(img, x + 11, y + 6, 6, 8, 2, accent);
    for (index, line) in lines.iter().enumerate() {
        draw_text(
            fonts,
            img,
            line,
            (x + 21) as f32,
            (y + 3 + index as i32 * TAG_LINE) as f32,
            S_TAG,
            INK,
        );
    }
    block_h + 4
}

/// 右栏一节：标题 + 若干活动行（色条 / 名称 / 开始粉—结束红），返回下一个 y
fn draw_activity_section(
    fonts: &text::Fonts,
    img: &mut RgbaImage,
    x: i32,
    y: i32,
    width: i32,
    label: &str,
    rows: &[&Activity],
) -> i32 {
    draw_section_title(
        fonts,
        img,
        x as f32,
        y as f32,
        label,
        draw::rgb(61, 139, 253),
    );
    let mut cursor = y + 42;
    if rows.is_empty() {
        draw_text(fonts, img, "暂无", x as f32, cursor as f32, S_ROW, SUB);
        return cursor + 44;
    }
    for activity in rows {
        draw::fill_rounded(img, x, cursor - 4, width, 42, 10, WHITE);
        draw::fill_rounded(
            img,
            x + 12,
            cursor + 6,
            6,
            22,
            3,
            level_color(type_level(activity.activity_type)),
        );
        // 时间：开始粉 — 结束红，右对齐
        let begin = month_day(activity.start_time);
        let end = month_day(activity.end_time);
        let w_b = text::measure(fonts, &begin, S_TIME);
        let w_s = text::measure(fonts, "—", S_TIME);
        let w_e = text::measure(fonts, &end, S_TIME);
        let total = w_b + w_s + w_e + 12.0;
        let rx = (x + width) as f32 - total - 16.0;
        draw_text(fonts, img, &begin, rx, (cursor + 7) as f32, S_TIME, PINK);
        draw_text(
            fonts,
            img,
            "—",
            rx + w_b + 6.0,
            (cursor + 7) as f32,
            S_TIME,
            SUB,
        );
        draw_text(
            fonts,
            img,
            &end,
            rx + w_b + 6.0 + w_s + 6.0,
            (cursor + 7) as f32,
            S_TIME,
            RED,
        );
        // 标题：超宽就截断（时间那一列先占位）
        let title_w = (x + width) as f32 - total - 16.0 - (x + 30) as f32 - 12.0;
        draw_text(
            fonts,
            img,
            &fit_text(fonts, &activity.content, S_ROW, title_w),
            (x + 30) as f32,
            (cursor + 4) as f32,
            S_ROW,
            INK,
        );
        cursor += 46;
    }
    cursor + 10
}

/// 最多两行折行；第二行还放不下就截断加省略号
fn wrap2(fonts: &text::Fonts, content: &str, size: f32, max_w: f32) -> Vec<String> {
    if text::measure(fonts, content, size) <= max_w {
        return vec![content.to_string()];
    }
    let mut first = String::new();
    let mut offset = 0usize;
    for (index, ch) in content.char_indices() {
        let mut candidate = first.clone();
        candidate.push(ch);
        if text::measure(fonts, &candidate, size) > max_w && !first.is_empty() {
            offset = index;
            break;
        }
        first.push(ch);
        offset = index + ch.len_utf8();
    }
    let rest = &content[offset..];
    if text::measure(fonts, rest, size) <= max_w {
        return vec![first, rest.to_string()];
    }
    vec![first, fit_text(fonts, rest, size, max_w)]
}

/// 单行截断：放不下就砍到刚好再加省略号
fn fit_text(fonts: &text::Fonts, content: &str, size: f32, max_w: f32) -> String {
    if text::measure(fonts, content, size) <= max_w {
        return content.to_string();
    }
    let mut out = String::new();
    for ch in content.chars() {
        let mut candidate = out.clone();
        candidate.push(ch);
        candidate.push('…');
        if text::measure(fonts, &candidate, size) > max_w && !out.is_empty() {
            break;
        }
        out.push(ch);
    }
    out.push('…');
    out
}

/// level -> 色块颜色（对应 ActivityColorMap: 1橙 2紫 3蓝 4红 5绿）
fn level_color(level: i32) -> draw::Color {
    match level {
        2 => draw::rgb(138, 43, 226),
        3 => draw::rgb(16, 126, 247),
        4 => draw::rgb(245, 108, 108),
        5 => draw::rgb(103, 194, 58),
        _ => draw::rgb(255, 140, 0),
    }
}

/// 「X的生日」→「X」：这一节的标题已经写着生日，每条再重复一遍只是噪音
fn strip_birthday_suffix(content: &str) -> &str {
    content.strip_suffix("的生日").unwrap_or(content)
}

fn local_date(ms: i64) -> Option<NaiveDate> {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|dt| dt.date_naive())
}

fn local_month_day(ms: i64) -> (u32, u32) {
    Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|dt| (dt.month(), dt.day()))
        .unwrap_or((0, 0))
}

fn month_day(ms: i64) -> String {
    let (month, day) = local_month_day(ms);
    format!("{month:02}/{day:02}")
}

fn local_millis(day: NaiveDate) -> i64 {
    let naive = day.and_hms_opt(0, 0, 0).unwrap_or_default();
    Local
        .from_local_datetime(&naive)
        .earliest()
        .map(|dt| dt.timestamp_millis())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 折行最多两行，且第二行永远不超过给定宽度
    #[test]
    fn wrap_keeps_two_lines_at_most() {
        let Some(fonts) = text::acquire() else {
            return; // 本机没中文字体就跳过（渲染探针另有说明）
        };
        let lines = wrap2(&fonts, "【特别委托】道具掉落量3倍", S_TAG, 143.0);
        assert!(lines.len() <= 2, "{lines:?}");
        for line in &lines {
            assert!(text::measure(&fonts, line, S_TAG) <= 143.0 + 1.0, "{line}");
        }
    }

    /// 短名字不折行
    #[test]
    fn short_names_stay_on_one_line() {
        let Some(fonts) = text::acquire() else {
            return;
        };
        assert_eq!(wrap2(&fonts, "常规维护", S_TAG, 143.0).len(), 1);
    }

    /// 生日后缀只在渲染时去掉，数据本身不动
    #[test]
    fn birthday_suffix_is_only_a_display_concern() {
        assert_eq!(strip_birthday_suffix("白子的生日"), "白子");
        assert_eq!(strip_birthday_suffix("白子"), "白子");
    }
}
