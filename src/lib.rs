//! BluearchivePlugin —— 碧蓝档案功能插件。
//!
//! 本 crate 把「功能部分」从 arona 框架里独立出来：抽卡、活动日历、攻略、塔罗、
//! 名字记录、定时任务、备份恢复等命令与它们的数据/图片/数据库都在这里。
//! 框架（OneBot 连接、管理面板、群授权/黑名单、命令分发骨架）通过 [`arona`] 提供，
//! 本插件只实现 `arona::plugin::AronaPlugin` 的四个阶段，再在文件末尾用
//! `arona::export_arona_plugin!` 导出 C 符号。没有 `main`，也跑不起独立进程：
//! 编出的 dll 由框架启动时扫描 `plugins/` 装载，进程、运行时与 OneBot 连接都归框架管。
//!
//! 调试：`cargo build --release` 产出 `target/release/bluearchive_plugin.dll`，拷进
//! 框架运行目录的 `plugins/bluearchive/`，再在那个目录里启动 `arona-rs.exe`
//! （加 `--nogui` 走命令行）。工具链口径（rustc 版本、profile、crt-static、arona 的
//! feature 集合）必须与宿主一致，否则握手会拒载——日志里写的是中文原因，
//! 完整规范见框架仓库的 `PLUGIN_DEVELOPMENT.md`。

// 功能模块都是插件私有；对外只暴露 BluearchivePlugin 这一个入口。
// 命令实现、命令表登记（dispatcher）与服务清单（api）平铺在三处，不再套一层目录。
mod activity;
mod api;
mod commands;
mod config;
mod data;
mod db;
mod dispatcher;
mod entity;
mod gacha;
mod image;
/// 内存探针：只在测试构建里编译，用来把"字面那份轮廓常驻到底有多大、出图之后回不回得来"落成数字
#[cfg(test)]
mod memprobe;
#[cfg(test)]
#[global_allocator]
static MEMORY_PROBE: memprobe::Counting = memprobe::Counting;
mod runtime;
mod util;

#[cfg(test)]
mod onebot_print_test;
#[cfg(test)]
mod ten_pull_test;
#[cfg(test)]
mod trainer_test;

use arona::plugin::{AronaPlugin, PluginContext, PluginMeta, PluginRegistrar};
use arona::runtime::config::Feature;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicI32, Ordering};

/// 本插件的稳定 id：框架按它规定落盘位置（`plugins/bluearchive/`、`config/bluearchive/arona.yml`、
/// `data/bluearchive/`），`disabled_plugins` 里也写它。改名等于换一份用户数据，别动。
pub(crate) const PLUGIN_ID: &str = "bluearchive";

/// 本插件的数据目录（data/bluearchive：数据库与备份都在这）
pub(crate) fn data_dir() -> PathBuf {
    arona::runtime::paths::plugin_data_dir(PLUGIN_ID)
}

/// 本插件的图片目录（data/bluearchive/image：活动日历图、抽卡卡面、攻略缓存…）
pub(crate) fn image_dir() -> PathBuf {
    arona::runtime::paths::plugin_image_dir(PLUGIN_ID)
}

/// 本插件的 SQLite 数据库文件（data/bluearchive/arona.db）
pub(crate) fn db_file() -> PathBuf {
    data_dir().join("arona.db")
}

