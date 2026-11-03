//! 活动日历图渲染（对应原版 util/ActivityUtil.createActivityImage + ImageUtil）
//! 白底排版：标题 + 日期、正在进行/即将开始 两个分组；每个活动一行圆角色块，
//! 底色按 activity_type.level 映射（橙/紫/蓝/红/绿 五档），白字左对齐，
//! 右侧显示“今天/明天/后天 X点 开始/结束”的易读时间，底部标注数据来源。
//! 字体不可用时渲染返回 Err，由命令层回退为文本日历。
//! 产物为固定文件名 activity-<服务>.png, 属于「本地资源图片」: 由每日 0 点与活动到期后
//! 5 分钟的定时任务刷新(见 commands::activity), /活动 命中本地图片时直接发送。

use crate::data::activity::type_level;
use crate::entity::{Activity, ActivityType, ServerLocale};
use crate::image::draw;
use crate::image::text;
use crate::util::time::translate_readable_time;
use image::RgbaImage;
use std::path::PathBuf;

const TITLE_SIZE: f32 = 46.0;
const DATE_SIZE: f32 = 26.0;
const SECTION_SIZE: f32 = 34.0;
const CONTENT_SIZE: f32 = 30.0;
const TIME_SIZE: f32 = 23.0;
const FOOTER_SIZE: f32 = 22.0;
const PAD_X: i32 = 30;
const PAD_Y: i32 = 34;
const MIN_WIDTH: i32 = 620;
const MAX_WIDTH: i32 = 1800;

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
    let mut active = active.clone();
    let mut pending = pending.clone();
    active.sort_by_key(|a| a.end_time);
    pending.sort_by_key(|a| a.start_time);

    // 生日单独摘出来置顶。它们的类型本来就是独立的 `Birthday`，但一直混在「即将开始」
    // 里，被一堆普通活动埋掉——用户要的是「生日在最上面、活动在下面」。
    // 两批都收：窗口是未来一周，生日只会落在 pending 里，但真跨到 active 也不能漏。
    let (mut birthdays, active): (Vec<Activity>, Vec<Activity>) = active
        .into_iter()
        .partition(|activity| activity.activity_type == ActivityType::Birthday);
    let (pending_birthdays, pending): (Vec<Activity>, Vec<Activity>) = pending
        .into_iter()
        .partition(|activity| activity.activity_type == ActivityType::Birthday);
    birthdays.extend(pending_birthdays);
    birthdays.sort_by_key(|activity| activity.start_time);

    // 没生日就整节不画：硬摆一行「学生生日 / 无」比不画更难看
    let birthday_rows = collect_birthday_rows(&birthdays);
    let active_rows = collect_rows(&active, false);
    let pending_rows = collect_rows(&pending, true);

    let title = format!("{}活动日历", server.server_name());
    let date = chrono::Local::now().format("%Y/%m/%d").to_string();
    let footer = "数据来源: https://ba.gamekee.com/";

    // 画布宽度: 取标题与最宽“内容+时间”行的自然宽度, 限制在 MIN..MAX
    let mut widest = text::measure(&fonts, &title, TITLE_SIZE) as i32;
    widest = widest.max(text::measure(&fonts, &date, DATE_SIZE) as i32);
    for row in active_rows.iter().chain(pending_rows.iter()) {
        widest = widest.max(row.content_width(&fonts));
    }
    widest = widest.max(text::measure(&fonts, footer, FOOTER_SIZE) as i32);
    let width = (widest + PAD_X * 2).clamp(MIN_WIDTH, MAX_WIDTH);

    // 画布高度 = 按绘制顺序累加各元素盒高
    let mut height = PAD_Y;
    height += line_box(&fonts, TITLE_SIZE);
    height += line_box(&fonts, DATE_SIZE) + 10;
    height += draw_sections_preview(&fonts, &birthday_rows, &active_rows, &pending_rows, width);
    height += line_box(&fonts, FOOTER_SIZE) + PAD_Y;

    let mut img = RgbaImage::new(width as u32, height.max(120) as u32);
    draw::fill_rect(&mut img, 0, 0, width, height, draw::rgb(255, 255, 255));

    let mut y = PAD_Y as f32;
    // 标题（居中）
    let title_h = line_box(&fonts, TITLE_SIZE);
    draw_line_centered(&fonts, &mut img, &title, y, title_h, TITLE_SIZE, INK);
    y += title_h as f32;
    // 日期（右对齐）
    let date_h = line_box(&fonts, DATE_SIZE);
    draw_line_right(&fonts, &mut img, &date, width, y, date_h, DATE_SIZE, GRAY);
    y += date_h as f32 + 10.0;
    // 学生生日（置顶）：这一节是用户明确要的「生日在最上面」
    if !birthday_rows.is_empty() {
        y = draw_section_rows(&fonts, &mut img, y, width, "学生生日", &birthday_rows);
    }
    // 正在进行
    y = draw_section_rows(&fonts, &mut img, y, width, "正在进行", &active_rows);
    // 即将开始
    y = draw_section_rows(&fonts, &mut img, y, width, "即将开始", &pending_rows);
    // 数据来源
    let footer_h = line_box(&fonts, FOOTER_SIZE);
    draw_line_left(&fonts, &mut img, footer, y, footer_h, FOOTER_SIZE, GRAY);

    let file = image_path(server);
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    img.save(&file)
        .map_err(|err| format!("保存活动图片失败: {err}"))?;
    Ok(file)
}

