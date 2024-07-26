mod dlna;
mod get_video_list;
use dialoguer::Select;
use dialoguer::theme::ColorfulTheme;

fn main() {
    let urls = get_video_list::get().unwrap();
    let ret = get_video_list::resort(urls);

    let (renders_discovered,selection)=loop{
        let renders_discovered = dlna::discover().unwrap();
        if renders_discovered.len()==0{
            println!("没找到设备，正在重试");
            continue;
        }
    
        let mut outer: Vec<String> = Vec::with_capacity(7);
        outer.push("重试".to_string());
        for render in &renders_discovered{
            let out = format!("{}",render);
            outer.push(out);
        }
    
        let selection = Select::with_theme(&ColorfulTheme::default())
            .with_prompt("选择一台设备")
            .default(0)
            .items(&outer)
            .interact()
            .unwrap();
        
        match selection{
            0=>{}
            r=>{break (renders_discovered,r-1);}
        }
    };


    let mut render = renders_discovered[selection].clone();

    loop{
        let vl = get_video_list::get_random_url_list(&ret).unwrap();
        println!("{:?}",&vl);
        for video in &vl{
            render = dlna::play(render,video.url.as_str()).unwrap();
        }
    }
}