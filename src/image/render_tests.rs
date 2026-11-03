//! 仅测试环境使用的图片渲染探针与像素级自检：
//! 用合成数据生成抽卡结果图/活动日历图并核对关键区域颜色，
//! 运行: cargo test render -- --nocapture

use crate::entity::{Activity, ActivityType, ServerLocale};
use image::RgbaImage;

/// 统计区域内接近指定颜色的像素数
fn count_color(img: &RgbaImage, r: u8, g: u8, b: u8, tol: u8) -> usize {
    let mut count = 0usize;
    for pixel in img.pixels() {
        let [pr, pg, pb, _] = pixel.0;
        if pr.abs_diff(r) <= tol && pg.abs_diff(g) <= tol && pb.abs_diff(b) <= tol {
            count += 1;
        }
    }
    count
}

/// 某块区域里有没有深色像素（用来确认格子里真的画了日期数字）
fn has_ink(img: &RgbaImage, x0: u32, y0: u32, w: u32, h: u32) -> bool {
    (y0..(y0 + h).min(img.height())).any(|y| {
        (x0..(x0 + w).min(img.width())).any(|x| {
            let [r, g, b, _] = img.get_pixel(x, y).0;
            // 工作日的日期是深色，今天那一格是粉色，两种都算「有字」
            let dark = r < 120 && g < 120 && b < 130;
            let pink = r > 200 && g < 170 && (120..210).contains(&b);
            dark || pink
        })
    })
}

fn sample_activities() -> (Vec<Activity>, Vec<Activity>) {
    let now = chrono::Utc::now().timestamp_millis();
    let mut active = Vec::new();
    let mut pending = Vec::new();
    let specs = [
        ("总力战: ビナー", ActivityType::DecisiveBattle, true),
        ("【复刻】沙漠の狐の対抗戦", ActivityType::Activity, true),
        ("学院交流会 3rd", ActivityType::CollegeExchangeDrop, true),
        ("特殊任务 经验值2倍", ActivityType::SpecialDrop, true),
        ("常规维护", ActivityType::Maintenance, false),
        ("砂狼シロコ的生日", ActivityType::Birthday, true),
        ("PickUp: 星野(水着)", ActivityType::PickUp, false),
    ];
    for (index, (name, kind, is_active)) in specs.iter().enumerate() {
        // 即将开始的排到未来几天：网格只挂「当天开始」的活动，全塞在过去就测不到标签
        let start = if *is_active {
            now - 86_400_000 * (index as i64 % 3)
        } else {
            now + 86_400_000 * (index as i64 + 1)
        };
        let end = start + 86_400_000 * 4;
        let activity = Activity::new(name.to_string(), ServerLocale::JP, start, end, *kind);
        if *is_active {
            active.push(activity);
        } else {
            pending.push(activity);
        }
    }
    (active, pending)
}

fn save_preview(path: &std::path::Path, width: u32) -> std::path::PathBuf {
    let preview = path.with_extension("preview.png");
    let image = image::open(path).expect("读取图片失败");
    let scaled = image.resize(
        width,
        width * image.height() / image.width(),
        image::imageops::FilterType::Triangle,
    );
    scaled.save(&preview).expect("保存预览失败");
    preview
}

