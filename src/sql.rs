use crate::get_video_list;
use crate::get_video_list::VideoUrl;
use anyhow::anyhow;
use anyhow::Result;
use chrono::{Datelike, Local};
use log::{error, info, warn};
use rusqlite::Connection;
use std::fs::remove_file;
use std::path::Path;

const DB: &str = "urls.db";

pub fn update() -> Result<()> {
    let file_path = Path::new(DB);
    let mut others = Vec::new();
    if file_path.exists() {
        others = get_data()?;
        info!("删除已存在的{}", DB);
        remove_file(file_path)?;
    }

    let ret = get_exact(Some(others))?;

    let conn = Connection::open(DB)?;

    let now = Local::now();
    let date = format!("{}-{}-{}", now.year(), now.month(), now.day());
    info!("获取到date = {:?}", date);

    info!("保存到{}", DB);
    let mut value = Vec::with_capacity(ret.len());
    for i in &ret {
        value.push(serde_json::to_string(&i)?);
    }

    let value = serde_json::to_string(&value)?;

    conn.execute(
        "CREATE TABLE videos_with_exact_url (
            value TEXT NOT NULL
        )",
        (),
    )?;

    info!("保存到videos_with_exact_url 值为{:?}", &value);
    conn.execute(
        "INSERT INTO videos_with_exact_url (value) VALUES (?1)",
        [&value],
    )?;

    conn.execute(
        "CREATE TABLE date (
            value TEXT NOT NULL
        )",
        (),
    )?;

    info!("保存到date 值为{:?}", &[&date]);
    conn.execute("INSERT INTO date (value) VALUES (?1)", [&date])?;

    Ok(())
}

pub fn get_exact(others: Option<Vec<VideoUrl>>) -> Result<Vec<VideoUrl>> {
    info!("从xmtv.cn上获取");
    let urls = get_video_list::get()?;
    info!("urls = {:?}", &urls);

    let mut ret = Vec::with_capacity(urls.len());
    if let Some(datas) = others {
        'outer: for per in &urls {
            for data in &datas {
                if data == per {
                    ret.push(data.to_owned());
                    continue 'outer;
                }
            }
            ret.push(per.to_owned());
        }
    } else {
        ret = urls;
    }

    info!("ret = {:?}", &ret);

    info!("获取具体视频url");
    let urls = get_video_list::get_video_to_url(ret)?;
    info!("urls = {:?}", &urls);

    Ok(urls)
}

fn database_error() -> Result<Vec<VideoUrl>> {
    error!("数据库获取失败，请更新");
    update()?;
    warn!("数据库获取失败，已更新");
    Err(anyhow!("数据库获取失败，已更新"))
}

fn get_data() -> Result<Vec<VideoUrl>> {
    let conn = Connection::open(DB)?;
    let mut stmt = conn.prepare("SELECT value FROM videos_with_exact_url")?;
    let mut rows = stmt.query([])?;

    let Some(row) = rows.next()? else {
        return database_error();
    };
    let json: String = row.get(0)?;
    info!("获取到json = {:?}", &json);

    let ret_string: Vec<String> = serde_json::from_str(json.as_str())?;

    let mut ret: Vec<VideoUrl> = Vec::new();
    for i in ret_string {
        let one = serde_json::from_str(i.as_str())?;
        ret.push(one);
    }
    Ok(ret)
}

pub fn get() -> Result<Vec<VideoUrl>> {
    let now = Local::now();
    let date = format!("{}-{}-{}", now.year(), now.month(), now.day());

    let file_path = Path::new(DB);
    if !file_path.exists() {
        error!("不存在{}", DB);
        info!("生成{}", DB);
        update()?;
    }

    let mut db_date = String::new();
    {
        let conn = Connection::open(DB)?;

        let mut stmt = conn.prepare("SELECT value FROM date")?;
        let mut rows = stmt.query([])?;

        let Some(row) = rows.next()? else {
            return database_error();
        };

        let ret: String = row.get(0)?;
        db_date += ret.as_str();
        info!("获取到 db_date = {:?}", &db_date);
    }

    if db_date != date {
        info!("日期不对应 现在日期为 date = {:?}", &date);
        info!("重新获取");
        update()?;
    }

    let ret = get_data()?;

    info!("获取到 ret = {:?}", &ret);

    Ok(ret)
}
