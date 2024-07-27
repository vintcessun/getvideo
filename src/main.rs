mod dlna;
mod get_video_list;
mod sql;
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;
use log::{error, info, warn};
use anyhow::Result;
use std::env::set_var;

fn main(){
    set_var("RUST_LOG","info");
    env_logger::init();

    loop{
        let selection = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("选择一个选项")
        .default(0)
        .item("投屏电视 通过数据库")
        .item("投屏电视 在线上获取")
        .item("更新urls")
        .item("退出")
        .interact()
        .unwrap();

        loop{
            match match selection{
                0=>cast(true),
                1=>cast(false),
                2=>sql::update(),
                3=>{return;},
                _=>Ok(()),
            }{
                Ok(_)=>{break},
                Err(_)=>{},
            }
        }
    }
}

fn cast(by_db:bool)->Result<()>{
    warn!("获取视频列表");

    let urls = if by_db{
        sql::get()?
    }
    else{
        sql::get_exact()?
    };

    info!("对视频列表进行分类");
    let ret = get_video_list::resort(urls);
    info!("ret = {:?}",&ret);

    let (renders_discovered,selection)=loop{
        info!("寻找设备");
        let renders_discovered = dlna::discover()?;
        if renders_discovered.len()==0{
            error!("没找到设备");
            let selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("请选择一个")
            .default(0)
            .item("重试")
            .item("退出")
            .interact()?;
            match selection{
                0=>{continue;},
                1=>{return Ok(());},
                _=>{},
            }
        }

        info!("找到设备 renders_discovered = {:?}",&renders_discovered);
        let mut outer: Vec<String> = Vec::with_capacity(7);
        
        outer.push("重试".to_string());
        outer.push("返回".to_string());
        for render in &renders_discovered{
            let out = format!("{}",render);
            outer.push(out);
        }
    
        let selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("选择一台设备")
            .default(0)
            .items(&outer)
            .interact()?;
        
        match selection{
            0=>{}
            1=>{return Ok(());}
            r=>{break (renders_discovered,r-2);}
        }
    };

    

    let mut render = renders_discovered[selection].clone();
    info!("已选择设备 render = {:?}",render);

    loop{
        warn!("正在随机挑选一部戏曲");
        let vl = get_video_list::get_random_url_list(&ret)?;
        info!("挑选到 vl = {:?}",&vl);
        for video in &vl{
            warn!("将要投屏：{:?}",&video);
            render = dlna::play(render,video.url.as_str())?;
        }
    }
}