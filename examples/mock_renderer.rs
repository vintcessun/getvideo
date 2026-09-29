//! 一台假的 DLNA 电视，用来在没有真实设备的情况下测试扫描和投屏。
//!
//! 它实现了真设备会做的三件事：
//!
//! 1. 在 `239.255.255.250:1900` 上监听 M-SEARCH，收到就单播回一个带 `LOCATION` 的响应；
//! 2. 开机时往组播地址广播几条 `NOTIFY ssdp:alive`；
//! 3. 起一个 HTTP 服务，提供设备描述 XML 和 AVTransport 的 SOAP 控制端点
//!    （`SetAVTransportURI` / `Play` / `Stop` / `GetTransportInfo`）。
//!
//! 直接跑起来当成一台真电视用：
//!
//! ```text
//! cargo run --example mock_renderer
//!
//! # 另开一个窗口。PowerShell：
//! $env:DLNA_INCLUDE_LOOPBACK="1"
//! cargo run --bin client
//!
//! # cmd.exe 则是（注意 set 后面别留空格）：
//! set DLNA_INCLUDE_LOOPBACK=1
//! cargo run --bin client
//! ```
//!
//! 集成测试通过 `#[path]` 直接把这个文件当模块引进去，所以这里的东西都是 `pub` 的。

use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use socket2::{Domain, Protocol, SockAddr, SockRef, Socket, Type};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

const SSDP_ADDR: Ipv4Addr = Ipv4Addr::new(239, 255, 255, 250);
const SSDP_PORT: u16 = 1900;

/// 假设备的名字。带上 FastCast 是因为 `auto` 那个二进制默认按这个关键字挑设备。
pub const FRIENDLY_NAME: &str = "FastCast 模拟电视";
pub const UDN: &str = "uuid:2f3e5a91-0000-4000-8000-getvideomock1";
pub const AV_TRANSPORT: &str = "urn:schemas-upnp-org:service:AVTransport:1";

const DESC_PATH: &str = "/desc.xml";
const CONTROL_PATH: &str = "/AVTransport/control";
const SCPD_PATH: &str = "/AVTransport/scpd.xml";
const EVENT_PATH: &str = "/AVTransport/event";

/// 一台跑起来了的假设备。
///
/// 三个集成测试各自只用到其中一部分，`allow(dead_code)` 免得没用到的那些报警告。
#[allow(dead_code)]
pub struct MockRenderer {
    /// 设备描述 XML 的地址，等价于真设备 SSDP 响应里的 `LOCATION`。
    pub location: String,
    /// HTTP 服务监听的端口。
    pub port: u16,
    state: Arc<State>,
    _tasks: Vec<tokio::task::JoinHandle<()>>,
}

#[allow(dead_code)]
impl MockRenderer {
    /// 当前被投屏的地址（还没投过就是 `None`）。
    pub fn current_uri(&self) -> Option<String> {
        self.state.current_uri.lock().unwrap().clone()
    }

    /// 收到过多少次 `Play`。
    pub fn play_count(&self) -> u32 {
        self.state.play_count.load(Ordering::SeqCst)
    }
}

struct State {
    /// 还要报告多少次 `PLAYING`，减到 0 就变 `STOPPED`，模拟一集播完。
    playing_polls_left: AtomicU32,
    play_count: AtomicU32,
    current_uri: std::sync::Mutex<Option<String>>,
    /// 每次 `Play` 之后重置成这个值。
    polls_per_video: u32,
}

impl State {
    fn transport_state(&self) -> &'static str {
        if self.current_uri.lock().unwrap().is_none() {
            return "NO_MEDIA_PRESENT";
        }
        // 每查一次少一次，模拟播放进度走完
        let left = self.playing_polls_left.load(Ordering::SeqCst);
        if left == 0 {
            "STOPPED"
        } else {
            self.playing_polls_left.fetch_sub(1, Ordering::SeqCst);
            "PLAYING"
        }
    }
}

