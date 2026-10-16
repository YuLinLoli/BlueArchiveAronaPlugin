//! 中文字体发现与文本光栅化（对应原版 java.awt.Font/Graphics2D.drawString）
//! 运行时在常见系统字体目录中查找可渲染中文的字体（Windows 首选 simhei/msyh，
//! Linux/macOS 回退 Noto CJK / 文泉驿 / PingFang），找不到字体时渲染函数返回 None。

use crate::image::draw;
use image::RgbaImage;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// 字体集合：faces[0] 为默认可用面（可能来自 .ttc 的某个 collection index）
pub struct Fonts {
    faces: Vec<fontdue::Font>,
}

impl Fonts {
    pub fn face(&self) -> &fontdue::Font {
        &self.faces[0]
    }
}

/// 字面缓存**只存 Weak**。fontdue 会把字体文件里每个码位的字形轮廓全量预解析后常驻，
/// 实测这一档就是 126 MB（simhei 文件才 9.3 MB；换 msyh、simsun、Noto 都在 120~190 MB 这个量级），
/// 而它只在出图的那一瞬间用得上——所以不该由一个 `static` 替进程长期按着它。
/// 谁在出图谁拿着 `Arc`（见 [`acquire`]），最后一个 Arc 落下后轮廓整个还给分配器；
/// 下一次出图再解析一遍（实测 0.3 s 量级），解析期间握着锁，同批并发出图不会各解析一份。
static CACHE: OnceLock<Mutex<Weak<Fonts>>> = OnceLock::new();

/// 取字面：槽里还有人拿着就复用，没有才去盘上找并解析。
/// 回 `None` 就是"本机没有能渲染中文的字体"——调用方要么按无文本出图，要么明确报错，
/// 口径与原来的 `available()` 相同，只是这一份不再常驻。
pub fn acquire() -> Option<Arc<Fonts>> {
    let cache = CACHE.get_or_init(Default::default);
    let mut slot = cache
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(alive) = slot.upgrade() {
        return Some(alive);
    }
    let fresh = Arc::new(discover()?);
    *slot = Arc::downgrade(&fresh);
    Some(fresh)
}

/// 文本宽度（px），用于居中/右对齐排版
pub fn measure(fonts: &Fonts, text: &str, size: f32) -> f32 {
    let font = fonts.face();
    let mut width = 0.0f32;
    for ch in text.chars() {
        let metrics = font.metrics(ch, size);
        width += metrics.advance_width;
    }
    width
}

/// 单行文字基线指标: (ascent, descent, new_line_size)
pub fn line_metrics(fonts: &Fonts, size: f32) -> Option<(f32, f32, f32)> {
    let lm = fonts.face().horizontal_line_metrics(size)?;
    Some((lm.ascent, lm.descent, lm.new_line_size))
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
    let font = fonts.face();
    let mut cursor = x;
    for ch in text.chars() {
        let (metrics, bitmap) = font.rasterize(ch, size);
        if metrics.width > 0 && metrics.height > 0 && !bitmap.is_empty() {
            // 位图左上角相对基线/笔位置: x=floor(bounds.xmin), y=floor(-(ymin+height))
            let origin_x = (cursor + metrics.bounds.xmin).floor() as i32;
            let origin_y =
                (baseline_y + (-(metrics.bounds.ymin + metrics.bounds.height)).floor()) as i32;
            for row in 0..metrics.height {
                for col in 0..metrics.width {
                    let coverage = bitmap[row * metrics.width + col];
                    if coverage > 0 {
                        draw::blend_pixel(
                            img,
                            origin_x + col as i32,
                            origin_y + row as i32,
                            color,
                            coverage,
                        );
                    }
                }
            }
        }
        cursor += metrics.advance_width;
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

fn is_cjk(font: &fontdue::Font) -> bool {
    // 中文字符必须有可光栅化的字形（若缺失会走默认字形，宽度为 0 视为不支持）
    let (metrics, _) = font.rasterize('活', 24.0);
    metrics.width > 0 && metrics.height > 0
}

/// 尝试从字体文件字节加载可用的 collection face
fn load_face(bytes: &[u8]) -> Option<fontdue::Font> {
    // .ttc/.otc 等集合字体最多尝试前 6 个 face
    for index in 0..6u32 {
        let settings = fontdue::FontSettings {
            collection_index: index,
            ..Default::default()
        };
        if let Ok(font) = fontdue::Font::from_bytes(bytes, settings)
            && is_cjk(&font)
        {
            return Some(font);
        }
    }
    None
}

fn load_file(path: &Path) -> Option<fontdue::Font> {
    let bytes = std::fs::read(path).ok()?;
    load_face(&bytes)
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

/// 扫描系统并缓存字体
fn discover() -> Option<Fonts> {
    let files = collect_font_files();
    for file in files {
        if let Some(font) = load_file(&file) {
            return Some(Fonts { faces: vec![font] });
        }
    }
    // 兜底：直接探测几个明确路径（目录扫描可能受权限限制）
    for name in WINDOWS_CANDIDATES {
        let path = PathBuf::from("C:/Windows/Fonts").join(name);
        if let Some(font) = load_file(&path) {
            return Some(Fonts { faces: vec![font] });
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
