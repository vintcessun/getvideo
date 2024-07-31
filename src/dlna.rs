use crab_dlna::{Render,Error};
use xml::escape::escape_str_attribute;
use std::{thread, time};
use log::{error, info, warn, debug};
use anyhow::Result;

fn sleep(t:u64){
    thread::sleep(time::Duration::from_millis(t));
}

const PAYLOAD_PLAY: &str = r#"
    <InstanceID>0</InstanceID>
    <Speed>1</Speed>
"#;

#[derive(Debug,Clone)]
pub struct Media{
    video_url:String,
    video_type:String,
}
impl Media{
    pub fn new(url:&str)->Self{
        let t = url.split(".").collect::<Vec<_>>();
        let video_type = t[t.len()-1];
        let ret = Media{video_url:url.to_string(),video_type:video_type.to_string()};
        ret
    }
}

pub fn play(render: Render, url:&str) -> Result<Render>{
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let ret = loop{
                warn!("开始投屏 url = {}",&url);
                match _play(render.clone(), Media::new(url)).await{
                    Err(_t)=>{
                        error!("投屏错误 url = {}\n render = {:?}",&url,&render);
                    }
                    Ok(ret)=>{
                        info!("投屏成功");
                        info!("render已更新");
                        info!("render = {:?}",&ret);
                        break ret;
                    }
                }
            };
            Ok(ret)
        })
}

pub async fn _play(render: Render, streaming_server: Media) -> Result<Render> {
    info!("投屏{}",&streaming_server.video_url);
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
            uri_video = &streaming_server.video_url,
            type_video = &streaming_server.video_type,
            uri_sub = &streaming_server.video_url,
            type_sub = &streaming_server.video_type
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
        .map_err(Error::DLNASetAVTransportURIError)?;

    //println!("Playing video");
    render
        .service
        .action(render.device.url(), "Play", PAYLOAD_PLAY)
        .await
        .map_err(Error::DLNAPlayError)?;

    //streaming_server_handle
    //    .await
    //    .map_err(Error::DLNAStreamingError)?;

    let stop = vec!["STOPPED","NO_MEDIA_PRESENT"];

    sleep(20000);

    loop{
        let ret = render
        .service
        .action(render.device.url(),"GetTransportInfo",PAYLOAD_PLAY)
        .await
        .map_err(Error::DLNAPlayError)?;
        //println!("{:?}",&ret);
        if ret.contains_key("CurrentTransportState"){
            debug!("DLNA设备状态{}",ret["CurrentTransportState"]);
            if stop.contains(&ret["CurrentTransportState"].as_str()){
                break;
            }
        }
        else{
            break;
        }
    };

    Ok(render)
}

pub fn discover()->Result<Vec<Render>>{
    tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .build()
    .unwrap()
    .block_on(async {
        let renders_discovered: Vec<Render> = Render::discover(20).await?;
        Ok(renders_discovered)
    })
}