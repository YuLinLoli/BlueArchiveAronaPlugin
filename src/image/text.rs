//! 中文字体与文本绘制（对应原版 java.awt.Font/Graphics2D.drawString）
//!
//! 字面那一层在框架的 `arona::runtime::text` 里：三级解析（配置 / 随包 `fonts/` 的思源宋体 /
//! 系统候选）、按 (字符, 字号) 计的有界字形缓存、`PxScale` 换算、Weak 缓存，全在那一处，
//! 三个出图插件共用。本模块只做这个插件要的：拿字面、量宽、单行绘制与居中绘制。
//!
//! 与另两个插件的差别是**兜底那一级**：本插件会递归扫系统字体目录，逐个文件试"能不能渲染中文"
//! （`Face::renders('活')`），挑第一个命中的——它不像 bilibili/petpet 只认一张固定候选表。
//! 框架那三级都空时才走这条路（见 [`discover`]）。

use crate::image::draw;
use arona::runtime::text::{self as font, Face};
use image::RgbaImage;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// 一次出图要用的字面。框架那份 `Typeface` 的 newtype：本模块的四个函数都收 `&Fonts`，
/// 忘了取字面是编译不过，不是悄悄画一张没字的图。
pub struct Fonts(Arc<font::Typeface>);

impl Fonts {
    fn face(&self) -> &Face {
        // 一律常规：粗体那一档根本没装载，理由见 [`regular_only`]
        self.0.face(false)
    }
}

/// 取字面：框架那一级还有人拿着就复用，没有才去盘上找并装载。
///
/// 回 `None` 就是"本机没有能渲染中文的字体"——调用方要么按无文本出图，要么明确报错，
/// 口径与原来的 `available()` 相同。缓存只存 Weak（框架那层），所以最后一个持有者落下就整个
/// 还给分配器；惰性后端下重装一次是 0.01 s 量级，这一档几乎不要钱。
pub fn acquire() -> Option<Arc<Fonts>> {
    let face = match regular_only() {
        Some(face) => face,
        // 框架那三级都空（安装目录没摆 fonts/、系统候选一个没命中）才去递归扫
        None => discover()?,
    };
    Some(Arc::new(Fonts(face)))
}

/// 只要常规那一档：本插件没有粗体（原版 java.awt 那边也没画过粗体中文），
/// 把候选表里配对的 `msyhbd.ttc` 一起装进来是白占 11 MB。
fn regular_only() -> Option<Arc<font::Typeface>> {
    let (regular, _) = font::resolve("", &[]).ok()?;
    font::Typeface::acquire((regular, None)).ok()
}

/// 文本宽度（px），用于居中/右对齐排版
pub fn measure(fonts: &Fonts, text: &str, size: f32) -> f32 {
    fonts.face().text_width(text, size)
}

/// 单行文字基线指标: (ascent, descent, new_line_size)。
///
/// 口径由框架的 `Face::line_metrics` 保证与 fontdue 一致（hhea 三数乘 `px / units_per_em`，
/// `new_line_size = ascent - descent + line_gap`），这里只把四个字段收成三个。
pub fn line_metrics(fonts: &Fonts, size: f32) -> Option<(f32, f32, f32)> {
    let metrics = fonts.face().line_metrics(size)?;
    Some((metrics.ascent, metrics.descent, metrics.new_line_size))
}

/// 在 (x, baseline_y) 处绘制一行文字（y 轴向下为正），带抗锯齿与 alpha 混合
pub fn draw_line(
    fonts: &Fonts,
    img: &mut RgbaImage,
    text: &str,
    x: f32,
    baseline_y: f32,
    size: f32,
    color: draw::Color,
) {
    let face = fonts.face();
    let mut cursor = x;
    for ch in text.chars() {
        let (glyph, coverage) = face.rasterize(ch, size);
        if glyph.width > 0 && glyph.height > 0 {
            // 位图左上角相对基线/笔位置: x=cursor+xmin, y=baseline-(ymin+height)。
            // `ymin` 是下边界（向上为正），所以顶边就是 `-(ymin + height)`——与换后端之前那一式同解，
            // 差别只是包围盒在框架那一级已经取过整，这里不再对整数 floor。
            let top = -(glyph.ymin + glyph.height as i32);
            let origin_x = (cursor + glyph.xmin as f32).floor() as i32;
            let origin_y = (baseline_y + top as f32).floor() as i32;
            for row in 0..glyph.height {
                for col in 0..glyph.width {
                    let alpha = coverage[row * glyph.width + col];
                    if alpha > 0 {
                        draw::blend_pixel(
                            img,
                            origin_x + col as i32,
                            origin_y + row as i32,
                            color,
                            alpha,
                        );
                    }
                }
            }
        }
        cursor += glyph.advance;
    }
}

