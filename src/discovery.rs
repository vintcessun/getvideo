//! 自建 SSDP 设备发现。
//!
//! 为什么不直接用 crab-dlna 的 `Render::discover`：它底层是 ssdp-client 2.1.0，
//! 在 Windows 上有五个会导致「扫不到设备」的问题，而这两个库都已经停更
//! （crab-dlna 卡在 0.2.1、ssdp-client 卡在 2.1.0），升级解决不了：
//!
//! 1. **只在一块网卡上搜索。** ssdp-client 用 `connect(8.8.8.8:80)` 反查本机 IP，
//!    拿到的是「默认路由」那块网卡。本机装了 OpenVPN / SecureLink 等一堆虚拟网卡，
//!    VPN 一连默认路由就跑进隧道，M-SEARCH 组播包发给了 VPN，电视永远收不到。
//! 2. **一次搜索只发一个组播包。** UDP 组播本来就允许丢包，丢一个就等于没搜。
//! 3. **只发 `urn:schemas-upnp-org:service:AVTransport:1` 一种 ST。**
//!    很多国产盒子（FastCast / 乐播 / 当贝等）只回应 `ssdp:all` 或 MediaRenderer，
//!    对「按服务查询」的 ST 不理睬 —— 这就是「手机能投屏，程序却扫不到」。
//! 4. **拿到 LOCATION 后串行抓设备描述，而且没有超时。**
//!    网络里有一台不响应的设备，整个扫描就卡死在那台上。
//! 5. **超时语义是「多久没收到包」而不是「总共扫多久」**，行为不可预期。
//!
//! 这里重写一遍：所有可用网卡并发搜索、多种 ST、多轮重发、同时被动监听设备主动
//! 广播的 NOTIFY，最后并发地带超时抓设备描述。

use crate::dlna::Render;
use if_addrs::IfAddr;
use log::{debug, info, warn};
use rupnp::http::Uri;
use rupnp::ssdp::URN;
use socket2::{Domain, Protocol, SockAddr, Socket, Type};
use std::collections::HashSet;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;

const SSDP_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SSDP_PORT: u16 = 1900;

/// 组播 TTL。默认的 2 在部分家用路由 / AP 上会被吃掉，放宽到 4。
const MULTICAST_TTL: u32 = 4;

/// M-SEARCH 的 MX（设备回应前的随机等待上限，单位秒）。
const MX: u32 = 2;

/// 每种 ST 重发几轮，用来对抗组播丢包。
const SEARCH_ROUNDS: usize = 3;

/// 两轮重发之间的间隔。
const RESEND_INTERVAL: Duration = Duration::from_millis(800);

/// 抓单台设备描述 XML 的超时。rupnp 自己不带超时，必须由我们兜底。
const DEVICE_FETCH_TIMEOUT: Duration = Duration::from_secs(5);

/// SSDP 响应缓冲区。ssdp-client 用的 2048 对 `ssdp:all` 的长响应会截断，放大到 8K。
const RECV_BUF: usize = 8192;

/// 依次尝试的搜索目标。不同厂商对 ST 的响应差别很大，全都发一遍才不会漏。
const SEARCH_TARGETS: [&str; 4] = [
    // 最宽的一网打尽，国产盒子基本都认这个
    "ssdp:all",
    // 标准的「媒体渲染器」设备类型
    "urn:schemas-upnp-org:device:MediaRenderer:1",
    // crab-dlna 原本唯一会发的那种
    "urn:schemas-upnp-org:service:AVTransport:1",
    // 有些设备只在根设备查询时应答
    "upnp:rootdevice",
];

/// 手动指定设备描述 XML 地址，跳过扫描。
/// 网络禁用组播（校园网 / 热点的 AP 隔离）时的逃生通道。
const ENV_DEVICE_URL: &str = "DLNA_DEVICE_URL";

/// 手动指定用于搜索的本机网卡 IP，逗号分隔，覆盖自动枚举。
const ENV_INTERFACES: &str = "DLNA_SEARCH_INTERFACES";

/// 设为 1 时把回环网卡也纳入搜索（本地跑模拟设备做测试时用）。
const ENV_LOOPBACK: &str = "DLNA_INCLUDE_LOOPBACK";

