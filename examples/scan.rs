//! 只扫描、不投屏，把找到的设备打出来。
//!
//! 遇到「扫不到设备」先跑这个，它会把每块网卡各收到多少响应都打出来，
//! 比在主程序里瞎猜快得多：
//!
//! ```text
//! cargo run --example scan
//! cargo run --example scan -- 15      # 扫 15 秒
//! ```

use std::net::Ipv4Addr;

#[tokio::main]
async fn main() {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Debug)
        .format_timestamp(None)
        .init();

    let secs: u64 = std::env::args()
        .nth(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(8);

    let interfaces = getvideo::discovery::usable_ipv4_interfaces();
    println!("本机可用网卡: {interfaces:?}");
    if interfaces.contains(&Ipv4Addr::LOCALHOST) {
        println!("（已把回环算进来，可以扫到本机的 mock_renderer）");
    }
    println!("开始扫描，{secs} 秒……\n");

    let renders = getvideo::discovery::discover(secs).await;

    println!();
    if renders.is_empty() {
        println!("没扫到任何可投屏的设备。");
        println!("排查顺序：");
        println!("  1. 电脑和电视连的是不是同一个 WiFi / 同一个网段");
        println!("  2. Windows 防火墙有没有拦（第一次运行要允许，专用+公用都勾）");
        println!("  3. 路由器 / 热点是不是开了 AP 隔离（校园网、公司网常见）");
        println!("  4. 上面「本机可用网卡」里有没有你实际在用的那块网卡的地址");
        println!("  实在不行：设 DLNA_DEVICE_URL=http://电视IP:端口/描述文件.xml 跳过扫描");
        return;
    }

    println!("扫到 {} 台可投屏设备：", renders.len());
    for (i, render) in renders.iter().enumerate() {
        println!("  [{i}] {}", render.device.friendly_name());
        println!("      类型: {}", render.device.device_type());
        println!("      服务: {}", render.service.service_type());
        println!("      地址: {}", render.device.url());
    }
}