/// 垂直居中绘制文字: area 范围内单行水平居中、块内垂直居中
// 绘图入参就是目标区域几何 + 字号 + 颜色，天然超 7 个
#[allow(clippy::too_many_arguments)]
pub fn draw_centered(
    fonts: &Fonts,
    img: &mut RgbaImage,
    text: &str,
    area_x: i32,
    area_y: i32,
    area_w: i32,
    area_h: i32,
    size: f32,
    color: draw::Color,
) {
    let width = measure(fonts, text, size);
    let x = area_x as f32 + (area_w as f32 - width) / 2.0;
    let baseline = area_y as f32 + area_h as f32 / 2.0 + size * 0.42;
    draw_line(fonts, img, text, x, baseline, size, color);
}

/// 扫到一个候选文件时怎么决定用不用它：装载一次，再看它能不能渲染中文。
///
/// 只认集合字体（`.ttc`/`.otc`）的第 0 面：`FontArc` 没有面索引这一档。系统里那些中文集合字
/// 第 0 面就是常规字面，随包的思源宋体更是单面 `.otf`，所以这一档没被用到；真要指定集合里的
/// 某一面，把自己那份拆成单面文件放进 `fonts/`。
fn load_file(path: &Path) -> Option<Arc<font::Typeface>> {
    let face = font::Typeface::acquire((path.to_path_buf(), None)).ok()?;
    // 「活」必须有真字形：缺字形会落到 .notdef，描出来是空的，那种字体画中文只能出豆腐
    face.face(false).renders('活').then_some(face)
}

/// Windows 常见中文字体文件名（按优先级）
const WINDOWS_CANDIDATES: &[&str] = &[
    "simhei.ttf",
    "msyh.ttc",
    "msyhbd.ttc",
    "msyhl.ttc",
    "simsun.ttc",
    "simsunb.ttf",
    "Deng.ttf",
    "Dengb.ttf",
    "Dengl.ttf",
];

/// 候选字体目录（按平台）
fn font_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Ok(windir) = std::env::var("WINDIR") {
        dirs.push(PathBuf::from(windir).join("Fonts"));
    }
    dirs.push(PathBuf::from("C:/Windows/Fonts"));
    if cfg!(target_os = "macos") {
        dirs.push(PathBuf::from("/System/Library/Fonts"));
        dirs.push(PathBuf::from("/Library/Fonts"));
    }
    dirs.push(PathBuf::from("/usr/share/fonts"));
    dirs.push(PathBuf::from("/usr/local/share/fonts"));
    if let Ok(home) = std::env::var("HOME") {
        dirs.push(PathBuf::from(home.clone()).join(".fonts"));
        dirs.push(PathBuf::from(home).join(".local/share/fonts"));
    }
    dirs
}

/// 递归收集字体候选文件并优先尝试名字含 CJK 关键字的字体
fn collect_font_files() -> Vec<PathBuf> {
    let mut files = Vec::new();
    for dir in font_dirs() {
        collect_font_files_in(&dir, &mut files, 0);
    }
    files.sort_by_key(|p| {
        let name = p
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_lowercase();
        let score = if name.contains("simhei") || name.contains("msyh") {
            0
        } else if name.contains("yahei")
            || name.contains("pingfang")
            || name.contains("song")
            || name.contains("hei")
        {
            1
        } else if name.contains("cjk")
            || name.contains("noto")
            || name.contains("wqy")
            || name.contains("droid")
            || name.contains("source")
        {
            2
        } else {
            3
        };
        (score, name)
    });
    files
}

