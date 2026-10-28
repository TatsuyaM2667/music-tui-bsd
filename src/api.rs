use anyhow::Result;
use futures_util::StreamExt;
use once_cell::sync::Lazy;
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio_util::io::StreamReader;

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct TrackInfo {
    pub path: String,
    pub lrc: Option<String>,
    pub video: Option<String>,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub track_number: Option<i32>,
    pub duration: f64,
}

// 環境変数から URL を取得する。設定されていない場合はパニックせずに空文字などを返す
static BASE_URL: Lazy<String> =
    Lazy::new(|| std::env::var("WORKERS_URL").unwrap_or_else(|_| "".to_string()));

pub async fn fetch_tracks_streaming(
    tx_track: tokio::sync::mpsc::Sender<TrackInfo>,
    tx_progress: tokio::sync::mpsc::Sender<f64>,
    pause_signal: Arc<AtomicBool>,
) -> Result<()> {
    let url = format!("{}/tracks", *BASE_URL);
    let res = reqwest::get(url).await?;
    let total_size = res.content_length().unwrap_or(1);

    let mut downloaded: u64 = 0;
    let stream = res
        .bytes_stream()
        .map(|result| result.map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e)));
    let reader = StreamReader::new(stream);
    let mut lines = BufReader::new(reader).lines();

    while let Some(line) = lines.next_line().await? {
        while pause_signal.load(Ordering::SeqCst) {
            tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        }
        downloaded += line.len() as u64 + 1;
        if let Ok(track) = serde_json::from_str::<TrackInfo>(&line) {
            let _ = tx_track.send(track).await;
        }
        let progress = (downloaded as f64 / total_size as f64) * 100.0;
        let _ = tx_progress.send(progress).await;
    }
    Ok(())
}

fn safe_encode_path(path: &str) -> String {
    path.split('/')
        .map(|s| urlencoding::encode(s).into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

pub fn stream_url_from_path(path: &str) -> String {
    format!(
        "{}/stream/{}",
        *BASE_URL,
        safe_encode_path(path.trim_start_matches('/'))
    )
}

pub fn video_url_from_path(path: &str) -> String {
    format!(
        "{}/stream/{}",
        *BASE_URL,
        safe_encode_path(path.trim_start_matches('/'))
    )
}

pub fn lyrics_url_from_path(path: &str) -> String {
    format!(
        "{}/lyrics/{}",
        *BASE_URL,
        safe_encode_path(path.trim_start_matches('/'))
    )
}

pub async fn fetch_lyrics_from_url(url: &str) -> Result<String> {
    let res = reqwest::get(url).await?;
    Ok(res.text().await?)
}

pub async fn update_track_order(tracks: &[TrackInfo]) -> Result<()> {
    let url = format!("{}/reorder", *BASE_URL);
    let client = reqwest::Client::new();
    let res = client.post(url).json(tracks).send().await?;
    if !res.status().is_success() {
        return Err(anyhow::anyhow!("Failed to update order: {}", res.status()));
    }
    Ok(())
}