/// 启动假设备。
///
/// * `polls_per_video` —— 每次 `Play` 之后，`GetTransportInfo` 报告几次 `PLAYING`
///   才转成 `STOPPED`。测试里给个小数字，手动玩的时候给大一点。
/// * `announce` —— 是否响应 M-SEARCH / 广播 NOTIFY。只测 SOAP 时可以关掉。
pub async fn start(polls_per_video: u32, announce: bool) -> std::io::Result<MockRenderer> {
    let state = Arc::new(State {
        playing_polls_left: AtomicU32::new(0),
        play_count: AtomicU32::new(0),
        current_uri: std::sync::Mutex::new(None),
        polls_per_video,
    });

    // 绑 0.0.0.0 让局域网里的机器也能访问，端口交给系统分配，避免和别的东西撞
    let listener = TcpListener::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    let port = listener.local_addr()?.port();

    let mut tasks = Vec::new();
    tasks.push(tokio::spawn({
        let state = Arc::clone(&state);
        async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        let state = Arc::clone(&state);
                        tokio::spawn(async move {
                            let _ = serve_http(stream, state, port).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("[mock] accept 失败: {e}");
                        return;
                    }
                }
            }
        }
    }));

    if announce {
        match build_ssdp_socket() {
            Ok(socket) => {
                let socket = Arc::new(socket);
                tasks.push(tokio::spawn(respond_to_searches(Arc::clone(&socket), port)));
                tasks.push(tokio::spawn(announce_alive(socket, port)));
            }
            Err(e) => eprintln!("[mock] 无法监听 SSDP（1900 端口可能被占）: {e}"),
        }
    }

    Ok(MockRenderer {
        location: format!("http://127.0.0.1:{port}{DESC_PATH}"),
        port,
        state,
        _tasks: tasks,
    })
}

impl Drop for MockRenderer {
    fn drop(&mut self) {
        for task in &self._tasks {
            task.abort();
        }
    }
}

// ---------------------------------------------------------------- SSDP 部分

fn build_ssdp_socket() -> std::io::Result<UdpSocket> {
    let socket = Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::UDP))?;
    // Windows 上 1900 被系统的 SSDPSRV 占着，靠这个共享端口
    socket.set_reuse_address(true)?;
    socket.bind(&SockAddr::from(SocketAddrV4::new(
        Ipv4Addr::UNSPECIFIED,
        SSDP_PORT,
    )))?;
    socket.set_multicast_ttl_v4(4)?;
    socket.set_multicast_loop_v4(true)?;

    // 在每一块网卡上都加入组播组，否则只有系统默认那块能收到 M-SEARCH，
    // 这正是被测代码要解决的「多网卡」问题的另一面
    let mut joined = 0;
    for ip in local_ipv4s() {
        if socket.join_multicast_v4(&SSDP_ADDR, &ip).is_ok() {
            joined += 1;
        }
    }
    if joined == 0 {
        return Err(std::io::Error::other("没有网卡能加入 SSDP 组播组"));
    }

    socket.set_nonblocking(true)?;
    UdpSocket::from_std(socket.into())
}

/// 本机所有 IPv4 地址，含回环 —— 假设备要在回环上也能被找到，方便本地测试。
fn local_ipv4s() -> Vec<Ipv4Addr> {
    let mut ips = vec![Ipv4Addr::LOCALHOST];
    for iface in if_addrs::get_if_addrs().unwrap_or_default() {
        let if_addrs::IfAddr::V4(v4) = iface.addr else {
            continue;
        };
        if !v4.ip.is_loopback() && !v4.ip.is_link_local() && !v4.ip.is_unspecified() {
            ips.push(v4.ip);
        }
    }
    ips
}

/// 找出「要把包发给 `peer`，本机会用哪个地址」，用来填 LOCATION。
///
/// 对方在回环上就回 127.0.0.1，在局域网就回对应网卡的地址 —— 真设备也是这个行为。
fn source_ip_for(peer: SocketAddr) -> Ipv4Addr {
    // 不发任何数据，connect 只是让系统按路由表选一块网卡出来
    let probed = std::net::UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))
        .and_then(|probe| probe.connect(peer).and_then(|()| probe.local_addr()));

    match probed {
        Ok(SocketAddr::V4(local)) if !local.ip().is_unspecified() => *local.ip(),
        _ => Ipv4Addr::LOCALHOST,
    }
}

