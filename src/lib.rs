//! 从 xmtv 抓戏曲视频链接并投屏到局域网里的 DLNA 设备。
//!
//! 拆成 lib 是为了让集成测试能直接调 [`discovery`] 和 [`dlna`]
//! —— 设备扫描这块光靠手动连电视试，出了问题根本没法定位。

pub mod data_store;
pub mod discovery;
pub mod dlna;
