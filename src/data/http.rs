//! 插件的出站 HTTP：只负责拼请求头，真正发请求的是框架
//!
//! 这里刻意不链接 reqwest。dll 会把 reqwest/hyper/tokio 静态链接成独立的一份，而运行时上下文
//! 是 thread-local 的：框架能在自己 poll 的每个任务里替这一份补上上下文，却管不到 hyper 自己
//! 派生的后台任务（补连接、空闲驱逐），那些任务一 `tokio::spawn` 就 panic，顺带把连接池的锁
//! 变成 PoisonError，之后每个请求都跟着炸。`arona::runtime::http` 把请求递给宿主那份客户端，
//! 池子、keepalive、超时全开也跑在宿主的常驻 runtime 上（详见框架的 PLUGIN_DEVELOPMENT.md §14.1）。
//!
//! 三个 `*_with` 之外的函数都只是这套请求头的组合器，调用方无需知道底下走的是谁。

/// 浏览器 UA（对应原版 gameKeeHeaders 中的 user-agent）
pub const USER_AGENT_VALUE: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/150.0.0.0 Safari/537.36";
/// arona 云端接口使用的 UA（对应原版 NetworkUtil.request）
pub const BACKEND_USER_AGENT_VALUE: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/102.0.0.0 Safari/537.36";

const ACCEPT_LANGUAGE_VALUE: &str = "zh-CN,zh;q=0.9,zh-Hans;q=0.8,und;q=0.7,zh-Hant;q=0.6,ja;q=0.5";
const JSON_ACCEPT: &str = "application/json, text/plain, */*";
const IMAGE_ACCEPT: &str = "image/avif,image/webp,image/apng,image/svg+xml,image/*,*/*;q=0.8";

/// GameKee 各 JSON 接口的完整浏览器请求头（对应原版 GameKeeUtil.gameKeeHeaders）。
///
/// 注意：host/connection 由 HTTP 栈自动处理，accept-encoding 由框架的客户端自动协商并解压，
/// 手动设置会与自动解压冲突，因此这里不重复设置这两类字段。
pub fn game_kee_headers(referer: &str) -> Vec<(&'static str, String)> {
    vec![
        ("accept", JSON_ACCEPT.to_string()),
        ("accept-language", ACCEPT_LANGUAGE_VALUE.to_string()),
        ("device-num", "1".to_string()),
        ("dnt", "1".to_string()),
        ("game-alias", "ba".to_string()),
        ("lang", "zh-cn".to_string()),
        ("referer", referer.to_string()),
        (
            "sec-ch-ua",
            "\"Not;A=Brand\";v=\"8\", \"Chromium\";v=\"150\", \"Google Chrome\";v=\"150\""
                .to_string(),
        ),
        ("sec-ch-ua-mobile", "?0".to_string()),
        ("sec-ch-ua-platform", "\"Windows\"".to_string()),
        ("sec-fetch-dest", "empty".to_string()),
        ("sec-fetch-mode", "cors".to_string()),
        ("sec-fetch-site", "same-origin".to_string()),
        ("user-agent", USER_AGENT_VALUE.to_string()),
        ("x-requested-with", "XMLHttpRequest".to_string()),
    ]
}

/// GameKee 图片 CDN 下载头（对应原版 downloadImages）
pub fn game_kee_image_headers(referer: &str) -> Vec<(&'static str, String)> {
    vec![
        ("accept", IMAGE_ACCEPT.to_string()),
        ("accept-language", ACCEPT_LANGUAGE_VALUE.to_string()),
        ("referer", referer.to_string()),
        ("user-agent", USER_AGENT_VALUE.to_string()),
    ]
}

/// api-cdn.gamekee.com 内容 JSON 请求头（对应原版 getScheduleNoteImages 中的内联请求头）
pub fn game_kee_content_headers(referer: &str) -> Vec<(&'static str, String)> {
    vec![
        ("accept", JSON_ACCEPT.to_string()),
        ("accept-language", ACCEPT_LANGUAGE_VALUE.to_string()),
        ("dnt", "1".to_string()),
        ("origin", "https://www.gamekee.com".to_string()),
        ("referer", referer.to_string()),
        (
            "sec-ch-ua",
            "\"Not;A=Brand\";v=\"8\", \"Chromium\";v=\"150\", \"Google Chrome\";v=\"150\""
                .to_string(),
        ),
        ("sec-ch-ua-mobile", "?0".to_string()),
        ("sec-ch-ua-platform", "\"Windows\"".to_string()),
        ("sec-fetch-dest", "empty".to_string()),
        ("sec-fetch-mode", "cors".to_string()),
        ("sec-fetch-site", "same-site".to_string()),
        ("user-agent", USER_AGENT_VALUE.to_string()),
    ]
}

/// arona 云端鉴权头（对应原版 NetworkUtil.requestWithAuth）
pub fn arona_backend_headers(uuid: &str, version: &str) -> Vec<(&'static str, String)> {
    vec![
        ("Authorization", uuid.to_string()),
        ("version", version.to_string()),
        ("user-agent", BACKEND_USER_AGENT_VALUE.to_string()),
    ]
}

/// 带指定请求头的 GET，返回响应文本
pub async fn get_with(url: &str, headers: &[(&str, String)]) -> Result<String, String> {
    arona::runtime::http::get(url, headers).await
}

/// 带指定请求头的 GET，返回原始字节
pub async fn get_bytes_with(url: &str, headers: &[(&str, String)]) -> Result<Vec<u8>, String> {
    arona::runtime::http::get_bytes(url, headers).await
}

/// 通用浏览器 GET + referer + 少量额外头（kivo.wiki 等非 GameKee 接口使用）
pub async fn get(url: &str, referer: &str, extra: &[(&str, &str)]) -> Result<String, String> {
    let mut headers: Vec<(&str, String)> = vec![
        ("user-agent", USER_AGENT_VALUE.to_string()),
        ("accept", JSON_ACCEPT.to_string()),
        ("accept-language", ACCEPT_LANGUAGE_VALUE.to_string()),
        ("referer", referer.to_string()),
    ];
    for (key, value) in extra {
        headers.push((key, (*value).to_string()));
    }
    get_with(url, &headers).await
}

/// 通用浏览器 GET 图片字节（学生头像/塔罗图片下载）
pub async fn get_bytes(url: &str, referer: &str) -> Result<Vec<u8>, String> {
    let headers: Vec<(&str, String)> = vec![
        ("user-agent", USER_AGENT_VALUE.to_string()),
        ("accept", IMAGE_ACCEPT.to_string()),
        ("accept-language", "zh-CN,zh;q=0.9".to_string()),
        ("referer", referer.to_string()),
    ];
    get_bytes_with(url, &headers).await
}
