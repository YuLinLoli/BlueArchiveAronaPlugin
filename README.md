# 碧蓝档案插件 · Arona-rs 功能包

[Arona-rs](https://github.com/YuLinLoli/Arona-rs) 框架的一个**功能插件**：抽卡、活动日历、攻略查询、
塔罗牌、游戏名记录、备份恢复与每日推送。框架本身只负责连接、授权、命令分发与插件生命周期，
群里能回应什么完全由装了哪些插件决定 —— 这个仓库就是其中一揽子玩法。

- 框架仓库：<https://github.com/YuLinLoli/Arona-rs>（插件接口说明见其 `PLUGIN_DEVELOPMENT.md`）
- 上游原版：<https://github.com/diyigemt/arona>（Kotlin + Mirai 插件，玩法与数据来源）
- 开源协议：GNU AGPL-3.0-only（见 `LICENSE`，中文译本 `LICENSE.zh-CN.md`）

## 命令一览

| 分类 | 命令 |
| --- | --- |
| 抽卡 | `/单抽` `/十连` `/狗叫` `/历史` `/抽卡服务器 日服\|国服\|国际服` |
| 名片 | `/游戏名` `/谁是` `/谁叫` `/叫我` |
| 娱乐 | `/塔罗牌 [牌名]` |
| 攻略 | `/活动 [日服\|国服\|国际服]` `/攻略 <关键词>` |
| 管理 | `/arona status\|services\|service list\|version\|help`、`/config`、`/任务`、`/备份`、`/恢复`、`/紧急停止` |
| 推送 | 每天定点推三服活动日历（默认 8:00，可配）；活动结束前 5 小时与 1 小时各提醒一次；学生生日并入日历 |

图片（抽卡结果图、活动日历图）由 Rust 自己绘制，不依赖 Java AWT；系统没有可渲染中文的字体时自动降级成文本。
塔罗牌图沿用原版方案从 CDN 下载。

## 怎么装进框架

框架不链接本仓库、也不需要在自己的构建里登记它：编出一个 dll，丢进框架运行目录的
`plugins\`，启动时框架自动扫描、握手、装载（和 mirai 把 jar 丢进 `plugins/` 一样）。三步：

1. 用**与宿主同一套工具链**编译（硬性口径见下一节）：

   ```bash
   git clone https://github.com/YuLinLoli/BlueArchiveAronaPlugin
   cd BlueArchiveAronaPlugin
   cargo build --release                 # 产物 target\release\bluearchive_plugin.dll
   ```

2. 把 dll 放进框架的运行目录，每个插件独占一层以 id 命名的子目录（推荐，方便另放资源）：

   ```
   <框架运行目录>\plugins\bluearchive\bluearchive_plugin.dll
   ```

   默认安装位置是 `%LOCALAPPDATA%\Programs\Arona-rs\`（装成「为所有用户」时在
   `Program Files\Arona-rs\`）。直接把 dll 丢在 `plugins\` 根下也认。

3. 启动框架。日志里应该有握手通过与装载成功两行，GUI 的「插件管理」页同时就能看到它：

   ```
   [Arona] 插件 ABI 握手通过: 布局 8，契约 1.0.0
   [Arona] 已装载动态插件: BluearchivePlugin 0.4.1 (bluearchive_plugin.dll)
   ```

装好后配置与数据都落在框架的运行目录里，本仓库不需要也不应该提交它们：

```
plugins/bluearchive/plugin.yml   插件清单（框架首次装载时生成：id、名称、版本、loader）
config/bluearchive/arona.yml     本插件的配置（首次启动自动生成带注释的模板）
data/bluearchive/                数据库、image/ 生成的图片、backups/ 备份
```

运行期开关在框架的 `config/arona.yml`：`disabled_plugins: [bluearchive]` 整体停用，
`group_settings` 里可按群关掉单个功能 key（`gacha` / `name` / `tarot` / `activity` /
`trainer` / `task` / `backup` / `config` / `emergency` / `help`）。改完热重载，不必重启，
面板上也能直接开合。

## 工具链口径（对不上就拒载）

dll 与宿主跑在同一个进程里，而 `arona` 是被静态链接进 dll 的**第二份**，所以装载前要比对
工具链指纹，任何一项不一致都拒载（日志里给中文原因）：

| 项 | 本仓库怎么对齐 |
| --- | --- |
| `arona` 版本 + 除 `gui`/`default` 外的 feature 集合 | 依赖写 `package = "arona-rs", version = "1.1.0"`，且 `default-features = false`（不替宿主决定 GUI）。框架在 crates.io 上的发布名带 `-rs`，库名仍是 `arona`，所以 `use arona::…` 不用改 |
| rustc 版本（`rustc -vV` 的 release 行） | 钉在仓库根的 `rust-toolchain.toml`，`cargo build` 自动选中它；要和宿主对上就得和框架仓库那一行**逐字相同** |
| 目标三元组 | `x86_64-pc-windows-msvc` |
| profile | 宿主是 release，插件也必须 `--release` |
| CRT 链接方式 | `.cargo/config.toml` 里已写 `-C target-feature=+crt-static`，与框架仓库一致 |

一句话：**谁编的 `arona-rs.exe`，就用同一套工具链编这个 dll**。rustc 版本这一项不再靠自觉，
两个仓库各持一份同版本的 `rust-toolchain.toml`；框架那边升版本，这里必须跟着改并重编，
否则老 dll 会全部拒载。更细的规矩（含拒载日志对照表）写在框架仓库的 `PLUGIN_DEVELOPMENT.md`。

## 本地开发

```bash
cargo build --release     # 产出 dll：target/release/bluearchive_plugin.dll
cargo test                # 纯函数离线单测（走 rlib 那份，不需要宿主）
cargo clippy --all-targets
```

`Cargo.toml` 里对框架暂时用的是相邻目录的 path 依赖（`../Arona-rs/crates/arona`），改框架立刻能编。
发布口径是 git 标签或 crates.io 依赖（`arona = { package = "arona-rs", version = "1.1.0", … }`）——
接口需求已经跨过了已发布的标签：符号布局 5 那批（`Bot`/`Group`/`Member` 对象模型、聊天记录库
走宿主侧 trait 出口、`Image` 句柄）不在已推送的 `v1.0.0` 里，符号布局 6 起定时任务改走宿主通道
（`framework.jobs()`）本地 `v1.1.0` 标签（布局 5）也还没有，而框架 HEAD 已经抬到布局 8，所以这里
先停在 path。
要把这个仓库发给别人用时，换成 git 标签或 crates.io 依赖再提交 —— 注释里写着具体那一行。

调试：把编出的 dll 拷进框架运行目录的 `plugins\bluearchive\`，在那个目录里跑
`arona-rs.exe --nogui`（命令行模式，日志直接进控制台）。本 crate 没有 `main`，也**不该有**：
进程、OneBot 连接与 tokio 运行时都归框架，插件只交出 `AronaPlugin` 的实现——
这点和 mirai 的插件 jar 一样。

## 与框架的版本关系

本仓库按 `arona-rs 1.1.0`（符号布局 8、契约 1.0.0）的插件接口开发（`AronaPlugin` 四阶段装配、`PluginRegistrar` 登记、
`PluginContext` 运行期能力）。契约允许破坏性演进、不留兼容层：每次动接口，框架侧会在同一个
提交里把示例插件（框架仓库的 `plugins/hello`）改到能编译，本仓库跟着同步一次。升级框架后
重新 `cargo build --release` 就行——真混了版本，握手会在装载那一步替你挡下来。
