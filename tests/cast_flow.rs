//! 投屏全流程测试：识别设备 → 投屏 → 轮询播放状态 → 播完。
//!
//! 这条路径不碰组播，所以在任何环境（包括断网的 CI）里都能稳定跑。

#[path = "../examples/mock_renderer.rs"]
mod mock;

use getvideo::{discovery, dlna};
use std::time::Duration;

/// 兜底超时：`dlna::play` 和 `is_stopped` 内部是「失败就无限重试」，
/// 假设备一旦回错东西测试会挂死，套一层超时让它失败得干脆点。
const LIMIT: Duration = Duration::from_secs(20);

#[tokio::test]
async fn 从设备地址一路投屏到播放结束() {
    let result = tokio::time::timeout(LIMIT, async {
        // 每次 Play 之后报告 2 次 PLAYING 就转 STOPPED
        let renderer = mock::start(2, false).await.expect("假设备起不来");

        // ① 抓设备描述，认出它支持 AVTransport
        let render = discovery::fetch_render(&renderer.location)
            .await
            .expect("应该识别出这是一台可投屏的设备");
        assert_eq!(render.device.friendly_name(), mock::FRIENDLY_NAME);
        let shown = format!("{render}");
        assert!(shown.contains("AVTransport"), "展示给用户的名字里应该有服务类型: {shown}");
        assert!(shown.contains(mock::FRIENDLY_NAME), "展示的名字不对: {shown}");

        // ② 投屏
        let url = "http://192.168.1.9:8000/xiqu/ep1.mp4";
        let render = dlna::play(render, url).await;
        assert_eq!(
            renderer.current_uri().as_deref(),
            Some(url),
            "设备收到的播放地址不对"
        );
        assert_eq!(renderer.play_count(), 1, "应该正好调用一次 Play");

        // ③ 播放中：is_stopped 必须是 false，否则主循环会以为播完了直接跳下一集
        assert!(!dlna::is_stopped(&render).await, "第 1 次轮询应该还在播");
        assert!(!dlna::is_stopped(&render).await, "第 2 次轮询应该还在播");

        // ④ 播完：转成 STOPPED，主循环靠这个推进到下一集
        assert!(dlna::is_stopped(&render).await, "播完之后应该报告已停止");

        // ⑤ 再投下一集，状态要能重新回到「播放中」
        let next = "http://192.168.1.9:8000/xiqu/ep2.mp4";
        let render = dlna::play(render, next).await;
        assert_eq!(renderer.current_uri().as_deref(), Some(next));
        assert_eq!(renderer.play_count(), 2);
        assert!(!dlna::is_stopped(&render).await, "换集之后应该重新开始播");
    })
    .await;

    assert!(result.is_ok(), "投屏流程超过 {LIMIT:?} 没跑完");
}

#[tokio::test]
async fn 地址打不开时不会卡死() {
    // 端口 1 上不会有东西在听，必须快速失败而不是无限期等待。
    // 原来 crab-dlna 走的 rupnp 抓设备描述没有超时，一台不响应的设备就能拖死整轮扫描。
    let result = tokio::time::timeout(
        LIMIT,
        discovery::fetch_render("http://127.0.0.1:1/desc.xml"),
    )
    .await;

    match result {
        Ok(render) => assert!(render.is_none(), "连不上的地址不该被当成设备"),
        Err(_) => panic!("抓不到设备描述时应该超时返回，而不是一直挂着"),
    }
}

#[tokio::test]
async fn 无法解析的地址直接忽略() {
    assert!(discovery::fetch_render("这不是一个地址").await.is_none());
    assert!(discovery::fetch_render("").await.is_none());
}