/// 列出可以用来发 M-SEARCH 的本机 IPv4 地址。
///
/// 关键点：**枚举全部网卡**，而不是像 ssdp-client 那样只取默认路由那一块。
/// 过滤掉回环和 169.254 自动专用地址（没插网线 / 没连上的虚拟网卡都是这个段），
/// 它们上面不可能有电视。
pub fn usable_ipv4_interfaces() -> Vec<Ipv4Addr> {
    if let Ok(manual) = std::env::var(ENV_INTERFACES) {
        let list: Vec<Ipv4Addr> = manual
            .split(',')
            .filter_map(|s| s.trim().parse().ok())
            .collect();
        if !list.is_empty() {
            info!("使用 {ENV_INTERFACES} 指定的网卡: {list:?}");
            return list;
        }
    }

    // 注意 trim：cmd 里写 `set X=1 && cargo run` 会把值存成 "1 "（带尾空格），
    // 不 trim 的话这个开关会莫名其妙地不生效
    let include_loopback = std::env::var(ENV_LOOPBACK).is_ok_and(|v| v.trim() == "1");

    let ifaces = match if_addrs::get_if_addrs() {
        Ok(ifaces) => ifaces,
        Err(e) => {
            warn!("枚举网卡失败: {e}");
            return Vec::new();
        }
    };

    let mut seen = HashSet::new();
    let mut result = Vec::new();
    for iface in ifaces {
        let IfAddr::V4(v4) = iface.addr else { continue };
        let ip = v4.ip;
        if !is_searchable(&ip, include_loopback) {
            debug!("跳过网卡 {} ({ip})", iface.name);
            continue;
        }
        if seen.insert(ip) {
            debug!("可用网卡 {} ({ip})", iface.name);
            result.push(ip);
        }
    }
    result
}

fn is_searchable(ip: &Ipv4Addr, include_loopback: bool) -> bool {
    if ip.is_unspecified() || ip.is_broadcast() || ip.is_multicast() {
        return false;
    }
    if ip.is_loopback() {
        return include_loopback;
    }
    // 169.254.0.0/16：没拿到 DHCP 的网卡（本机那一堆没连上的 TAP/VPN 网卡都是这个）
    !ip.is_link_local()
}

/// 建一个用于发 M-SEARCH 的 socket，绑定在指定网卡上。
///
/// 这里同时绑定本地地址**并且**设置 `IP_MULTICAST_IF`：Windows 上只绑地址不够，
/// 出向组播走哪块网卡是由 `IP_MULTICAST_IF` 决定的。
fn build_search_socket(local: Ipv4Addr) -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    socket.set_reuse_address(true)?;
    socket.bind(&SockAddr::from(SocketAddrV4::new(local, 0)))?;
    socket.set_multicast_if_v4(&local)?;
    socket.set_multicast_ttl_v4(MULTICAST_TTL)?;
    // 保持开启：本机跑模拟设备做测试时，靠它把组播回环给本地监听者
    socket.set_multicast_loop_v4(true)?;
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into())
}

/// 建一个被动监听 NOTIFY 公告的 socket。
///
/// 设备会周期性地往组播地址广播 `ssdp:alive`，监听它可以捞到那些**不回应 M-SEARCH**
/// 的设备。属于尽力而为：Windows 上 1900 端口被系统的 SSDPSRV 服务占着，
/// 靠 `SO_REUSEADDR` 共享；共享不了就跳过，不影响主路径。
fn build_notify_socket(locals: &[Ipv4Addr]) -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // 必须在 bind 之前设置
    socket.set_reuse_address(true)?;
    // Windows 不允许直接绑组播地址，只能绑 0.0.0.0 再加入组
    socket.bind(&SockAddr::from(SocketAddrV4::new(
        Ipv4Addr::UNSPECIFIED,
        SSDP_PORT,
    )))?;
    let mut joined = 0;
    for local in locals {
        match socket.join_multicast_v4(&SSDP_ADDR, local) {
            Ok(()) => joined += 1,
            Err(e) => debug!("在网卡 {local} 上加入组播组失败: {e}"),
        }
    }
    if joined == 0 {
        return Err(std::io::Error::other("没有任何网卡成功加入 SSDP 组播组"));
    }
    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into())
}

/// 取 HTTP 风格报文里某个头的值，大小写不敏感。
fn header<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim())
            .filter(|v| !v.is_empty())
    })
}