async fn respond_to_searches(socket: Arc<UdpSocket>, port: u16) {
    let mut buf = vec![0u8; 4096];
    loop {
        let (n, from) = match socket.recv_from(&mut buf).await {
            Ok(pair) => pair,
            // Windows 上 ICMP 不可达会让 recv 报错，忽略继续
            Err(_) => continue,
        };
        let text = String::from_utf8_lossy(&buf[..n]).to_string();
        if !text.to_ascii_uppercase().starts_with("M-SEARCH") {
            continue;
        }
        let Some(st) = header(&text, "ST") else {
            continue;
        };
        if !we_answer_to(st) {
            continue;
        }

        // 真设备会按 MX 随机等一小会儿再回，这里也等一下，顺便验证被测代码
        // 的等待时间是够的
        tokio::time::sleep(Duration::from_millis(80)).await;

        let location = format!("http://{}:{port}{DESC_PATH}", source_ip_for(from));
        let response = search_response(st, &location);
        let _ = socket.send_to(response.as_bytes(), from).await;
    }
}

/// 假设备愿意回应哪些搜索目标。
fn we_answer_to(st: &str) -> bool {
    st == "ssdp:all"
        || st == "upnp:rootdevice"
        || st == AV_TRANSPORT
        || st == "urn:schemas-upnp-org:device:MediaRenderer:1"
}

fn search_response(st: &str, location: &str) -> String {
    let lines = [
        "HTTP/1.1 200 OK".to_string(),
        "CACHE-CONTROL: max-age=1800".to_string(),
        "EXT:".to_string(),
        format!("LOCATION: {location}"),
        "SERVER: Windows/10 UPnP/1.0 MockRenderer/1.0".to_string(),
        format!("ST: {st}"),
        format!("USN: {UDN}::{st}"),
    ];
    format!("{}\r\n\r\n", lines.join("\r\n"))
}

