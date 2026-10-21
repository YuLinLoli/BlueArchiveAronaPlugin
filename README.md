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
   cargo build --release                 # Windows：target\release\bluearchive_plugin.dll
   # Linux：cargo zigbuild --release --target x86_64-unknown-linux-gnu.2.17
   #        产物 target/x86_64-unknown-linux-gnu/release/libbluearchive_plugin.so
   ```

2. 把 dll 放进框架的运行目录，每个插件独占一层以 id 命名的子目录（推荐，方便另放资源）：

   ```
   <框架运行目录>\plugins\bluearchive\bluearchive_plugin.dll
   ```

   默认安装位置是 `%LOCALAPPDATA%\Programs\Arona-rs\`（装成「为所有用户」时在
   `Program Files\Arona-rs\`）。直接把 dll 丢在 `plugins\` 根下也认。

3. 启动框架。日志里应该有握手通过与装载成功两行，GUI 的「插件管理」页同时就能看到它：

   ```
   [Arona] 插件 ABI 握手通过: 布局 10
   [Arona] 已装载动态插件: BluearchivePlugin 0.4.6 (bluearchive_plugin.dll)
   ```

   Linux 上同一行的文件名是 `libbluearchive_plugin.so`。

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
| 除 `gui`/`default` 外的 feature 集合 | 依赖写 `package = "arona-rs", version = "1.1.0"`，且 `default-features = false`（不替宿主决定 GUI）。框架在 crates.io 上的发布名带 `-rs`，库名仍是 `arona`，所以 `use arona::…` 不用改。**crate 版本本身自框架 1.2.1 起不再进指纹**，只靠布局号与这几项把关 |
| rustc 版本（`rustc -vV` 的 release 行） | 钉在仓库根的 `rust-toolchain.toml`，`cargo build` 自动选中它；要和宿主对上就得和框架仓库那一行**逐字相同** |
| 目标三元组 | Windows 是 `x86_64-pc-windows-msvc`，Linux 是 `x86_64-unknown-linux-gnu`——**两平台的产物不能互换**，把 dll 改名叫 `.so` 会被当场拒载 |
| profile | 宿主是 release，插件也必须 `--release` |
| CRT 链接方式 | `.cargo/config.toml` 里已写 `-C target-feature=+crt-static`，与框架仓库一致，且**只对 MSVC 目标生效**。Linux 不静态链接运行库：musl 静态下 `dlopen` 是空桩，静态化了插件反而装不进去。CI 另用 `cargo zigbuild` 把 glibc 钉在 2.17，于是框架的标准包与 compat 包宿主装的是同一份 `.so` |

一句话：**谁编的 `arona-rs.exe`，就用同一套工具链编这个 dll**。rustc 版本这一项不再靠自觉，
两个仓库各持一份同版本的 `rust-toolchain.toml`。框架 1.2.1 起握手只比布局号 + 工具链指纹，
所以「框架升版本」本身不再自动让老 dll 失效；但只要指纹里任何一段变了（rustc、target、
profile、crt-static、feature 集合），或布局号抬了，就必须重编。1.2.0 及更早编的插件在 1.2.1
宿主上正属于这一类：指纹串去掉了 `arona/<版本>` 前缀，而比对是全等，于是必然对不上。
更细的规矩（含拒载日志对照表）写在框架仓库的 `PLUGIN_DEVELOPMENT.md`。

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
（`framework.jobs()`）本地 `v1.1.0` 标签（布局 5）也还没有，而框架 HEAD 已经抬到布局 10，所以这里
先停在 path。
要把这个仓库发给别人用时，换成 git 标签或 crates.io 依赖再提交 —— 注释里写着具体那一行。

调试：把编出的 dll 拷进框架运行目录的 `plugins\bluearchive\`，在那个目录里跑
`arona-rs.exe --nogui`（命令行模式，日志直接进控制台）。本 crate 没有 `main`，也**不该有**：
进程、OneBot 连接与 tokio 运行时都归框架，插件只交出 `AronaPlugin` 的实现——
这点和 mirai 的插件 jar 一样。

## 内存与字体

- **字面那一层不在本插件里**，用框架的 `arona::runtime::text`（2026-10-17 先换惰性后端、同日三份重复实现
  并成这一份）。它装的是"字体文件的原始字节 + 按 (字符, 字号) 计的有界字形缓存 + `PxScale` 换算 +
  Weak 缓存"，画到哪个字才解析哪个字。换掉的 fontdue 那条路会把字体文件里**每个码位**的轮廓全量预解析后
  常驻，实测本机命中 msyh 那一档是 160.6 MB（simhei 126、simsun 156、Noto 170——只跟字形个数与轮廓
  复杂度走，跟文件多大无关，换小文件省不下来）。惰性这一档常驻就是文件字节那一档，装载 0.01 s。
  换后端**不改观感**：两档的几何逐字比过（TrueType 与 CFF 两类轮廓都验），包围盒与 advance
  最大偏差 0 格、0.0 px。
  两个口径由框架保证与 fontdue 一致，图上的字才不会跳：`PxScale` 的基准是
  `height_unscaled() = ascent - descent`（雅黑 1320）而不是 `unitsPerEm`（1000），不换算整张图的文字
  会整体小 24%；`line_metrics` 是 hhea 三数乘 `px / units_per_em`、`new_line_size = ascent - descent + line_gap`，
  行盒高度对不上就是这里。
- **字面取哪一份**：框架那三级（配置指定 > 安装目录 `fonts/` 的思源宋体 > 系统候选），与 GUI、
  bilibili、petpet 同一个出口，所以部署机上这张图的字体与面板一致、不随机器漂。三级都空时本插件
  还有一层自己的兜底——**递归扫系统字体目录**，逐个文件试 `renders('活')` 挑第一个能画中文的
  （另两个插件只认一张固定候选表，这一张表是本插件特有的）。
- **只要常规那一档**：本插件没有粗体（原版 java.awt 那边也没画过粗体中文），所以走
  `resolve` + `acquire((regular, None))` 而不是 `discover`——后者会把候选表里配对的 `msyhbd.ttc`
  一起装进来，白占 11 MB（34.9 → 18.8 MB 就是这一档的差）。
- **字面只在这次出图期间存在**：框架那层缓存只存 `Weak`，谁出图谁拿 `Arc`（`text::acquire()`），
  最后一个落下就整个还给分配器。惰性后端下装载本来就只要 0.01 s（fontdue 那一档是 0.34~0.36 s），
  所以"还掉再取"几乎不要钱——但口径没变，仍然不常驻。
- **集合字体只认第 0 面**（`FontArc` 没有面索引这一档）。随包的思源宋体是单面 `.otf`，系统那些中文字体
  第 0 面就是常规字面，所以这一档实际没被用到；真要指定某一面，把自己那份拆成单面文件放进 `fonts/`。
- **忘取字面是编译不过，不是悄悄画一张没字的图**：`measure`/`line_metrics`/`draw_line`/`draw_centered`
  都显式收 `&Fonts`（它是框架 `Typeface` 的 newtype），原来的 `available()` 已删。`gacha` 那两处
  "有字体才画字"改成 `Option<&Fonts>`，字体缺时照旧出无字图、不拒回。
- **读数怎么量**：`src/memprobe.rs` 是只在 `cfg(test)` 编译的计数分配器（生产 dll 不含），给出存活字节、
  高水位、以及 drop 之后回不回得到基线，另从 `K32GetProcessMemoryInfo` 取工作集与私有字节
  （任务管理器「内存」那一列就是工作集）。跑法：
  `cargo test --release --lib -- --ignored --test-threads=1 --nocapture the_font_lives_only`。
  本机实测（收进框架那一份实现之后）：取字面 **18.8 MB**、装载 0.01 s → drop 之后 **0.0 MB**，
  工作集 6.8 → 7.2 MB、私有 1.3 → 1.3 MB、进程峰值工作集 26.1 MB；
  出图期间第二次 `acquire` 多占 0.0 MB、耗时 0.168 ms（查表 + 两次 Arc 计数，不再读盘）。
  两条都钉成了断言。换之前同一枚探针的读数：字面 160.6 MB、解析 0.34~0.36 s。

## 与框架的版本关系

本仓库按 `arona-rs 1.2.1`（符号布局 10）的插件接口开发（`AronaPlugin` 四阶段装配、`PluginRegistrar` 登记、
`PluginContext` 运行期能力）。接口允许破坏性演进、不留兼容层：每次动接口，框架侧会在同一个
提交里把示例插件（框架仓库的 `plugins/hello`）改到能编译，本仓库跟着同步一次。升级框架后
重新 `cargo build --release` 就行——真混了版本，握手会在装载那一步替你挡下来。