/// 本地活动日历图路径: data/bluearchive/image/activity/activity-<服务>.png
pub fn image_path(server: ServerLocale) -> PathBuf {
    crate::image_dir()
        .join("activity")
        .join(format!("activity-{}.png", server.command_name()))
}

/// 本地已刷新好的活动日历图: 存在即直接复用(命令层不再重新渲染), 不存在返回 None
pub fn cached_image(server: ServerLocale) -> Option<PathBuf> {
    let file = image_path(server);
    file.exists().then_some(file)
}

const INK: draw::Color = [40, 44, 52, 255];
const GRAY: draw::Color = [120, 124, 132, 255];
const WHITE: draw::Color = [255, 255, 255, 255];

/// 单个活动展示行
struct Row {
    content: String,
    time_text: String,
    level: i32,
    /// 指定底色（生日用它的粉）；`None` 时按 `level` 从活动色板取
    color: Option<draw::Color>,
}

impl Row {
    fn content_width(&self, fonts: &text::Fonts) -> i32 {
        let content_w = text::measure(fonts, &self.content, CONTENT_SIZE) as i32;
        let time_w = if self.time_text.is_empty() {
            0
        } else {
            text::measure(fonts, &self.time_text, TIME_SIZE) as i32 + 30
        };
        content_w + time_w + PAD_X
    }

    fn box_height(&self, fonts: &text::Fonts) -> i32 {
        let line = line_box(fonts, CONTENT_SIZE).max(line_box(fonts, TIME_SIZE));
        line + 16
    }
}

fn collect_rows(activities: &[Activity], future: bool) -> Vec<Row> {
    if activities.is_empty() {
        return vec![Row {
            content: "无".to_string(),
            time_text: String::new(),
            level: 1,
            color: None,
        }];
    }
    activities
        .iter()
        .map(|activity| {
            let ts = if future {
                activity.start_time
            } else {
                activity.end_time
            };
            Row {
                content: activity.content.clone(),
                time_text: translate_readable_time(ts, future),
                level: type_level(activity.activity_type),
                color: None,
            }
        })
        .collect()
}

/// 生日的底色：和活动那五档（橙/紫/蓝/红/绿）分开，一眼能看出"这条不是活动"
const BIRTHDAY_COLOR: draw::Color = [255, 122, 168, 255];