/// SSDP 报文里指向的一台设备。
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Endpoint {
    /// 设备描述 XML 的地址。
    pub location: String,
    /// 设备唯一标识，取 USN 里 `::` 前面那截（一般是 `uuid:...`）。
    ///
    /// 同一台设备会因为两件事被重复发现：一是它同时回应了我们发的好几种 ST，
    /// 二是它有多个 IP（有线 + 无线，或者我们从多块网卡都问到了它），
    /// 这时 location 不一样但 USN 里的 uuid 是同一个。按 location 去重会让
    /// 同一台电视在选择列表里出现好几次，所以要按这个标识去重。
    pub key: String,
}

/// 从 SSDP 报文里解析出一台设备。
///
/// 同时认两种报文：M-SEARCH 的单播响应（`HTTP/1.1 200 OK`）和设备主动广播的
/// `NOTIFY`。NOTIFY 里只接受 alive/update，`ssdp:byebye` 是设备下线，要丢掉。
pub fn parse_endpoint(text: &str) -> Option<Endpoint> {
    let first = text.lines().next()?.trim();
    let upper = first.to_ascii_uppercase();

    if upper.starts_with("HTTP/1.") {
        // 只要成功响应
        if upper.split_whitespace().nth(1) != Some("200") {
            return None;
        }
    } else if upper.starts_with("NOTIFY") {
        let nts = header(text, "NTS")?;
        if !nts.eq_ignore_ascii_case("ssdp:alive") && !nts.eq_ignore_ascii_case("ssdp:update") {
            return None;
        }
    } else {
        // M-SEARCH 请求本身（自己发的组播回环）等等，忽略
        return None;
    }

    let location = header(text, "LOCATION")?.to_string();
    // 没给 USN 的设备（不太规范但确实存在）就退回用地址当标识
    let key = header(text, "USN")
        .and_then(|usn| usn.split("::").next())
        .map(str::trim)
        .filter(|k| !k.is_empty())
        .unwrap_or(&location)
        .to_string();

    Some(Endpoint { location, key })
}

/// 在一块网卡上搜索，返回收到的所有设备。
async fn search_on_interface(local: Ipv4Addr, timeout: Duration) -> HashSet<Endpoint> {
    let socket = match build_search_socket(local) {
        Ok(socket) => Arc::new(socket),
        Err(e) => {
            warn!("网卡 {local} 无法用于搜索: {e}");
            return HashSet::new();
        }
    };

    let deadline = Instant::now() + timeout;
    let dest = SocketAddr::from(SocketAddrV4::new(SSDP_ADDR, SSDP_PORT));

    // 发送和接收并行：一边分轮次重发，一边持续收响应
    let sender = tokio::spawn({
        let socket = Arc::clone(&socket);
        async move {
            for round in 0..SEARCH_ROUNDS {
                for st in SEARCH_TARGETS {
                    let msg = msearch_message(st);
                    if let Err(e) = socket.send_to(msg.as_bytes(), dest).await {
                        debug!("网卡 {local} 第 {} 轮发送 {st} 失败: {e}", round + 1);
                    }
                    // 稍微岔开，别让设备一次收到四个包直接丢
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                tokio::time::sleep(RESEND_INTERVAL).await;
            }
        }
    });

    let found = recv_endpoints(&socket, deadline, &format!("网卡 {local}")).await;
    sender.abort();

    // 按网卡分别记数：用户报「还是扫不到」时，这一行能立刻区分
    // 「包根本没发出去」和「发出去了但没人回」
    info!("网卡 {local} 收到 {} 个设备响应", found.len());
    found
}

fn msearch_message(search_target: &str) -> String {
    let lines = [
        "M-SEARCH * HTTP/1.1".to_string(),
        format!("HOST: {SSDP_ADDR}:{SSDP_PORT}"),
        "MAN: \"ssdp:discover\"".to_string(),
        format!("ST: {search_target}"),
        format!("MX: {MX}"),
        "USER-AGENT: Windows/10 UPnP/1.0 getvideo/1.0".to_string(),
    ];
    format!("{}\r\n\r\n", lines.join("\r\n"))
}

/// 被动监听设备主动广播的 NOTIFY。
async fn listen_notify(locals: &[Ipv4Addr], timeout: Duration) -> HashSet<Endpoint> {
    let socket = match build_notify_socket(locals) {
        Ok(socket) => socket,
        Err(e) => {
            // 1900 被占用是 Windows 上的常态，不是错误
            debug!("无法监听 SSDP 公告（1900 端口可能被系统 SSDPSRV 占用）: {e}");
            return HashSet::new();
        }
    };
    let deadline = Instant::now() + timeout;
    let found = recv_endpoints(&socket, deadline, "NOTIFY 公告").await;
    if !found.is_empty() {
        info!("从设备主动公告中捞到 {} 个设备", found.len());
    }
    found
}

/// 在截止时间前持续收包并解析出设备。
async fn recv_endpoints(socket: &UdpSocket, deadline: Instant, who: &str) -> HashSet<Endpoint> {
    let mut found = HashSet::new();
    let mut buf = vec![0u8; RECV_BUF];

    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        let (n, from) = match tokio::time::timeout(remaining, socket.recv_from(&mut buf)).await {
            // 到点了，正常结束
            Err(_) => break,
            Ok(Ok(pair)) => pair,
            Ok(Err(e)) => {
                // Windows 特有：之前发出去的包收到 ICMP 端口不可达时，
                // 下一次 recv 会返回 WSAECONNRESET。这不是致命错误，绝对不能就此退出，
                // 否则一台拒收的设备就能让整轮扫描提前结束。
                debug!("{who} 收包出错（已忽略继续等）: {e}");
                continue;
            }
        };

        let text = String::from_utf8_lossy(&buf[..n]);
        match parse_endpoint(&text) {
            Some(endpoint) => {
                if found.insert(endpoint.clone()) {
                    debug!(
                        "{who} 从 {from} 发现: {} ({})",
                        endpoint.location, endpoint.key
                    );
                }
            }
            None => debug!("{who} 忽略来自 {from} 的报文"),
        }
    }

    found
}

