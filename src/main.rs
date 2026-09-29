use getvideo::{data_store, dlna};
use anyhow::Result;
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;
use log::{error, info, warn};
use std::sync::mpsc;
use std::thread;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Debug)
        .init();

    loop {
        let selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("选择一个选项")
            .default(0)
            .item("投屏电视")
            .item("更新urls")
            .item("退出")
            .interact()
            .unwrap();

        match selection {
            0 => cast().await?,
            1 => {
                data_store::update().await?;
            }
            2 => {
                break;
            }
            _ => {
                break;
            }
        };
    }
    Ok(())
}

async fn cast() -> Result<()> {
    warn!("获取视频列表");

    let urls = data_store::get().await?;

    info!("对视频列表进行分类");
    let ret = xmtv_api::sort_by_title(urls);
    info!("ret = {:?}", ret);

    let (renders_discovered, selection) = loop {
        info!("寻找设备");
        let renders_discovered = dlna::discover().await?;
        if renders_discovered.is_empty() {
            error!("没找到设备");
            let selection = Select::with_theme(&ColorfulTheme::default())
                .with_prompt("选择一台设备")
                .default(0)
                .item("重试")
                .item("退出")
                .interact()?;
            match selection {
                0 => {
                    continue;
                }
                1 => {
                    return Ok(());
                }
                _ => {}
            }
        }

        info!("找到设备 renders_discovered = {:?}", renders_discovered);
        let mut outer: Vec<String> = Vec::with_capacity(7);

        outer.push("重试".to_string());
        outer.push("返回".to_string());
        for render in &renders_discovered {
            let out = format!("{render}");
            outer.push(out);
        }

        let selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("选择一台设备")
            .default(0)
            .items(&outer)
            .interact()?;

        match selection {
            0 => {}
            1 => {
                return Ok(());
            }
            r => {
                break (renders_discovered, r - 2);
            }
        }
    };

    let mut render = renders_discovered[selection].clone();
    info!("已选择设备 render = {render:?}");

    let mut control = thread::spawn(|| {});
    let (mut _tx, mut rx) = mpsc::channel();
    'outer: loop {
        warn!("正在随机挑选一部戏曲");
        // 这里**不能重试**：`ret` 在这个循环里不会变，挑不出来就是列表本身空的，
        // 再挑一百次也是同样的结果。老代码写的是 `loop { Err => error!() }`，
        // 空列表时会以最快速度刷屏空转，既看不出原因也停不下来。
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