/// 生日行：右侧写具体日期（11/05）而不是「明天」——生日问的是哪一天，
/// 不是还剩几个小时，相对时间在这里反而要多想一步。
///
/// 名字去掉「的生日」后缀：这一节标题已经写着「学生生日」，每条再重复一遍就只是噪音。
fn collect_birthday_rows(birthdays: &[Activity]) -> Vec<Row> {
    use chrono::{Datelike, TimeZone};
    birthdays
        .iter()
        .map(|activity| {
            let date = chrono::Local
                .timestamp_millis_opt(activity.start_time)
                .single();
            let time_text = date
                .map(|date| format!("{:02}/{:02}", date.month(), date.day()))
                .unwrap_or_default();
            let content = activity
                .content
                .strip_suffix("的生日")
                .unwrap_or(&activity.content)
                .to_string();
            Row {
                content,
                time_text,
                level: type_level(activity.activity_type),
                color: Some(BIRTHDAY_COLOR),
            }
        })
        .collect()
}

/// 单行文本的盒高（行距 + 上下留白）
fn line_box(fonts: &text::Fonts, size: f32) -> i32 {
    let line = text::line_metrics(fonts, size)
        .map(|(_, _, new_line)| new_line)
        .unwrap_or(size * 1.3);
    line.ceil() as i32
}

/// 预览高度: 各分组标题 + 所有行盒高之和
fn draw_sections_preview(
    fonts: &text::Fonts,
    birthday_rows: &[Row],
    active_rows: &[Row],
    pending_rows: &[Row],
    width: i32,
) -> i32 {
    let section_h = line_box(fonts, SECTION_SIZE) + 12;
    let mut total = 0;
    // 只有生日那一节可能为空（没生日就不画），活动两节空时 collect_rows 会给一行「无」，
    // 标题照旧要留位置——这里按「rows 为空才跳过标题」处理，两种情况都对
    for rows in [birthday_rows, active_rows, pending_rows] {
        if rows.is_empty() {
            continue;
        }
        total += section_h;
        for row in rows {
            let _ = width;
            total += row.box_height(fonts) + 8;
        }
    }
    total
}

/// 绘制一个分组（标题 + 色块行），返回下一元素起点 y
fn draw_section_rows(
    fonts: &text::Fonts,
    img: &mut RgbaImage,
    y: f32,
    width: i32,
    label: &str,
    rows: &[Row],
) -> f32 {
    let mut cursor = y;
    // 分组标题: 左侧色条 + 深色文字
    let section_h = line_box(fonts, SECTION_SIZE);
    draw::fill_rect(
        img,
        PAD_X - 6,
        cursor as i32 + 4,
        8,
        section_h - 8,
        draw::rgb(90, 150, 255),
    );
    draw_line_left(fonts, img, label, cursor, section_h, SECTION_SIZE, INK);
    cursor += section_h as f32 + 12.0;
    for row in rows {
        let block_h = row.box_height(fonts);
        draw_activity_block(fonts, img, row, width, cursor, block_h);
        cursor += block_h as f32 + 8.0;
    }
    cursor
}

