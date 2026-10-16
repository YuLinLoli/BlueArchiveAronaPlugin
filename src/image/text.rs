//! 中文字体发现与文本光栅化（对应原版 java.awt.Font/Graphics2D.drawString）
//!
//! 字面优先要框架随包的那一份（`arona::runtime::assets::cjk_font()`，安装目录 `fonts/` 下的
//! 思源宋体简中），拿不到才去常见系统字体目录扫（Windows 首选 simhei/msyh，Linux/macOS 回退
//! Noto CJK / 文泉驿 / PingFang），一份能渲染中文的都没有就返回 None。
//!
//! 栅格化走 ab_glyph 的惰性后端：内存里只留字体文件的原始字节，画到哪个字才解析哪个字，
//! 栅格结果进一层按 (字符, 字号) 计的有界 LRU。换掉的是 fontdue 那种"把每个码位的轮廓一次
//! 全解析完"的路子——bilibili 插件那侧同一枚探针量过：雅黑常驻 307.6 MB、思源宋体 253.5 MB，
//! 装载还要 0.3~0.6 s；惰性这一档装载 0.01 s、常驻就是文件字节那一档（思源宋体两份 22.6 MB）。
//! 两档后端的几何逐字比过（TrueType 与 CFF 两类轮廓都验），包围盒与 advance 最大偏差 0 格、0.0 px，
//! 所以这一处换的不是观感，只是谁在什么时候解析轮廓。

use crate::image::draw;
use ab_glyph::{Font as _, Glyph, ScaleFont};
use image::RgbaImage;
use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

/// 字形缓存上限。一张图实际用到的 (字符, 字号) 组合在几百到一两千这一档，4096 条按 1~2 KB 算
/// 是几 MB——比全量预解析的 120~250 MB 差两个数量级，又足够让一张图里反复出现的字都命中
const GLYPH_CACHE_MAX: usize = 4096;

/// 一个栅格好的字形：落点 + 覆盖率位图。落点口径沿用 fontdue 的 `Metrics`——`xmin`/`ymin` 是
/// 相对笔位置（水平笔位、垂直基线）的左边界与**下**边界，y 向上为正，所以下面那一路贴图代码不用动
struct Raster {
    xmin: i32,
    ymin: i32,
    width: usize,
    height: usize,
    advance: f32,
    coverage: Vec<u8>,
}

type Cached = Arc<Raster>;

/// 字体集合：惰性后端下它就是那份原始字节 + 一层字形缓存
pub struct Fonts {
    face: ab_glyph::FontArc,
    glyphs: Mutex<Cache>,
}

/// 有界的字形 LRU，口径与 bilibili 插件、petpet 的素材缓存一致：命中挪到队尾，到顶从头上摘
struct Cache {
    map: HashMap<(char, u32), Cached>,
    order: VecDeque<(char, u32)>,
}

impl Cache {
    fn touch(&mut self, key: (char, u32)) {
        if let Some(at) = self.order.iter().position(|have| *have == key) {
            self.order.remove(at);
        }
        self.order.push_back(key);
    }

    fn put(&mut self, key: (char, u32), done: Cached) {
        while self.map.len() >= GLYPH_CACHE_MAX && self.map.len() > 1 {
            match self.order.pop_front() {
                Some(oldest) => {
                    self.map.remove(&oldest);
                }
                None => break,
            }
        }
        self.map.insert(key, done);
        self.touch(key);
    }
}

impl Fonts {
    /// 把"字号 = em 的像素数"换成 ab_glyph 要的 `PxScale`。
    ///
    /// ab_glyph 的 `PxScale` 基准不是 em，而是 `height_unscaled() = ascent - descent`
    /// （雅黑那一档 1320 而 unitsPerEm 是 1000）——不换算所有文字会整体小 24%，排版全乱。
    /// 换算式两边一约：`scale / height_unscaled == px / units_per_em`。
    fn scale(&self, px: f32) -> f32 {
        scale_of(&self.face, px)
    }

    /// 一个字占多宽（排版只用这一个数，不必解析轮廓）
    fn advance(&self, ch: char, px: f32) -> f32 {
        let scaled = self.face.as_scaled(self.scale(px));
        scaled.h_advance(scaled.glyph_id(ch))
    }

    fn raster(&self, ch: char, px: f32) -> Cached {
        let key = (ch, px.to_bits());
        let mut cache = self
            .glyphs
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(hit) = cache.map.get(&key) {
            let hit = Arc::clone(hit);
            cache.touch(key);
            return hit;
        }
        let done = Arc::new(uncached(&self.face, self.scale(px), ch));
        cache.put(key, Arc::clone(&done));
        done
    }
}

/// 字号(px) → ab_glyph 要的 `PxScale`（见 [`Fonts::scale`] 为什么要这一道换算）
fn scale_of(font: &ab_glyph::FontArc, px: f32) -> f32 {
    let em = font.units_per_em().unwrap_or(1000.0);
    if em == 0.0 {
        return px;
    }
    px * font.height_unscaled() / em
}

