use anyhow::Result;
use chrono::Local;
use log::{info, warn};
use serde::{Deserialize, Serialize};
use std::path::Path;
use tokio::fs as tokio_fs;
use xmtv_api::VideoUrl;

const DATA_FILE: &str = "data.txt";
const DATE_FORMAT: &str = "%Y-%m-%d";

#[derive(Debug, Serialize, Deserialize)]
struct StoredData {
    last_update: String,
    videos: Vec<VideoUrl>,
}

pub async fn update() -> Result<()> {
    let file_path = Path::new(DATA_FILE);
    let mut existing_videos = Vec::new();

    if file_path.exists() {
        match load_data().await {
            Ok(data) => {
                existing_videos = data.videos;
                info!("加载现有数据，共 {} 个视频", existing_videos.len());
            }
            Err(e) => {
                warn!("加载现有数据失败: {e}, 将创建新文件");
            }
        }
    }

    let new_videos = get_exact(Some(existing_videos)).await?;
    save_data(&new_videos).await?;

    Ok(())
}

pub async fn get_exact(others: Option<Vec<VideoUrl>>) -> Result<Vec<VideoUrl>> {
    info!("从 xmtv_api 获取视频信息");

    let video_urls = xmtv_api::get().await?;
    info!("获取到 {} 个视频信息", video_urls.len());

    let mut ret = Vec::with_capacity(video_urls.len());

    if let Some(existing_videos) = others {
        'outer: for video_url in &video_urls {
            for existing_video in &existing_videos {
                if existing_video == video_url {
                    ret.push(existing_video.clone());
                    continue 'outer;
                }
            }
            ret.push(video_url.clone());
        }
    } else {
        ret = video_urls;
    }

    info!("处理后的视频数量: {}", ret.len());
    Ok(ret)
}

async fn load_data() -> Result<StoredData> {
    let content = tokio_fs::read_to_string(DATA_FILE).await?;
    let stored_data: StoredData = serde_json::from_str(&content)?;
    Ok(stored_data)
}

async fn save_data(videos: &[VideoUrl]) -> Result<()> {
    let now = Local::now();
    let date = now.format(DATE_FORMAT).to_string();

    let stored_data = StoredData {
        last_update: date.clone(),
        videos: videos.to_vec(),
    };

    let content = serde_json::to_string_pretty(&stored_data)?;
    tokio_fs::write(DATA_FILE, content).await?;

    info!("数据已保存到 {DATA_FILE}，最后更新日期: {date}");
    info!("共保存 {} 个视频", videos.len());

    Ok(())
}

fn should_update(last_update: &str) -> bool {
    let now = Local::now();
    let current_date = now.format(DATE_FORMAT).to_string();

    if last_update == current_date {
        info!("今天已经更新过，跳过更新");
        return false;
    }

    match chrono::NaiveDate::parse_from_str(last_update, DATE_FORMAT) {
        Ok(last_date) => {
            let days_diff = now.date_naive().signed_duration_since(last_date).num_days();
            days_diff >= 1
        }
        Err(_) => {
            warn!("日期格式错误，强制更新");
            true
        }
    }
}

pub async fn get() -> Result<Vec<VideoUrl>> {
    let file_path = Path::new(DATA_FILE);

    if !file_path.exists() {
        info!("数据文件不存在，开始首次更新");
        update().await?;
    }

    let stored_data = load_data().await?;

    if should_update(&stored_data.last_update) {
        info!("距离上次更新已超过一天，开始增量更新");
        update().await?;
        return load_data().await.map(|data| data.videos);
    }

    info!("使用缓存数据，最后更新: {}", stored_data.last_update);
    Ok(stored_data.videos)
}