#[ignore = "渲染压测(需系统中文字体), 运行: cargo test render_stress -- --ignored --nocapture"]
#[test]
fn render_stress_probe() {
    assert!(
        crate::image::text::acquire().is_some(),
        "本机未找到可用中文字体"
    );
    // 同一天塞 5 个活动：格子高度必须按「超出 3 个就加高」自己长上去，
    // 标签也不能被裁出格子（用户报过的「塞不下」就是这一档）
    let now = chrono::Utc::now().timestamp_millis();
    let day = now + 86_400_000 * 2;
    let specs = [
        ("【特别委托】道具掉落量3倍", ActivityType::SpecialDrop),
        ("佩洛洛斯拉 • 野外战", ActivityType::DecisiveBattle),
        (
            "我等乃神秘学研究会！~学院之谜与古老咒文~",
            ActivityType::Activity,
        ),
        ("常规维护（预计 5 小时）", ActivityType::Maintenance),
        ("PickUp: 星野(水手服)", ActivityType::PickUp),
    ];
    let pending: Vec<Activity> = specs
        .iter()
        .map(|(name, kind)| {
            Activity::new(
                name.to_string(),
                ServerLocale::CN,
                day,
                day + 86_400_000 * 5,
                *kind,
            )
        })
        .collect();
    let pair = (Vec::new(), pending);
    let path = crate::image::activity::render(&pair, ServerLocale::CN).expect("压测渲染失败");
    println!("stress image: {}", path.display());
    let img = image::open(&path).expect("读取压测图失败").to_rgba8();
    // 5 个活动 → 基础 3 格 + 2 格，格子约 364 高，总高必然过千
    assert!(img.height() > 950, "格子没随活动数加高：{}", img.height());
}

#[ignore = "渲染冒烟自检(需系统中文字体), 运行: cargo test render -- --ignored"]
#[test]
fn render_probe() {
    assert!(
        crate::image::text::acquire().is_some(),
        "本机未找到可用中文字体"
    );
    let pair = sample_activities();
    let path = crate::image::activity::render(&pair, ServerLocale::JP).expect("活动图渲染失败");
    println!("activity image: {}", path.display());
    let preview = save_preview(&path, 900);
    println!("activity preview: {}", preview.display());
    let img = image::open(&path).expect("读取活动图失败").to_rgba8();
    // 背景换成天蓝渐变了（对齐 bili 推送卡的 #d3edfa）：顶边应明显偏蓝
    let top = img.get_pixel(img.width() / 2, 2);
    assert!(
        top[2] as i32 > top[0] as i32 + 20,
        "背景不是天蓝渐变：{top:?}"
    );
    // 日期格子是白底
    assert!(
        count_color(&img, 255, 255, 255, 6) > 40_000,
        "缺少白色日期格子"
    );
    // 粉色：今天的高亮边框 + 左栏生日卡
    assert!(
        count_color(&img, 255, 122, 168, 14) > 1_000,
        "缺少粉色（今天/生日）"
    );
    // 活动色条：决战是蓝、掉落是橙
    // 蓝色只有一条色条（6x22）+ 端点是抗锯齿的，阈值取小一点
    assert!(count_color(&img, 16, 126, 247, 12) > 80, "缺少蓝色活动标记");
    assert!(count_color(&img, 255, 140, 0, 12) > 200, "缺少橙色活动标记");
    // 深色标题文字存在
    assert!(count_color(&img, 40, 44, 52, 20) > 200, "缺少标题文字");
    // 7 个格子里都得有日期数字：居中公式写过一次 (`(x+cell_w)/2 + x`)，越靠右偏得越多，
    // 后面几格的数字直接被画出画布——这一条就是那次事故的闸
    let cell_w = (1400 - 44 * 2) / 7;
    for index in 0..7u32 {
        let x0 = (44 + index as i32 * cell_w + 40) as u32;
        assert!(
            has_ink(&img, x0, 158, 100, 60),
            "第 {} 个格子里没有日期数字",
            index + 1
        );
    }
}