/// 开机广播：真设备上电后会往组播地址发几条 alive 公告。
async fn announce_alive(socket: Arc<UdpSocket>, port: u16) {
    let dest = SocketAddr::from(SocketAddrV4::new(SSDP_ADDR, SSDP_PORT));
    for _ in 0..3 {
        for ip in local_ipv4s() {
            let location = format!("http://{ip}:{port}{DESC_PATH}");
            let lines = [
                "NOTIFY * HTTP/1.1".to_string(),
                format!("HOST: {SSDP_ADDR}:{SSDP_PORT}"),
                "CACHE-CONTROL: max-age=1800".to_string(),
                format!("LOCATION: {location}"),
                format!("NT: {AV_TRANSPORT}"),
                "NTS: ssdp:alive".to_string(),
                "SERVER: Windows/10 UPnP/1.0 MockRenderer/1.0".to_string(),
                format!("USN: {UDN}::{AV_TRANSPORT}"),
            ];
            let msg = format!("{}\r\n\r\n", lines.join("\r\n"));
            // tokio 的 UdpSocket 没暴露 IP_MULTICAST_IF，借 socket2 设一下，
            // 保证每块网卡都真的广播到
            let _ = SockRef::from(socket.as_ref()).set_multicast_if_v4(&ip);
            let _ = socket.send_to(msg.as_bytes(), dest).await;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

// ---------------------------------------------------------------- HTTP 部分

async fn serve_http(mut stream: TcpStream, state: Arc<State>, port: u16) -> std::io::Result<()> {
    let (head, body) = read_request(&mut stream).await?;
    let mut parts = head.lines().next().unwrap_or_default().split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();

    let (status, content_type, body) = match (method.as_str(), path.as_str()) {
        ("GET", DESC_PATH) => (
            "200 OK",
            "text/xml; charset=\"utf-8\"",
            description_xml(port),
        ),
        ("GET", SCPD_PATH) => ("200 OK", "text/xml; charset=\"utf-8\"", scpd_xml()),
        ("POST", CONTROL_PATH) => {
            let action = header(&head, "SOAPAction")
                .and_then(|v| v.trim_matches('"').rsplit('#').next())
                .unwrap_or_default()
                .to_string();
            match handle_action(&action, &body, &state) {
                Some(xml) => ("200 OK", "text/xml; charset=\"utf-8\"", xml),
                None => (
                    "500 Internal Server Error",
                    "text/xml; charset=\"utf-8\"",
                    fault_xml(&action),
                ),
            }
        }
        _ => ("404 Not Found", "text/plain", "not found".to_string()),
    };

    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(response.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// 读一个 HTTP 请求，返回（头部, 请求体）。
async fn read_request(stream: &mut TcpStream) -> std::io::Result<(String, String)> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 1024];

    // 先读到头部结束
    let head_end = loop {
        if let Some(pos) = find_subslice(&buf, b"\r\n\r\n") {
            break pos;
        }
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(std::io::Error::other("请求还没读完连接就断了"));
        }
        buf.extend_from_slice(&chunk[..n]);
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut body = buf[head_end + 4..].to_vec();

    // 再按 Content-Length 把请求体读全
    let want: usize = header(&head, "Content-Length")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    while body.len() < want {
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..n]);
    }

    Ok((head, String::from_utf8_lossy(&body).to_string()))
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

fn handle_action(action: &str, body: &str, state: &State) -> Option<String> {
    match action {
        "SetAVTransportURI" => {
            let uri = xml_text(body, "CurrentURI").unwrap_or_default();
            println!("[mock] SetAVTransportURI: {uri}");
            *state.current_uri.lock().unwrap() = Some(uri);
            Some(action_response(action, &[]))
        }
        "Play" => {
            if state.current_uri.lock().unwrap().is_none() {
                // 没设过地址就 Play，真设备会报错
                return None;
            }
            state.play_count.fetch_add(1, Ordering::SeqCst);
            state
                .playing_polls_left
                .store(state.polls_per_video, Ordering::SeqCst);
            println!(
                "[mock] Play（第 {} 次）",
                state.play_count.load(Ordering::SeqCst)
            );
            Some(action_response(action, &[]))
        }
        "Stop" => {
            state.playing_polls_left.store(0, Ordering::SeqCst);
            Some(action_response(action, &[]))
        }
        "GetTransportInfo" => {
            let transport_state = state.transport_state();
            Some(action_response(
                action,
                &[
                    ("CurrentTransportState", transport_state),
                    ("CurrentTransportStatus", "OK"),
                    ("CurrentSpeed", "1"),
                ],
            ))
        }
        other => {
            println!("[mock] 不支持的动作: {other}");
            None
        }
    }
}

/// 从 SOAP 请求体里取一个元素的文本，够用就行，不做完整 XML 解析。
fn xml_text(body: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = body.find(&open)? + open.len();
    let end = body[start..].find(&close)? + start;
    Some(unescape(body[start..end].trim()))
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

fn action_response(action: &str, values: &[(&str, &str)]) -> String {
    let inner: String = values
        .iter()
        .map(|(k, v)| format!("<{k}>{v}</{k}>"))
        .collect();
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <u:{action}Response xmlns:u="{AV_TRANSPORT}">{inner}</u:{action}Response>
  </s:Body>
</s:Envelope>"#
    )
}

fn fault_xml(action: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<s:Envelope xmlns:s="http://schemas.xmlsoap.org/soap/envelope/" s:encodingStyle="http://schemas.xmlsoap.org/soap/encoding/">
  <s:Body>
    <s:Fault>
      <faultcode>s:Client</faultcode>
      <faultstring>UPnPError</faultstring>
      <detail>
        <UPnPError xmlns="urn:schemas-upnp-org:control-1-0">
          <errorCode>401</errorCode>
          <errorDescription>Invalid Action: {action}</errorDescription>
        </UPnPError>
      </detail>
    </s:Fault>
  </s:Body>
</s:Envelope>"#
    )
}

/// 设备描述 XML。
///
/// 注意 `SCPDURL` / `controlURL` / `eventSubURL` 必须写成**路径**而不是完整 URL：
/// rupnp 把它们解析成 `PathAndQuery`，再和设备地址的 host 拼起来。
fn description_xml(port: u16) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<root xmlns="urn:schemas-upnp-org:device-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <device>
    <deviceType>urn:schemas-upnp-org:device:MediaRenderer:1</deviceType>
    <friendlyName>{FRIENDLY_NAME}</friendlyName>
    <manufacturer>getvideo</manufacturer>
    <modelName>Mock Renderer</modelName>
    <modelNumber>{port}</modelNumber>
    <UDN>{UDN}</UDN>
    <serviceList>
      <service>
        <serviceType>urn:schemas-upnp-org:service:RenderingControl:1</serviceType>
        <serviceId>urn:upnp-org:serviceId:RenderingControl</serviceId>
        <SCPDURL>/RenderingControl/scpd.xml</SCPDURL>
        <controlURL>/RenderingControl/control</controlURL>
        <eventSubURL>/RenderingControl/event</eventSubURL>
      </service>
      <service>
        <serviceType>{AV_TRANSPORT}</serviceType>
        <serviceId>urn:upnp-org:serviceId:AVTransport</serviceId>
        <SCPDURL>{SCPD_PATH}</SCPDURL>
        <controlURL>{CONTROL_PATH}</controlURL>
        <eventSubURL>{EVENT_PATH}</eventSubURL>
      </service>
    </serviceList>
  </device>
</root>"#
    )
}