fn collect_font_files_in(dir: &Path, files: &mut Vec<PathBuf>, depth: u32) {
    if depth > 3 {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_font_files_in(&path, files, depth + 1);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext = ext.to_lowercase();
            if ext == "ttf" || ext == "ttc" || ext == "otf" || ext == "otc" {
                files.push(path);
            }
        }
    }
}

/// 框架那三级都空时的兜底：递归扫系统字体目录，逐个文件试"能不能渲染中文"，挑第一个命中的；
/// 再兜一层直接探几个明确路径（目录扫描可能受权限限制）。全都空则回 None——
/// 调用方按"本机没有能渲染中文的字体"办。
///
/// 这一张候选表是本插件特有的（bilibili 与 petpet 只认固定路径），所以留在插件侧。
fn discover() -> Option<Arc<font::Typeface>> {
    for file in collect_font_files() {
        if let Some(face) = load_file(&file) {
            return Some(face);
        }
    }
    for name in WINDOWS_CANDIDATES {
        let path = PathBuf::from("C:/Windows/Fonts").join(name);
        if let Some(face) = load_file(&path) {
            return Some(face);
        }
    }
    None
}

#[cfg(test)]
mod memprobe_tests {
    use super::*;
    use crate::memprobe::{live, mb, os_footprint};
    use std::time::Instant;

    /// 字面这份常驻的两条钉子：**同一次出图里复用同一份**（第二次拿不该再解析一遍），
    /// 以及**出图结束之后整个还掉**。要单线程独占进程跑，否则并发用例的存活字节会一起算进来：
    /// `cargo test --release --lib -- --ignored --test-threads=1 --nocapture memprobe`
    #[test]
    #[ignore = "要读系统字体，且得独占进程才读得准"]
    fn the_font_lives_only_as_long_as_the_render_does() {
        let base = live();
        let (ws0, _, priv0) = os_footprint();
        let start = Instant::now();
        let Some(first) = acquire() else {
            println!("（本机找不到可渲染中文的字体，这一条验不了）");
            return;
        };
        let parsed = start.elapsed();
        let held = live() - base;
        // 真用它一次，别让这一份 Arc 被当成没用到
        let width = measure(&first, "活动日历", 32.0);
        let again = Instant::now();
        let second = acquire().expect("第一次拿得到，第二次也该拿得到");
        let reused = live() - base;
        let reuse_cost = again.elapsed();

        println!(
            "字面常驻 {}（解析 {:.2} s，量到宽度 {width:.0}px）",
            mb(held),
            parsed.as_secs_f64()
        );
        println!(
            "出图期间再拿一次：常驻 {}（多 {}），耗时 {:.3} ms",
            mb(reused),
            mb(reused.saturating_sub(held)),
            reuse_cost.as_secs_f64() * 1000.0
        );
        drop(second);
        drop(first);
        let rest = live() - base;
        let (ws1, peak, priv1) = os_footprint();
        println!(
            "drop 之后存活 {}，工作集 {} → {}，私有 {} → {}，进程峰值工作集 {}",
            mb(rest),
            mb(ws0),
            mb(ws1),
            mb(priv0),
            mb(priv1),
            mb(peak)
        );
        // 复用那条：Weak  upgrade 只多一份引用，不该再解析第二遍
        assert!(
            reused.saturating_sub(held) < 4 * 1024 * 1024,
            "同一次出图里第二次 acquire 多占了 {}，像是在重解析",
            mb(reused.saturating_sub(held))
        );
        assert!(
            reuse_cost.as_millis() < 50,
            "第二次 acquire 花了 {:.1} ms，不是在复用",
            reuse_cost.as_secs_f64() * 1000.0
        );
        // 这一条是「用完就释放」的闸：出图结束还回不到基线，就是有强引用没放
        assert!(
            rest < 8 * 1024 * 1024,
            "drop 之后仍常驻 {}，说明字面还被谁按着",
            mb(rest)
        );
    }
}
