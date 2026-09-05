//! 逃生通道测试：组播被网络禁掉时，用 `DLNA_DEVICE_URL` 直接指定设备。
//!
//! 校园网 / 手机热点常开 AP 隔离，组播根本出不去，这时候扫描再怎么改都没用，
//! 只能手填地址。这条路必须真的能走通，否则遇到那种网络就彻底没辙了。

#[path = "../examples/mock_renderer.rs"]
mod mock;

use getvideo::discovery;

#[tokio::test]
async fn 指定设备地址时跳过扫描直接用() {
    // 不开 SSDP：假设备完全不响应搜索，模拟组播被彻底屏蔽的网络
    let renderer = mock::start(3, false).await.expect("假设备起不来");

    unsafe {
        std::env::set_var("DLNA_DEVICE_URL", &renderer.location);
    }

    let renders = discovery::discover(30).await;

    assert_eq!(renders.len(), 1, "应该正好拿到指定的那一台设备");
    assert_eq!(renders[0].device.friendly_name(), mock::FRIENDLY_NAME);
}