fn scpd_xml() -> String {
    let actions = ["SetAVTransportURI", "Play", "Stop", "GetTransportInfo"]
        .map(|name| format!("<action><name>{name}</name></action>"))
        .join("");
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<scpd xmlns="urn:schemas-upnp-org:service-1-0">
  <specVersion><major>1</major><minor>0</minor></specVersion>
  <actionList>{actions}</actionList>
  <serviceStateTable>
    <stateVariable sendEvents="no">
      <name>TransportState</name>
      <dataType>string</dataType>
    </stateVariable>
  </serviceStateTable>
</scpd>"#
    )
}

fn header<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    text.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim())
            .filter(|v| !v.is_empty())
    })
}

/// 集成测试只用上面那些函数，`main` 是给手动测试用的。
#[allow(dead_code)]
#[tokio::main]
async fn main() -> std::io::Result<()> {
    // 默认一直「在播」，除非收到 Stop。
    // 设 MOCK_POLLS=3 就是「被查 3 次播放状态之后算这集播完」，
    // 用来验证主程序会不会自动切下一集。
    let polls = std::env::var("MOCK_POLLS")
        .ok()
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(u32::MAX);
    let renderer = start(polls, true).await?;
    if polls != u32::MAX {
        println!("[mock] 每集查 {polls} 次播放状态后就报告播完");
    }
    println!("[mock] 假设备已启动: {FRIENDLY_NAME}");
    println!("[mock] 设备描述地址: {}", renderer.location);
    println!("[mock] 局域网内可用地址:");
    for ip in local_ipv4s() {
        println!("[mock]   http://{ip}:{}{DESC_PATH}", renderer.port);
    }
    println!("[mock] 现在在另一个窗口跑（PowerShell）：");
    println!("[mock]   $env:DLNA_INCLUDE_LOOPBACK=\"1\"");
    println!("[mock]   cargo run --example scan      # 先确认扫得到");
    println!("[mock]   cargo run --bin client        # 再走完整投屏");
    println!("[mock] 按 Ctrl+C 退出");

    tokio::signal::ctrl_c().await?;
    println!("[mock] 退出");
    Ok(())
}

/// 让 `#[path]` 引进来的测试不会因为 `main` 没被调用而报 warning。
#[allow(dead_code)]
fn _keep_main_referenced() {
    let _ = main;
}

/// 单元测试：这些是「假设备说的话」，说错了整个集成测试就白测了。
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 设备描述里带_avtransport() {
        let xml = description_xml(8080);
        assert!(xml.contains(AV_TRANSPORT));
        assert!(xml.contains(CONTROL_PATH));
        // 必须是路径，rupnp 会把它当 PathAndQuery 解析
        assert!(!xml.contains("<controlURL>http://"));
    }

    #[test]
    fn 只回应自己认得的搜索目标() {
        assert!(we_answer_to("ssdp:all"));
        assert!(we_answer_to(AV_TRANSPORT));
        assert!(!we_answer_to(
            "urn:schemas-upnp-org:service:ContentDirectory:1"
        ));
    }

    #[test]
    fn 从_soap_请求体里取出投屏地址() {
        let body = "<CurrentURI>http://a.b/c.mp4</CurrentURI>";
        assert_eq!(xml_text(body, "CurrentURI").unwrap(), "http://a.b/c.mp4");
        assert_eq!(xml_text(body, "Missing"), None);
    }
}
