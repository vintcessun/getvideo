# 一个自动从xmtv上爬取视频链接并投屏到指定DLNA设备的程序

因为手动b站投屏太麻烦了，最近有了时间，就写了这个项目。

## 用法

```bash
cargo run --bin client   # 交互式：自己选设备、选戏曲
cargo run --bin auto     # 无人值守：自动找设备、随机放
```

## 设备扫描

### 排查工具

扫不到设备时先跑这个，它会把每块网卡各收到多少响应都打出来：

```bash
cargo run --example scan        # 默认扫 8 秒
cargo run --example scan -- 15  # 扫 15 秒
```

### 没有真设备时怎么测

`mock_renderer` 是一台假电视，会真的响应 SSDP 搜索、真的处理 SOAP 投屏请求：

```powershell
# 窗口 1
cargo run --example mock_renderer

# 窗口 2（回环上也要能扫到假设备，所以要开这个开关）
$env:DLNA_INCLUDE_LOOPBACK="1"
cargo run --example scan     # 先确认扫得到
cargo run --bin client       # 再走完整投屏
```

用 cmd.exe 的话是 `set DLNA_INCLUDE_LOOPBACK=1`（单独一行，`set` 后面别跟
`&& cargo run`，那样会把值存成 `"1 "`）。PowerShell 里 `set` 是 `Set-Variable`
的别名，**设不出环境变量**，必须用 `$env:` 写法。

自动化测试跑的是同一套代码路径：

```bash
cargo test
```

### 环境变量

| 变量 | 作用 |
| --- | --- |
| `DLNA_SCAN_SECS` | 每轮扫描等几秒，默认 6 |
| `DLNA_DEVICE_NAME` | `auto` 按名字挑设备的关键字，默认 `FastCast`。连着 3 轮只扫到一台且名字对不上时，会退而用那一台 |
| `DLNA_DEVICE_URL` | 直接指定设备描述 XML 地址，跳过扫描 |
| `DLNA_SEARCH_INTERFACES` | 手动指定用哪几块网卡搜（逗号分隔的 IP） |
| `DLNA_INCLUDE_LOOPBACK` | 设为 `1` 时把回环也算进去，本地测试用 |
| `MOCK_POLLS` | 只对 `mock_renderer` 生效：假电视被查几次播放状态后就报告「这集播完了」，用来验证自动切集。默认一直在播 |

### 扫不到设备的排查顺序

1. 电脑和电视是不是同一个 WiFi、同一个网段；
2. Windows 防火墙有没有拦本程序 —— 第一次运行会弹窗，**专用网络和公用网络都要勾**；
3. 路由器 / 手机热点有没有开 AP 隔离、禁掉组播（校园网、公司网很常见）。
   这种网络下 SSDP 组播根本出不去，改代码也没用，只能用 `DLNA_DEVICE_URL` 直接指定地址；
4. `cargo run --example scan` 输出里的「本机可用网卡」有没有你实际在用的那块。

## 为什么自己写了设备发现

原来用的是 `crab-dlna` 的 `Render::discover`，它底层是 `ssdp-client`。
这两个库都停更了（分别卡在 0.2.1 和 2.1.0），而它们在 Windows 多网卡环境下有几个
会直接导致「扫不到设备」的问题：

1. **只在一块网卡上搜。** `ssdp-client` 用 `connect(8.8.8.8:80)` 反查本机 IP，拿到的是
   **默认路由**那块网卡。机器上装了 OpenVPN / TAP 之类的虚拟网卡时，VPN 一连默认路由
   就跑进隧道，M-SEARCH 发进了 VPN，电视永远收不到。
2. **一次搜索只发一个组播包。** UDP 组播本来就允许丢包，丢一个就等于没搜。
3. **只发 `AVTransport:1` 这一种搜索目标。** 很多国产盒子只回应 `ssdp:all` 或
   `MediaRenderer`，对按服务查询的 ST 不理睬 —— 这就是「手机能投屏，程序却扫不到」。
4. **拿到地址后串行抓设备描述，而且没有超时。** 网络里有一台不响应的设备，
   整轮扫描就卡死在那台上。
5. **超时算的是「多久没收到包」而不是「总共扫多久」**，行为不可预期。

现在的实现（`src/discovery.rs`）：枚举**所有**可用网卡并发搜索、四种搜索目标、
每种重发三轮、同时被动监听设备主动广播的 `NOTIFY`、最后并发地带超时抓设备描述，
并按 USN 里的设备 uuid 去重（同一台电视有线无线两个 IP 时不会重复出现）。

在同一台机器、同一个网络下实测：老实现 20 秒扫到 **0** 台，新实现扫到局域网里真实存在的
那台设备。

## 已知问题：数据更新会失败

`xmtv_api` 0.2.2 解析播出日期时是这么干的：找到节目名里 `"斗阵来看戏"` 的位置，
从后面切一段出来按空格分割，取第二段 `parse()` 成整数。

问题出在 `name.find("斗阵来看戏").unwrap_or(0) + "斗阵来看戏".len()` —— 节目名里
**没有**「斗阵来看戏」时它不是跳过这条，而是硬从第 15 字节开始切，切出一段中文
拿去 `parse()`，报 `invalid digit found in string`，整次更新全部失败。
xmtv 只要新增一个别的栏目就会触发。

`xmtv_api` 停更在 0.2.2，只能等它自己修（`unwrap_or(0)` 那里应该改成
「找不到就 `continue` 跳过这条」）。在那之前，本项目已经做了兜底：更新失败会打一条
warning 然后**继续用本地缓存投屏**，不会像以前那样整个程序直接退出。