/// 抓一台设备的描述 XML，如果它支持 AVTransport 就包成 `Render`。
pub async fn fetch_render(location: &str) -> Option<Render> {
    let uri: Uri = match location.parse() {
        Ok(uri) => uri,
        Err(e) => {
            debug!("设备地址无法解析 {location}: {e}");
            return None;
        }
    };

    let device =
        match tokio::time::timeout(DEVICE_FETCH_TIMEOUT, rupnp::Device::from_url(uri)).await {
            Err(_) => {
                debug!("获取设备描述超时（{DEVICE_FETCH_TIMEOUT:?}）: {location}");
                return None;
            }
            Ok(Err(e)) => {
                debug!("获取设备描述失败 {location}: {e}");
                return None;
            }
            Ok(Ok(device)) => device,
        };

    // 不要求版本号必须是 1：有的渲染器只提供 AVTransport:2 / :3，
    // crab-dlna 硬匹配 `AVTransport:1` 会把它们全部漏掉。
    let service = match device
        .services_iter()
        .find(|s| is_av_transport(s.service_type()))
    {
        Some(service) => service.clone(),
        None => {
            debug!(
                "设备「{}」不支持 AVTransport，跳过 ({location})",
                device.friendly_name()
            );
            return None;
        }
    };

    info!(
        "可投屏设备: {} [{}] @ {}",
        device.friendly_name(),
        service.service_type(),
        device.url()
    );
    Some(Render { device, service })
}

fn is_av_transport(urn: &URN) -> bool {
    matches!(urn, URN::Service(_, typ, _) if typ.eq_ignore_ascii_case("AVTransport"))
}

