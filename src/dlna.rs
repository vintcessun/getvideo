use anyhow::{Context, Result};
use log::{debug, error, info, warn};
use std::time::Duration;
use xml::escape::escape_str_attribute;

/// 一台可以投屏的 DLNA 设备：设备本体 + 它的 AVTransport 服务。
///
/// 原来这个类型来自 crab-dlna。那个库停更在 0.2.1，设备发现实现有问题
/// （见 [`crate::discovery`]），而且会顺带拖进来 warp、clap 一整套用不上的依赖。
/// 现在只保留真正干活的 rupnp（UPnP 设备描述解析 + SOAP 调用），
/// `Render` 这层薄壳自己定义。
#[derive(Debug, Clone)]
pub struct Render {
    /// UPnP 设备
    pub device: rupnp::Device,
    /// 设备上的 AVTransport 服务
    pub service: rupnp::Service,
}

impl std::fmt::Display for Render {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "[{}][{}] {} @ {}",
            self.device.device_type(),
            self.service.service_type(),
            self.device.friendly_name(),
            self.device.url()
        )
    }
}

const PAYLOAD_PLAY: &str = r#"
    <InstanceID>0</InstanceID>
    <Speed>1</Speed>
"#;

#[derive(Debug, Clone)]
pub struct Media {
    video_url: String,
    video_type: String,
}
impl Media {
    pub fn new(url: &str) -> Self {
        let t = url.split('.').collect::<Vec<_>>();
        let video_type = t[t.len() - 1];
        Self {
            video_url: url.to_string(),
            video_type: video_type.to_string(),
        }
    }
}

pub async fn play(render: Render, url: &str) -> Render {
    loop {
        warn!("开始投屏 url = {}", url);
        match _play(render.clone(), Media::new(url)).await {
            Err(_) => {
                error!("投屏错误 url = {}\n render = {:?}", url, render);
            }
            Ok(ret) => {
                info!("投屏成功");
                info!("render已更新");
                info!("render = {:?}", ret);
                break ret;
            }
        }
    }
}

pub async fn _play(render: Render, streaming_server: Media) -> Result<Render> {
    info!("投屏{}", streaming_server.video_url);
    //let subtitle_uri = streaming_server.video_url.clone();
    let payload_subtitle = escape_str_attribute(
        format!(r###"
            <DIDL-Lite xmlns="urn:schemas-upnp-org:metadata-1-0/DIDL-Lite/"
                xmlns:dc="http://purl.org/dc/elements/1.1/" 
                xmlns:upnp="urn:schemas-upnp-org:metadata-1-0/upnp/" 
                xmlns:dlna="urn:schemas-dlna-org:metadata-1-0/" 
                xmlns:sec="http://www.sec.co.kr/" 
                xmlns:xbmc="urn:schemas-xbmc-org:metadata-1-0/">
                <item id="0" parentID="-1" restricted="1">
                    <dc:title>nano-dlna Video</dc:title>
                    <res protocolInfo="http-get:*:video/{type_video}:" xmlns:pv="http://www.pv.com/pvns/" pv:subtitleFileUri="{uri_sub}" pv:subtitleFileType="{type_sub}">{uri_video}</res>
                    <res protocolInfo="http-get:*:text/srt:*">{uri_sub}</res>
                    <res protocolInfo="http-get:*:smi/caption:*">{uri_sub}</res>
                    <sec:CaptionInfoEx sec:type="{type_sub}">{uri_sub}</sec:CaptionInfoEx>
                    <sec:CaptionInfo sec:type="{type_sub}">{uri_sub}</sec:CaptionInfo>
                    <upnp:class>object.item.videoItem.movie</upnp:class>
                </item>
            </DIDL-Lite>
            "###,
            uri_video = streaming_server.video_url,
            type_video = streaming_server.video_type,
            uri_sub = streaming_server.video_url,
            type_sub = streaming_server.video_type
        ).as_str()).to_string();
    //println!("Subtitle payload");

    let payload_setavtransporturi = format!(
        r#"
        <InstanceID>0</InstanceID>
        <CurrentURI>{}</CurrentURI>
        <CurrentURIMetaData>{}</CurrentURIMetaData>
        "#,
        streaming_server.video_url.clone(),
        payload_subtitle
    );
    //println!("SetAVTransportURI payload");

    //info!("Starting media streaming server...");
    //let streaming_server_handle = tokio::spawn(async move { streaming_server.run().await });

    //println!("Setting Video URI");
    render
        .service
        .action(
            render.device.url(),
            "SetAVTransportURI",
            payload_setavtransporturi.as_str(),
        )
        .await
        .context("SetAVTransportURI 调用失败")?;

    //println!("Playing video");
    render
        .service
        .action(render.device.url(), "Play", PAYLOAD_PLAY)
        .await
        .context("Play 调用失败")?;

    //streaming_server_handle
    //    .await
    //    .map_err(Error::DLNAStreamingError)?;

    Ok(render)
}

pub async fn is_stopped(render: &Render) -> bool {
    let stop = ["STOPPED", "NO_MEDIA_PRESENT"];
    let ret = loop {
        match render
            .service
            .action(render.device.url(), "GetTransportInfo", PAYLOAD_PLAY)
            .await
            .context("GetTransportInfo 调用失败")
        {
            Ok(ret) => {
                break ret;
            }
            Err(_) => {
                error!("状态查询失败正在重试")
            }
        }
    };
    debug!("获取到 ret = {:?}", ret);
    if ret.is_empty() {
        return true;
    } else if ret.contains_key("CurrentTransportState") {
        let state = ret["CurrentTransportState"].clone();
        debug!("DLNA设备状态{}", state);
        if stop.contains(&state.as_str()) {
            return true;
        }
    }
    false
}

/// 查询播放状态的间隔。
///
/// 原来主循环里是 `while !is_stopped(..) {}` 空转，一秒能往设备打出去几十上百个
/// `GetTransportInfo`。真设备（尤其是便宜的国产盒子）扛不住这个频率，
/// 轻则卡顿重则不响应。真正的 DLNA 控制端一般 1~2 秒查一次。
pub const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// 默认扫描时长（秒）。可用环境变量 `DLNA_SCAN_SECS` 调整。
const DEFAULT_SCAN_SECS: u64 = 6;

fn scan_secs() -> u64 {
    std::env::var("DLNA_SCAN_SECS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(DEFAULT_SCAN_SECS)
}

/// 扫描局域网内可投屏的 DLNA 设备。
///
/// 原来这里是 `Render::discover(20)`，也就是 crab-dlna 自带的实现，
/// 在 Windows 多网卡（VPN / TAP 虚拟网卡）环境下基本扫不到东西，
/// 具体原因见 [`crate::discovery`] 的模块注释。现在换成自己的实现。
///
/// 返回空列表表示没扫到，不再当成错误往上抛 —— 调用方需要「没扫到就重试」，
/// 而不是整个程序退出。
pub async fn discover() -> Result<Vec<Render>> {
    Ok(crate::discovery::discover(scan_secs()).await)
}
