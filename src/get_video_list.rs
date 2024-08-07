use url::Url;
use reqwest::blocking::Client;
use serde_json::Value;
use rand::Rng;
use log::{error, info, warn};
use serde::{Deserialize,Serialize};
use anyhow::Result;
use indicatif::{ProgressBar, ProgressStyle};

#[derive(Debug,Clone, Serialize, Deserialize)]
pub struct VideoUrl{
    pub title:String,
    pub name:String,
    pub url:String,
    pub time:u32
}

#[derive(Debug)]
pub struct Video{
    pub title:String,
    pub range:Vec<VideoUrl>
}

pub fn get()->Result<Vec<VideoUrl>>{
    let url = Url::parse("https://mapi1.kxm.xmtv.cn/api/open/xiamen/web_search_list.php?count=10000&search_text=%E6%96%97%E9%98%B5%E6%9D%A5%E7%9C%8B%E6%88%8F&offset=0&bundle_id=livmedia&order_by=publish_time&time=0&with_count=1")?;
    info!("获取视频列表 url = {:?}",&url);

    let res = Client::new().get(url).send()?;
    let text:String = res.text()?;
    let json:Value = serde_json::from_str(text.as_str())?;
    info!("获取到视频信息 json = {}",json.to_string());

    let mut ret:Vec<VideoUrl> = vec![];
    let data = json["data"].as_array().unwrap().into_iter().rev();
    info!("获取到视频列表 data = {:?}",&data);

    for i in data{
        let name = i["title"].to_string().replace("\"","");
        let position = match name.find("斗阵来看戏"){
            Some(ret)=>{ret}
            _=>{name.len()}
        };
        let title = name[0..position].replace("（","(").split("(").collect::<Vec<_>>()[0].replace(" ","");
        let url_into_share = match i["content_urls"]["share"].as_str(){
            Some(ret)=>{ret.to_string()}
            _=>{continue;}
        };
        let position = match name.find("斗阵来看戏"){
            Some(ret)=>{ret}
            _=>{0}
        }+"斗阵来看戏".len();
        let t: &str = &name[position..];
        let t = t.split(" ").collect::<Vec<_>>();
        let t=if t.len()>=2{
            t[1].replace(".","").replace("-","")
        }
        else{
            //let t: &str = t[0];
            match url_into_share.find("-"){
                Some(_)=>{
                    let t = url_into_share.split("/").collect::<Vec<_>>();
                    let t = t[4];
                    let t = t.replace(".","").replace("-","");
                    t
                }
                _=>{
                    error!("存在一些无法识别的组别已经忽略，下面是一些信息或许有助于修复");
                    warn!("titile = {:?}",&title);
                    warn!("name = {:?}",&name);
                    warn!("url_into_share = {:?}",&url_into_share);
                    continue;
                }
            }
        };
        let t = t.parse::<u32>()?;
        let video = VideoUrl{title:title,name:name,url:url_into_share,time:t};
        info!("获取到单个视频信息 video = {:?}",&video);
        ret.push(video);
    }
    return Ok(ret);
}

pub fn get_video_url(url:&String)->Result<String>{
    let url_into_share=Url::parse(url.as_str())?;
    info!("获取视频页面 url = {:?}",url);

    let res = loop{
        match Client::new().get(url_into_share.clone()).send(){
            Ok(ret)=>{
                info!("获取到页面 ret = {:?}",&ret);
                break ret;
            }
            Err(_)=>{
                error!("获取页面失败 url = {:?}",url);
                info!("重试");
            }
        }
    };
    let text: String = res.text()?;
    let text = text[(text.find("<source src=").unwrap()+13)..].to_string();
    let download_url = text[..(text.find("\"").unwrap())].to_string();
    info!("从 {:?} 获取到视频源地址 {:?}",&url,&download_url);
    Ok(download_url)
}

pub fn resort (urls:Vec<VideoUrl>)->Vec<Video>{
    let mut videos: Vec<Video> = vec![];
    for url in &urls{
        let mut exists=false;
        for video in &mut videos{
            if url.title==video.title{
                exists=true;
                video.range.push(url.clone());
            }
        }
        if exists==false{
            let mut video=Video{title:url.title.clone(),range:vec![]};
            video.range.push(url.clone());
            videos.push(video);
        }
    }
    for video in &mut videos{
        video.range.sort_by(|a,b| a.time.cmp(&b.time));
    }
    return videos;
}

#[derive(Debug)]
pub struct Videoplay{
    pub name:String,
    pub url:String
}

pub fn get_video_to_url(mut videos:Vec<VideoUrl>)->Result<Vec<VideoUrl>>{
    let len = videos.len().try_into()?;
    let pb = ProgressBar::new(len);
    pb.set_style(ProgressStyle::default_bar()
    .template("{spinner:.green} [{elapsed_precise}] [{wide_bar:.cyan/blue}] {pos}/{len} ({per_sec}, {eta})")?);
    for video in &mut videos{
        video.url = loop{
            match get_video_url(&video.url){
                Ok(ret)=>{
                    warn!("成功获取 ret = {:?}",&ret);
                    break ret;
                },
                Err(_)=>{
                    error!("获取源url失败 video = {:?}",&video);
                },
            }
        };
        pb.inc(1);
    }
    pb.finish_with_message("源url获取完成");
    Ok(videos)
}

pub fn get_random_url_list(videos:&Vec<Video>)->Result<Vec<Videoplay>>{
    let mut rng = rand::thread_rng();
    let randnumber = rng.gen_range(0..videos.len());
    let randone = &videos[randnumber];
    let mut ret = Vec::with_capacity(12);
    for i in &randone.range{
        let name = i.name.clone();
        //let url = get_video_url(&i.url)?;
        let url = i.url.clone();
        let one = Videoplay{name:name,url:url};
        ret.push(one);
    }
    Ok(ret)
}