/// 扫描局域网内所有支持 AVTransport 的 DLNA 设备。
///
/// 不会返回 `Err`：扫不到就是空列表。单块网卡出错、某台设备抓不动，都只影响它自己。
pub async fn discover(timeout_secs: u64) -> Vec<Render> {
    // 逃生通道：网络禁了组播时直接指定设备地址
    if let Ok(url) = std::env::var(ENV_DEVICE_URL) {
        let url = url.trim().to_string();
        if !url.is_empty() {
            info!("使用 {ENV_DEVICE_URL} 指定的设备: {url}");
            return fetch_render(&url).await.into_iter().collect();
        }
    }

    let timeout = Duration::from_secs(timeout_secs.clamp(1, 60));
    let interfaces = usable_ipv4_interfaces();

    if interfaces.is_empty() {
        warn!(
            "没有找到可用网卡（全是回环或 169.254 未连接地址）。\
             请确认电脑已连上和电视同一个局域网。"
        );
        return Vec::new();
    }

    info!(
        "在 {} 块网卡上搜索 DLNA 设备，等待 {} 秒: {}",
        interfaces.len(),
        timeout.as_secs(),
        interfaces
            .iter()
            .map(Ipv4Addr::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    );

    // 所有网卡 + NOTIFY 监听全部并发，总耗时就是 timeout，不会叠加
    let searches = futures::future::join_all(
        interfaces
            .iter()
            .map(|ip| search_on_interface(*ip, timeout)),
    );
    let (per_interface, notified) = tokio::join!(searches, listen_notify(&interfaces, timeout));

    let mut endpoints: HashSet<Endpoint> = notified;
    for set in per_interface {
        endpoints.extend(set);
    }

    if endpoints.is_empty() {
        warn!(
            "没有收到任何 SSDP 响应。常见原因：\
             ① 电脑和电视不在同一个局域网 / 不在同一个 VLAN；\
             ② Windows 防火墙拦了本程序（第一次运行要点「允许访问」，专用和公用网络都勾上）；\
             ③ 路由器或热点开了 AP 隔离、禁用了组播（校园网常见）。\
             实在扫不到可以设环境变量 {ENV_DEVICE_URL} 指定设备描述地址绕过扫描。"
        );
        return Vec::new();
    }

    let device_count = endpoints
        .iter()
        .map(|e| e.key.as_str())
        .collect::<HashSet<_>>()
        .len();
    info!(
        "共发现 {device_count} 台 UPnP 设备（{} 个地址），开始获取详情",
        endpoints.len()
    );

    // 并发抓取并各自超时：一台没响应的设备不会再拖住整轮扫描。
    // 同一台设备的多个地址全都试一遍，其中一个不通还有别的兜底。
    let fetched = futures::future::join_all(endpoints.iter().map(|endpoint| async move {
        (
            endpoint.key.as_str(),
            fetch_render(&endpoint.location).await,
        )
    }))
    .await;

    let mut seen = HashSet::new();
    let mut renders = Vec::new();
    for (key, render) in fetched {
        let Some(render) = render else { continue };
        if seen.insert(key) {
            renders.push(render);
        } else {
            debug!("设备 {key} 已经通过别的地址找到过了，跳过重复项");
        }
    }

    if renders.is_empty() {
        warn!(
            "发现了 {device_count} 台 UPnP 设备，但没有一台支持 AVTransport（都不是能投屏的设备）。"
        );
    } else {
        info!("最终找到 {} 台可投屏设备", renders.len());
    }

    renders
}

#[cfg(test)]
mod tests {
    use super::*;

    const SEARCH_RESPONSE: &str = concat!(
        "HTTP/1.1 200 OK\r\n",
        "CACHE-CONTROL: max-age=1800\r\n",
        "EXT:\r\n",
        "LOCATION: http://192.168.1.20:8200/rootDesc.xml\r\n",
        "SERVER: Linux/3.10 UPnP/1.0 FastCast/1.0\r\n",
        "ST: urn:schemas-upnp-org:service:AVTransport:1\r\n",
        "USN: uuid:abcd::urn:schemas-upnp-org:service:AVTransport:1\r\n\r\n"
    );

    #[test]
    fn 解析_msearch_响应() {
        let endpoint = parse_endpoint(SEARCH_RESPONSE).unwrap();
        assert_eq!(endpoint.location, "http://192.168.1.20:8200/rootDesc.xml");
        assert_eq!(endpoint.key, "uuid:abcd");
    }

    #[test]
    fn 同一台设备的不同响应算同一台() {
        // 我们会发好几种 ST，同一台设备每种都回一次，USN 后半截不一样但 uuid 相同；
        // 设备有多个 IP（有线 + 无线）时 location 也会不一样。这些都得归并成一台，
        // 否则同一台电视会在选择列表里出现好几遍。
        let another = SEARCH_RESPONSE
            .replace(
                "USN: uuid:abcd::urn:schemas-upnp-org:service:AVTransport:1",
                "USN: uuid:abcd::upnp:rootdevice",
            )
            .replace("192.168.1.20", "10.0.0.7");
        let a = parse_endpoint(SEARCH_RESPONSE).unwrap();
        let b = parse_endpoint(&another).unwrap();

        assert_ne!(a.location, b.location, "两个地址本来就不一样");
        assert_eq!(a.key, b.key, "但应该被认成同一台设备");
    }

    #[test]
    fn 没有_usn_时退回用地址当标识() {
        let msg =
            "HTTP/1.1 200 OK\r\nLOCATION: http://192.168.1.40:80/d.xml\r\nST: ssdp:all\r\n\r\n";
        let endpoint = parse_endpoint(msg).unwrap();
        assert_eq!(endpoint.key, endpoint.location);
    }

    #[test]
    fn 头部大小写不敏感() {
        let msg = SEARCH_RESPONSE.replace("LOCATION:", "Location:");
        assert!(parse_endpoint(&msg).is_some());
        assert_eq!(
            header(SEARCH_RESPONSE, "server").unwrap(),
            "Linux/3.10 UPnP/1.0 FastCast/1.0"
        );
    }

    #[test]
    fn 非_200_响应被丢弃() {
        let msg = SEARCH_RESPONSE.replace("HTTP/1.1 200 OK", "HTTP/1.1 404 Not Found");
        assert_eq!(parse_endpoint(&msg), None);
    }

    #[test]
    fn 接受_notify_alive() {
        let msg = concat!(
            "NOTIFY * HTTP/1.1\r\n",
            "HOST: 239.255.255.250:1900\r\n",
            "LOCATION: http://192.168.1.30:49152/desc.xml\r\n",
            "NT: urn:schemas-upnp-org:device:MediaRenderer:1\r\n",
            "NTS: ssdp:alive\r\n",
            "USN: uuid:ef01::urn:schemas-upnp-org:device:MediaRenderer:1\r\n\r\n"
        );
        let endpoint = parse_endpoint(msg).unwrap();
        assert_eq!(endpoint.location, "http://192.168.1.30:49152/desc.xml");
        assert_eq!(endpoint.key, "uuid:ef01");
    }

    #[test]
    fn 丢弃_notify_byebye() {
        // 设备下线的公告不能当成「发现了设备」
        let msg = concat!(
            "NOTIFY * HTTP/1.1\r\n",
            "HOST: 239.255.255.250:1900\r\n",
            "LOCATION: http://192.168.1.30:49152/desc.xml\r\n",
            "NTS: ssdp:byebye\r\n\r\n"
        );
        assert_eq!(parse_endpoint(msg), None);
    }

    #[test]
    fn 丢弃自己发出去的_msearch() {
        // 开了组播回环之后自己会收到自己的包，不能被它干扰
        assert_eq!(parse_endpoint(&msearch_message("ssdp:all")), None);
    }

    #[test]
    fn 缺少_location_头() {
        let msg = "HTTP/1.1 200 OK\r\nST: ssdp:all\r\nUSN: uuid:abcd\r\n\r\n";
        assert_eq!(parse_endpoint(msg), None);
    }

    #[test]
    fn msearch_报文格式合法() {
        let msg = msearch_message("ssdp:all");
        assert!(msg.starts_with("M-SEARCH * HTTP/1.1\r\n"));
        assert!(msg.ends_with("\r\n\r\n"));
        assert!(msg.contains("MAN: \"ssdp:discover\""));
        assert!(msg.contains("HOST: 239.255.255.250:1900"));
        assert!(msg.contains("ST: ssdp:all"));
    }

    #[test]
    fn 网卡过滤规则() {
        // 没连上的网卡（含本机那一堆 TAP / VPN 虚拟网卡）
        assert!(!is_searchable(&Ipv4Addr::new(169, 254, 113, 49), false));
        // 正常局域网地址
        assert!(is_searchable(&Ipv4Addr::new(10, 142, 195, 34), false));
        assert!(is_searchable(&Ipv4Addr::new(192, 168, 1, 5), false));
        // 回环默认排除，测试时可以打开
        assert!(!is_searchable(&Ipv4Addr::LOCALHOST, false));
        assert!(is_searchable(&Ipv4Addr::LOCALHOST, true));
        assert!(!is_searchable(&Ipv4Addr::UNSPECIFIED, false));
    }

    #[test]
    fn 识别任意版本的_avtransport() {
        // 只认 AVTransport:1 正是原来会漏掉设备的原因
        assert!(is_av_transport(&URN::service(
            "schemas-upnp-org",
            "AVTransport",
            1
        )));
        assert!(is_av_transport(&URN::service(
            "schemas-upnp-org",
            "AVTransport",
            3
        )));
        assert!(!is_av_transport(&URN::service(
            "schemas-upnp-org",
            "RenderingControl",
            1
        )));
        assert!(!is_av_transport(&URN::device(
            "schemas-upnp-org",
            "AVTransport",
            1
        )));
    }

    #[test]
    fn 至少发一种最宽的搜索目标() {
        // 只发 AVTransport:1 正是原来扫不到国产盒子的原因
        assert!(SEARCH_TARGETS.contains(&"ssdp:all"));
        assert!(SEARCH_TARGETS.len() > 1);
    }
}
