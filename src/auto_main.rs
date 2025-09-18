mod data_store;
mod dlna;
use anyhow::Result;
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;
use log::{error, info, warn};
use std::sync::mpsc;
use std::thread;

#[tokio::main]
async fn main() -> Result<()> {
    env_logger::Builder::new()
        .filter_level(log::LevelFilter::Info)
        .init();
    auto_cast().await?;
    Ok(())
}

async fn auto_cast() -> Result<()> {
    let urls = data_store::get().await?;

    info!("对视频列表进行分类");
    let ret = xmtv_api::sort_by_title(urls);
    info!("ret = {:?}", &ret);

    let mut render = 'outer: loop {
        let renders_discovered = dlna::discover().await?;
        if renders_discovered.is_empty() {
            continue;
        }

        for render in &renders_discovered {
            let out = format!("{render}");
            if out.contains("FastCast") {
                break 'outer render.to_owned();
            }
        }
    };

    info!("已选择设备 render = {render:?}");

    let mut control = thread::spawn(|| {});
    let (mut _tx, mut rx) = mpsc::channel();
    'outer: loop {
        warn!("正在随机挑选一部戏曲");
        let vl = loop {
            match xmtv_api::get_random_url_list(&ret) {
                Ok(ret) => {
                    break ret;
                }
                Err(_) => {
                    error!("挑选失败，正在重新挑选");
                }
            }
        };
        info!("挑选到 vl = {:?}", &vl);
        let mut i = 0;
        let len = vl.len();
        'inner: while i < len {
            let video = &vl[i];
            info!("正在播放 {} 的第 {} 集", video.name, i + 1);
            warn!("将要投屏：{:?}", &video);
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
            }
            i += 1;
        }
    }

    Ok(())
}
