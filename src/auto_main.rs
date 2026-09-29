use getvideo::{data_store, dlna};
use anyhow::Result;
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;
use log::{error, info, warn};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .init();
    auto_cast().await?;
    Ok(())
}

/// 按设备名挑设备时用的关键字，可以用环境变量 `DLNA_DEVICE_NAME` 换掉。
///
/// 默认还是 FastCast，保持和以前一致。
const DEFAULT_DEVICE_NAME: &str = "FastCast";

/// 两轮扫描之间歇多久。扫描本身要好几秒，但万一它秒回（比如一块网卡都没有），
/// 没有这个间隔就会变成空转刷屏。
const RETRY_INTERVAL: Duration = Duration::from_secs(3);

/// 名字匹配不上时，先耐心等几轮再考虑退而求其次。
///
/// 不能第一轮就将就：电视还没开机的时候，网络里往往只有别的 DLNA 软件
/// （比如电脑上自己跑着的 Macast），一上来就用「只有一台就用它」会把戏曲
/// 投到那台上去。多等几轮给电视留出开机时间。
const ROUNDS_BEFORE_FALLBACK: u32 = 3;

/// 一直扫到找着设备为止。
///
/// 和原来相比改了三点：
/// 1. 扫描出错不再直接退出程序 —— 无人值守跑的东西，网络抖一下就挂掉没意义；
/// 2. 扫到了设备但没有一台叫这个名字时，会把实际扫到的名字打出来。
///    原来这种情况是闷头空转，用户只能看着它一直没反应，根本不知道是名字对不上；
/// 3. 连着 [`ROUNDS_BEFORE_FALLBACK`] 轮都只扫到同一台设备的话，就用它。
///    等这么久是为了避开「电视还没开机」那段时间，见上面常量的说明。
async fn wait_for_device() -> dlna::Render {
    let want = std::env::var("DLNA_DEVICE_NAME")
        .map(|v| v.trim().to_string())
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_DEVICE_NAME.to_string());

    let mut round: u32 = 0;
    loop {
        round += 1;
        let found = match dlna::discover().await {
            Ok(found) => found,
            Err(e) => {
                error!("扫描出错，{RETRY_INTERVAL:?} 后重试: {e}");
                tokio::time::sleep(RETRY_INTERVAL).await;
                continue;
            }
        };

        if let Some(render) = found.iter().find(|r| r.to_string().contains(want.as_str())) {
            info!("按关键字「{want}」选中设备: {render}");
            return render.clone();
        }

        match found.len() {
            0 => warn!("第 {round} 轮没扫到设备，{RETRY_INTERVAL:?} 后重试"),
            1 if round >= ROUNDS_BEFORE_FALLBACK => {
                let render = &found[0];
                warn!(
                    "连着 {round} 轮都没有名字含「{want}」的设备，全网就这一台，直接用它: {render}"
                );
                warn!("如果投错了地方，设 DLNA_DEVICE_NAME 指定你想要的设备名再跑一次");
                return render.clone();
            }
            1 => {
                warn!(
                    "第 {round} 轮只扫到 {}，名字不含「{want}」。\
                     再等等，电视可能还没开机（连着 {ROUNDS_BEFORE_FALLBACK} 轮还是这样就用它）",
                    found[0]
                );
            }
            n => {
                warn!("第 {round} 轮扫到 {n} 台设备，但没有一台名字含「{want}」：");
                for render in &found {
                    warn!("  - {render}");
                }
                warn!("可以设环境变量 DLNA_DEVICE_NAME 改成上面某台的名字，{RETRY_INTERVAL:?} 后重试");
            }
        }

        tokio::time::sleep(RETRY_INTERVAL).await;
    }
}

async fn auto_cast() -> Result<()> {
    let urls = data_store::get().await?;

    info!("对视频列表进行分类");
    let ret = xmtv_api::sort_by_title(urls);
    info!("ret = {:?}", ret);

    let mut render = wait_for_device().await;

    info!("已选择设备 render = {render:?}");

    let mut control = thread::spawn(|| {});
    let (mut _tx, mut rx) = mpsc::channel();
    'outer: loop {
        warn!("正在随机挑选一部戏曲");
        // 这里**不能重试**：`ret` 在这个循环里不会变，挑不出来就是列表本身空的，
        // 再挑一百次也是同样的结果。老代码写的是 `loop { Err => error!() }`，
        // 空列表时会以最快速度刷屏空转，既看不出原因也停不下来。
        //
        // 无人值守的 auto 在这种情况下退出是诚实的：没节目可放，
        // 假装还在跑只会让人以为一切正常。
        let vl = match xmtv_api::get_random_url_list(&ret) {
            Ok(vl) => vl,
            Err(e) => {
                error!("挑不出可播放的剧目（节目列表是空的？）：{e}");
                break 'outer;
            }
        };
        info!("挑选到 vl = {:?}", vl);
        let mut i = 0;
        let len = vl.len();
        'inner: while i < len {
            let video = &vl[i];
            info!("正在播放 {} 的第 {} 集", video.name, i + 1);
            warn!("将要投屏：{:?}", video);
            render = dlna::play(render, video.url.as_str()).await;
            if control.is_finished() {
                (_tx, rx) = mpsc::channel();
                control = thread::spawn(move || {
                    let selection = Select::with_theme(&ColorfulTheme::default())
                        .with_prompt("请选择一个")
                        .default(0)
                        .item("下一部")
                        .item("上一集")
                        .item("下一集")
                        .item("退出投屏")
                        .interact()
                        .unwrap();
                    _tx.send(selection).unwrap();
                });
            }
            while !dlna::is_stopped(&render).await {
                match rx.try_recv() {
                    Ok(selection) => match selection {
                        0 => {
                            continue 'outer;
                        }
                        1 => {
                            if i != 0 {
                                i -= 1;
                                continue 'inner;
                            } else {
                                continue 'inner;
                            }
                        }
                        2 => {
                            i += 1;
                            continue 'inner;
                        }
                        3 => {
                            break 'outer;
                        }
                        _ => {}
                    },
                    Err(_) => { /*error!("没有接收到");*/ }
                }
                // 歇一下再查下一次，别把设备问死
                tokio::time::sleep(dlna::POLL_INTERVAL).await;
            }
            i += 1;
        }
    }

    Ok(())
}