#[ignore = "渲染冒烟自检(需系统中文字体), 运行: cargo test render -- --ignored"]
#[tokio::test]
async fn render_gacha_probe() {
    use crate::gacha::{DrawReport, DrawResult};
    let mut results = Vec::new();
    for index in 0..10i64 {
        let star = (index % 3 + 1) as i32;
        results.push(DrawResult {
            star,
            name: format!("学生{}", index + 1),
            desc_name: format!("学生{}", index + 1),
            dev_name: String::new(),
            id: 10000 + index,
            avatar: String::new(),
            custom: index == 0 && star == 3,
            is_pickup: star == 3 && index < 3,
        });
    }
    let report = DrawReport {
        server: ServerLocale::JP,
        times: 10,
        results,
        hit_pickup: true,
        star1: 7,
        star2: 2,
        star3: 1,
        points: 10,
        pity_count: 3,
    };
    let path = crate::image::gacha::render_result(&report)
        .await
        .expect("抽卡图渲染失败");
    println!("gacha image: {}", path.display());
    let preview = save_preview(&path, 880);
    println!("gacha preview: {}", preview.display());
    let img = image::open(&path).expect("读取抽卡图失败").to_rgba8();
    // 背景渐变: 顶部偏蓝(160,213,246), 底部偏白(250,241,241)
    let top = img.get_pixel(20, 10).0;
    assert!(
        top[0].abs_diff(160) < 8 && top[1].abs_diff(213) < 8 && top[2].abs_diff(246) < 8,
        "顶部背景色错误: {top:?}"
    );
    let bottom = img.get_pixel(20, 1060).0;
    assert!(
        bottom[0].abs_diff(250) < 8 && bottom[1].abs_diff(241) < 8 && bottom[2].abs_diff(241) < 8,
        "底部背景色错误: {bottom:?}"
    );
    // 无头像占位渐变(140,180,210)->(96,134,168)盖住头像区
    let cell = img.get_pixel(500, 200).0;
    assert!(
        cell[2] > cell[0] && (90..170).contains(&cell[0]),
        "占位渐变异常: {cell:?}"
    );
    // 星级底板为灰蓝色渐变 (112,128,148)->(76,92,112)
    let plate = img.get_pixel(520, 330).0;
    assert!(
        plate[2] > plate[0] && plate[0] <= 130 && plate[2] >= 100,
        "星级底板颜色异常: {plate:?}"
    );
    // 金色星星像素存在(第一张1星 => 一颗, 位于底板中央附近)
    let gold = count_color(&img, 255, 224, 120, 60);
    assert!(gold > 150, "金色星星像素过少: {gold}");
    // 3星粉紫边框(第三列第一行卡片 index2, x=FIRST_COL+2*282=1032)
    let border = img.get_pixel(1033, 170).0;
    assert!(
        border[0].abs_diff(206) < 30 && border[2].abs_diff(240) < 30,
        "3星边框颜色异常: {border:?}"
    );
    // 右下角保底块为青色渐变并含白色计数文字
    let block = img.get_pixel(1900, 920).0;
    assert!(block[1].abs_diff(190) < 45, "保底块颜色异常: {block:?}");
    let white = count_color(&img, 255, 255, 255, 6);
    assert!(white > 1_000, "白色文字像素过少: {white}");
}

/// 启动刷新自检(联网): refresh_all_images 拉取三服活动并生成三张本地资源图片
#[ignore = "联网自检, 运行: cargo test refresh_all_images -- --ignored --nocapture --test-threads=1"]
#[tokio::test]
async fn refresh_all_images_creates_local_files() {
    assert!(
        crate::image::text::acquire().is_some(),
        "本机未找到可用中文字体"
    );
    crate::commands::activity::refresh_all_images().await;
    let mut produced = 0;
    for server in ServerLocale::ALL {
        match crate::image::activity::cached_image(server) {
            Some(file) => {
                produced += 1;
                assert!(file.exists(), "本地活动图片不存在: {}", file.display());
                println!("[启动刷新] {} -> {}", server.server_name(), file.display());
            }
            None => println!(
                "[启动刷新] {} 未生成图片(可能拉取活动失败)",
                server.server_name()
            ),
        }
    }
    assert_eq!(produced, 3, "应生成 日服/国际服/国服 三张本地活动图片");
}

#[ignore = "联网自检：拉真实活动与生日出图, 运行: cargo test render_live -- --ignored --nocapture"]
#[test]
fn render_live_probe() {
    let runtime = tokio::runtime::Runtime::new().expect("建运行时失败");
    let pair = runtime
        .block_on(crate::data::activity::fetch(ServerLocale::CN))
        .expect("拉取国服活动失败");
    println!("active {} / pending {}", pair.0.len(), pair.1.len());
    let path = crate::image::activity::render(&pair, ServerLocale::CN).expect("渲染失败");
    println!("live image: {}", path.display());
}
