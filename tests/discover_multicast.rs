//! 真·自动扫描测试：起一台假电视，然后走完整的 SSDP 组播流程把它扫出来。
//!
//! 这个测试跑的是和面对真电视时**一模一样**的代码路径 —— 发 M-SEARCH 组播、
//! 收单播响应、抓设备描述、筛出支持 AVTransport 的 —— 只是对面换成了本机的假设备。
//!
//! 单独一个测试文件是因为它要改进程级的环境变量，和别的测试放一起会打架。

#[path = "../examples/mock_renderer.rs"]
mod mock;

use getvideo::discovery;
use std::time::Duration;

#[tokio::test]
async fn 自动扫描能发现设备() {
    // 假电视在回环上也监听，把回环纳入搜索范围，这样断网时测试照样能跑
    unsafe {
        std::env::set_var("DLNA_INCLUDE_LOOPBACK", "1");
    }

    let interfaces = discovery::usable_ipv4_interfaces();
    assert!(
        interfaces.contains(&std::net::Ipv4Addr::LOCALHOST),
        "打开 DLNA_INCLUDE_LOOPBACK 之后应该把回环算进来，实际: {interfaces:?}"
    );

    let renderer = mock::start(5, true).await.expect("假设备起不来");
    // 等 SSDP 监听真正就绪，免得 M-SEARCH 发早了没人接
    tokio::time::sleep(Duration::from_millis(300)).await;

    let renders = discovery::discover(6).await;

    let names: Vec<String> = renders
        .iter()
        .map(|r| r.device.friendly_name().to_string())
        .collect();
    println!("扫到的设备: {names:?}");

    assert!(
        names.iter().any(|n| n == mock::FRIENDLY_NAME),
        "自动扫描应该能发现假设备「{}」，实际扫到: {names:?}\n\
         （假设备的地址是 {}）",
        mock::FRIENDLY_NAME,
        renderer.location
    );

    // 同一台设备会通过多个 ST 回好几次，去重必须生效
    let mock_count = names.iter().filter(|n| *n == mock::FRIENDLY_NAME).count();
    assert_eq!(mock_count, 1, "同一台设备不该重复出现: {names:?}");
}