/// 真正解析一个字的轮廓并铺成覆盖率位图（缓存没命中时才走这里）
fn uncached(font: &ab_glyph::FontArc, scale: f32, ch: char) -> Raster {
    let scaled = font.as_scaled(scale);
    let id = scaled.glyph_id(ch);
    let advance = scaled.h_advance(id);
    let glyph: Glyph = id.with_scale(scale);
    let Some(outline) = font.outline_glyph(glyph) else {
        return Raster {
            xmin: 0,
            ymin: 0,
            width: 0,
            height: 0,
            advance,
            coverage: Vec::new(),
        };
    };
    // ab_glyph 的 `px_bounds` 是 **y 向下**的像素包围盒（min.y 是顶边），而上面那一路沿用 fontdue
    // 的口径：`ymin` 是相对基线的**下**边界、向上为正。换算就一次取负。
    let bounds = outline.px_bounds();
    let (left, right) = (bounds.min.x.floor(), bounds.max.x.ceil());
    let (top, bottom) = (bounds.min.y.floor(), bounds.max.y.ceil());
    let width = (right - left).max(0.0) as usize;
    let height = (bottom - top).max(0.0) as usize;
    let mut coverage = vec![0u8; width * height];
    if width > 0 && height > 0 {
        outline.draw(|x, y, value| {
            let (x, y) = (x as usize, y as usize);
            if x < width && y < height {
                coverage[y * width + x] = (value * 255.0).round().clamp(0.0, 255.0) as u8;
            }
        });
    }
    Raster {
        xmin: left as i32,
        ymin: -(bottom as i32),
        width,
        height,
        advance,
        coverage,
    }
}

/// 字面缓存**只存 Weak**。惰性后端下这份常驻本来就只是字体文件的字节（思源宋体常规 11.1 MB
/// 加粗体 11.5 MB）再叠一层有界字形缓存，不再是要长期按着的百 MB 轮廓——但"没人出图就还掉"
/// 这条口径没变，而且装载从 0.3 s 落到 0.01 s，还了再取几乎不要钱。
/// 谁在出图谁拿着 `Arc`（见 [`acquire`]），最后一个 Arc 落下后整个还给分配器；
/// 期间握着锁，同批并发出图不会各装载一份。
static CACHE: OnceLock<Mutex<Weak<Fonts>>> = OnceLock::new();

/// 取字面：槽里还有人拿着就复用，没有才去盘上找并装载。
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
    text.chars().map(|ch| fonts.advance(ch, size)).sum()
}

/// 单行文字基线指标: (ascent, descent, new_line_size)。
///
/// 口径与 fontdue 的 `horizontal_line_metrics` 对齐：hhea 那三个数是**字库单位**，
/// 乘 `px / units_per_em` 落到像素；`descent` 通常在基线以下为负，
/// `new_line_size = ascent - descent + line_gap`（fontdue 就是这么算的，两处出的图要能对上）。
pub fn line_metrics(fonts: &Fonts, size: f32) -> Option<(f32, f32, f32)> {
    let em = fonts.face.units_per_em()?;
    if em == 0.0 {
        return None;
    }
    let k = size / em;
    let ascent = fonts.face.ascent_unscaled() * k;
    let descent = fonts.face.descent_unscaled() * k;
    let line_gap = fonts.face.line_gap_unscaled() * k;
    Some((ascent, descent, ascent - descent + line_gap))
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
    let mut cursor = x;
    for ch in text.chars() {
        let glyph = fonts.raster(ch, size);
        if glyph.width > 0 && glyph.height > 0 {
            // 位图左上角相对基线/笔位置: x=cursor+xmin, y=baseline-(ymin+height)。
            // `ymin` 是下边界（向上为正），所以顶边就是 `-(ymin + height)`——与换后端之前那一式同解，
            // 差别只是包围盒在 [`uncached`] 里已经取过整，这里不再对整数 floor。
            let top = -(glyph.ymin + glyph.height as i32);
            let origin_x = (cursor + glyph.xmin as f32).floor() as i32;
            let origin_y = (baseline_y + top as f32).floor() as i32;
            for row in 0..glyph.height {
                for col in 0..glyph.width {
                    let coverage = glyph.coverage[row * glyph.width + col];
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

fn is_cjk(font: &ab_glyph::FontArc) -> bool {
    // 中文字符必须有可光栅化的字形（缺失会落到 .notdef，宽度为 0 视为不支持）。
    // 探的是 24 px 这一档：直接拿 height_unscaled 当刻度会描出一张一千多像素高的位图，白_alloc_一次
    let glyph = uncached(font, scale_of(font, 24.0), '活');
    glyph.width > 0 && glyph.height > 0
}

/// 从字体文件字节装载一份可用的字面。
///
/// 只认集合字体（`.ttc`/`.otc`）的第 0 面：ab_glyph 的 `FontArc` 没有面索引这一档。系统里那些
/// 中文集合字第 0 面就是常规字面，随包的思源宋体更是单面 `.otf`，所以这一档没被用到；
/// 真要指定集合里的某一面，把自己那份拆成单面文件放进 `fonts/`。
fn load_face(bytes: Vec<u8>) -> Option<Fonts> {
    let face = ab_glyph::FontArc::try_from_vec(bytes).ok()?;
    is_cjk(&face).then(|| Fonts {
        face,
        glyphs: Mutex::new(Cache {
            map: HashMap::new(),
            order: VecDeque::new(),
        }),
    })
}

fn load_file(path: &Path) -> Option<Fonts> {
    load_face(std::fs::read(path).ok()?)
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

/// 装载字面：先要框架随包那一份（安装目录 `fonts/`，与 GUI 和 bilibili 插件同一个出口），
/// 拿不到才扫系统字体目录。两边都空则回 None——调用方按"没有可渲染中文的字体"办。
fn discover() -> Option<Fonts> {
    if let Some((regular, _)) = arona::runtime::assets::cjk_font()
        && let Some(fonts) = load_file(&regular)
    {
        return Some(fonts);
    }
    let files = collect_font_files();
    for file in files {
        if let Some(fonts) = load_file(&file) {
            return Some(fonts);
        }
    }
    // 兜底：直接探测几个明确路径（目录扫描可能受权限限制）
    for name in WINDOWS_CANDIDATES {
        let path = PathBuf::from("C:/Windows/Fonts").join(name);
        if let Some(fonts) = load_file(&path) {
            return Some(fonts);
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