fn draw_activity_block(
    fonts: &text::Fonts,
    img: &mut RgbaImage,
    row: &Row,
    width: i32,
    y: f32,
    block_h: i32,
) {
    if row.content == "无" {
        let line_h = line_box(fonts, CONTENT_SIZE).max(line_box(fonts, TIME_SIZE));
        let top = y + (block_h as f32 - line_h as f32) / 2.0;
        draw_line_left_at(
            fonts,
            img,
            "无",
            PAD_X as f32,
            top,
            line_h as f32,
            CONTENT_SIZE,
            GRAY,
        );
        return;
    }
    // 内容超出可用宽度时从尾部裁剪并加省略号（保留右侧时间）
    let mut content = row.content.clone();
    let time_w = if row.time_text.is_empty() {
        0
    } else {
        text::measure(fonts, &row.time_text, TIME_SIZE) as i32 + 34
    };
    let available = width - PAD_X * 2 - time_w;
    if text::measure(fonts, &content, CONTENT_SIZE) as i32 > available {
        while !content.is_empty() && text::measure(fonts, &content, CONTENT_SIZE) as i32 > available
        {
            content.pop();
        }
        content.push('…');
    }
    // 整行圆角色块
    draw::fill_rounded(
        img,
        PAD_X - 10,
        y as i32,
        width - (PAD_X - 10) * 2,
        block_h,
        22,
        row.color.unwrap_or_else(|| level_color(row.level)),
    );
    let box_top = y
        + (block_h as f32 - line_box(fonts, CONTENT_SIZE).max(line_box(fonts, TIME_SIZE)) as f32)
            / 2.0;
    let line_h = line_box(fonts, CONTENT_SIZE).max(line_box(fonts, TIME_SIZE));
    draw_line_left_at(
        fonts,
        img,
        &content,
        PAD_X as f32,
        box_top,
        line_h as f32,
        CONTENT_SIZE,
        WHITE,
    );
    if !row.time_text.is_empty() {
        let time_w = text::measure(fonts, &row.time_text, TIME_SIZE);
        draw_line_at(
            fonts,
            img,
            &row.time_text,
            width as f32 - PAD_X as f32 - time_w,
            box_top,
            line_h as f32,
            TIME_SIZE,
            WHITE,
        );
    }
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

// ---- 文字排版（top 为文本盒顶, 盒内按行高垂直居中） ----

fn draw_line_left(
    fonts: &text::Fonts,
    img: &mut RgbaImage,
    s: &str,
    top: f32,
    box_h: i32,
    size: f32,
    color: draw::Color,
) {
    draw_line_left_at(fonts, img, s, PAD_X as f32, top, box_h as f32, size, color);
}

// 绘图入参就是目标区域几何 + 字号 + 颜色，加上这一轮的字面天然超 7 个
#[allow(clippy::too_many_arguments)]
fn draw_line_left_at(
    fonts: &text::Fonts,
    img: &mut RgbaImage,
    s: &str,
    x: f32,
    top: f32,
    box_h: f32,
    size: f32,
    color: draw::Color,
) {
    draw_line_at(fonts, img, s, x, top, box_h, size, color);
}

// 同上：区域几何 + 字号 + 颜色，再加这一轮的字面
#[allow(clippy::too_many_arguments)]
fn draw_line_right(
    fonts: &text::Fonts,
    img: &mut RgbaImage,
    s: &str,
    width: i32,
    top: f32,
    box_h: i32,
    size: f32,
    color: draw::Color,
) {
    let text_w = text::measure(fonts, s, size);
    draw_line_at(
        fonts,
        img,
        s,
        width as f32 - PAD_X as f32 - text_w,
        top,
        box_h as f32,
        size,
        color,
    );
}

fn draw_line_centered(
    fonts: &text::Fonts,
    img: &mut RgbaImage,
    s: &str,
    top: f32,
    box_h: i32,
    size: f32,
    color: draw::Color,
) {
    let text_w = text::measure(fonts, s, size);
    draw_line_at(
        fonts,
        img,
        s,
        (img.width() as f32 - text_w) / 2.0,
        top,
        box_h as f32,
        size,
        color,
    );
}

// 同上：落点几何 + 字号 + 颜色，再加这一轮的字面
#[allow(clippy::too_many_arguments)]
fn draw_line_at(
    fonts: &text::Fonts,
    img: &mut RgbaImage,
    s: &str,
    x: f32,
    top: f32,
    box_h: f32,
    size: f32,
    color: draw::Color,
) {
    let baseline = top + box_h / 2.0 + size * 0.42;
    text::draw_line(fonts, img, s, x, baseline, size, color);
}