/// 本插件的备份目录（data/bluearchive/backups）
pub(crate) fn backups_dir() -> PathBuf {
    let dir = data_dir().join("backups");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 本插件的配置目录（config/bluearchive：插件自己的 arona.yml 由框架生成，其它随带配置放这里）
pub(crate) fn config_dir() -> PathBuf {
    arona::runtime::paths::plugin_config_dir(PLUGIN_ID)
}

/// 旧版本把图片/数据库/备份放在 `arona-standalone/` 下。升级后一次性**复制**到
/// `data/bluearchive/` 与 `config/bluearchive/`（只补缺、保留旧文件），用户不必重新拉数据。
fn migrate_legacy_dirs() {
    let legacy = arona::runtime::paths::legacy_root();
    if !legacy.is_dir() {
        return;
    }
    let data = data_dir();
    arona::runtime::paths::migrate_dir(&legacy.join("images"), &image_dir());
    for name in ["arona.db", "arona.db-wal", "arona.db-shm"] {
        arona::runtime::paths::migrate_file(&legacy.join("data").join(name), &data.join(name));
    }
    arona::runtime::paths::migrate_dir(&legacy.join("backups"), &backups_dir());
    arona::runtime::paths::migrate_file(
        &legacy.join("trainer_config.yml"),
        &config_dir().join("trainer_config.yml"),
    );
}

/// 碧蓝档案功能插件。
pub struct BluearchivePlugin;

/// 可被分群开关控制的功能清单（key 与 dispatcher::feature_for 对齐）。
const FEATURES: [Feature; 10] = [
    Feature {
        key: "gacha",
        name: "抽卡",
        description: "单抽/十连/抽卡服务器/狗叫/历史",
    },
    Feature {
        key: "name",
        name: "游戏名",
        description: "游戏名记录/谁是/叫我",
    },
    Feature {
        key: "tarot",
        name: "塔罗牌",
        description: "塔罗牌占卜",
    },
    Feature {
        key: "activity",
        name: "活动",
        description: "活动日历查询与本地日历图",
    },
    Feature {
        key: "trainer",
        name: "攻略",
        description: "活动攻略/日程笔记/当期卡池查询",
    },
    Feature {
        key: "task",
        name: "定时任务",
        description: "查看/触发定时任务(管理员)",
    },
    Feature {
        key: "backup",
        name: "备份恢复",
        description: "备份/恢复配置与数据库(管理员)",
    },
    Feature {
        key: "config",
        name: "配置管理",
        description: "/config 查看与修改配置(管理员)",
    },
    Feature {
        key: "emergency",
        name: "紧急停止",
        description: "非管理员投票制停止服务",
    },
    Feature {
        key: "help",
        name: "帮助",
        description: "帮助与运行状态查询",
    },
];

/// 上一次生效的推送小时（-1 = 尚未初始化）：arona.yml 热重载后据此判断是否需要重建每日任务
static LAST_NOTIFY_HOUR: AtomicI32 = AtomicI32::new(-1);

impl AronaPlugin for BluearchivePlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta::new(
            PLUGIN_ID,
            "BluearchivePlugin",
            env!("CARGO_PKG_VERSION"),
            "碧蓝档案功能插件（抽卡/活动/攻略/塔罗/名字记录/备份恢复）",
        )
        .with_author("Arona-rs")
    }

    fn install(&self, reg: &PluginRegistrar) -> Result<(), String> {
        // 先把旧目录里的数据补迁到统一位置，后面的建库/读图才找得到东西
        migrate_legacy_dirs();
        // 登记功能开关：框架 GUI 的「功能开关」页与配置模板注释据此生成
        for feature in FEATURES {
            reg.feature(feature);
        }
        // 登记本插件自持有的配置区（notify / trainer）：必须早于框架加载配置，
        // 否则 config/bluearchive/arona.yml 生成不出它们的带注释模板。
        reg.config::<config::NotifyConfig>("notify");
        reg.config::<config::TrainerConfig>("trainer");
        Ok(())
    }

    fn configure(&self, ctx: &PluginContext) -> Result<(), String> {
        // 把本插件的全部命令与兜底登记进框架的命令表（内部会注册全部服务）
        dispatcher::register(ctx, ctx.onebot_config.clone());
        // 聊天记录的记账/保留期/清理已在框架侧（arona::runtime::chatlog），插件不再自己挂钩子

        // --test-notify：20 秒后完整跑一次每日推送，便于联调验证
        if ctx.test_notify {
            let context = ctx.clone();
            ctx.delay_job(
                20,
                "TestNotify",
                Arc::new(move || {
                    let inner = context.clone();
                    context.spawn_as("测试推送", async move {
                        activity::notify::push(&inner, false).await;
                    });
                }),
            );
            arona::runtime::log::info("测试推送已安排, 将在 20 秒后执行");
        }
        Ok(())
    }

    fn start(&self, ctx: &PluginContext) -> Result<(), String> {
        arona::runtime::log::info("initializing database...");
        if !db::start() {
            // 库起不来只影响抽卡/名字记录这类要落库的功能，活动与攻略查询照常，
            // 所以记错误但不下线整个插件。
            arona::runtime::log::error("database init failed");
        }
        util::tarot::ensure_initialized();

        // 后台预热：kivo 学生数据 + 启动刷新本地资源图片（活动日历图 / 塔罗图）。
        // 走 ctx.spawn 而不是裸 tokio::spawn：插件被停用时框架要能取消它们。
        ctx.spawn_as("预热学生数据", async move {
            data::kivo::init().await;
        });
        ctx.spawn_as("刷新图片", async move {
            commands::activity::refresh_all_images().await;
            commands::tarot::download_all_images().await;
        });

        // 每日活动推送（含启动 20 秒后的预警初始化）+ 每天 0 点的本地资源图片刷新
        activity::notify::enable_service(ctx);
        LAST_NOTIFY_HOUR.store(
            config::notify(ctx.framework()).every_day_hour.clamp(0, 23),
            Ordering::SeqCst,
        );
        commands::activity::enable_image_refresh_job(ctx);
        Ok(())
    }

    fn on_config_reload(&self, ctx: &PluginContext) {
        // 框架在每次热重载/写入后都会回调这里；只有推送小时真的变了才重建每日任务。
        let hour = config::notify(ctx.framework()).every_day_hour.clamp(0, 23);
        let previous = LAST_NOTIFY_HOUR.swap(hour, Ordering::SeqCst);
        if previous != hour && ctx.framework().jobs().exists(activity::notify::DAILY_JOB) {
            ctx.remove_job(activity::notify::DAILY_JOB);
            activity::notify::enable_daily_job(ctx, hour as u32);
            arona::runtime::log::info(format!("推送小时变更，重建每日任务: 每天 {hour} 点"));
        }
        // 聊天记录的保留期与清理间隔由框架按 arona.yml 的 chatlog 段自己校准
    }

    fn stop(&self, _ctx: &PluginContext) {
        // 定时任务与后台命令不用在这里逐个取消：框架在 stop 之后按插件归属整组回收
        // （见 arona::plugin::manager::revoke_resources）。这里只关自己打开的库。
        db::close();
    }
}

// 动态装载入口：导出 `arona_plugin_abi` / `arona_plugin_toolchain` / `arona_plugin_host`
// / `arona_plugin_new` 四个 C 符号。框架扫描 plugins/ 时靠它们握手、核对工具链指纹、
// 把宿主的进程级状态接过来，最后取出插件实例。插件作者要写的就这一行。
// 单位结构体直接写类型名，不必 `::default()`。
arona::export_arona_plugin!(BluearchivePlugin);

#[cfg(test)]
mod abi_notes {
    /// README 里手抄的符号布局号必须等于编译进这份 dll 的那个常量。
    /// 数字漂了的后果是用户拿旧宿主装新 dll（或反过来），而拒载日志只会说"布局不匹配"——
    /// 这条与 `image-auto-save` 那仓同一口径，框架每次抬布局时这里会红，提醒改 README。
    #[test]
    fn the_readme_states_the_layout_this_dll_was_built_for() {
        let readme = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/README.md"))
            .expect("README.md 该在仓库根");
        let expected = format!("布局 {}", arona::plugin::abi::ABI_LAYOUT);
        assert!(
            readme.contains(&expected),
            "README 里找不到「{expected}」——框架抬了布局号，安装那一节要跟着改"
        );
    }
}
