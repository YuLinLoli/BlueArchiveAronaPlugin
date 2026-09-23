//! 本插件的命令层：命令实现、命令表登记与服务清单。
//!
//! 目录名 `standalone` 沿用上游 Kotlin 原版（diyigemt/arona）的包名，那里确实有独立
//! 进程那一套；在这里没有——本 crate 是 dll 插件，所有命令都由框架路由进来。
//! 也别和框架的 `arona::config::standalone` 搞混，那是框架自己那份 `config/arona.yml`。
pub mod api;
pub mod commands;
pub mod dispatcher;

#[cfg(test)]
mod onebot_print_test;
#[cfg(test)]
mod ten_pull_test;

#[cfg(test)]
mod trainer_test;
