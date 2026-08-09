use crate::config::*;
use crate::types::{Channel, SourceResult};
use reqwest::Client;
use serde_json::Value;
use std::io::Read;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Semaphore;
use url::Url;

// ── HTTP 工具函数 ─────────────────────────────────────────────────

fn make_client(timeout: Duration) -> Client {
    Client::builder()
        .timeout(timeout)
        .build()
        .expect("failed to build HTTP client")
}

/// 从 m3u8 播放列表获取第一个 TS 分片 URL
async fn get_ts_url(m3u8_url: &str, timeout: Duration) -> Option<String> {
    let resp = make_client(timeout).get(m3u8_url).send().await.ok()?;
    if resp.status() != 200 {
        return None;
    }
    let body = resp.text().await.ok()?;
    let parsed = Url::parse(m3u8_url).ok()?;
    let origin = format!("{}://{}", parsed.scheme(), parsed.host_str()?);
    let base = &m3u8_url[..m3u8_url.rfind('/')? + 1];

    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with("http") {
            return Some(line.to_string());
        } else if line.starts_with('/') {
            return Some(format!("{}{}", origin, line));
        } else {
            return Some(format!("{}{}", base, line));
        }
    }
    None
}

/// 下载 stream_url 最多 SPEED_TEST_SECS 秒并返回 MB/s
async fn measure_speed(stream_url: &str, deadline: Instant) -> f64 {
    let remaining = deadline.saturating_duration_since(Instant::now());
    if remaining.is_zero() {
        return -1.0;
    }
    let start = Instant::now();
    let client = make_client(remaining.min(Duration::from_secs(10)));
    let resp = match client.get(stream_url).send().await {
        Ok(r) if r.status() < reqwest::StatusCode::BAD_REQUEST => r,
        _ => return -1.0,
    };

    let mut size: u64 = 0;
    let mut stream = resp.bytes_stream();
    use futures_util::StreamExt;
    loop {
        match tokio::time::timeout(Duration::from_millis(500), stream.next()).await {
            Ok(Some(Ok(chunk))) => {
                size += chunk.len() as u64;
            }
            _ => break,
        }
        if size > 10 * 1024 * 1024 || start.elapsed() > SPEED_TEST_SECS || Instant::now() > deadline
        {
            break;
        }
    }
    let dur = start.elapsed().as_secs_f64().max(0.001);
    size as f64 / 1024.0 / 1024.0 / dur
}

/// 解析 m3u8 → 找到第一个分片 → 测速
async fn test_stream_url(stream_url: &str, deadline: Instant) -> f64 {
    if Instant::now() > deadline {
        return -1.0;
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let ts = get_ts_url(stream_url, remaining.min(Duration::from_secs(5))).await;
    let ts = match ts {
        Some(u) => u,
        None => return -1.0,
    };
    if Instant::now() > deadline {
        return -1.0;
    }
    measure_speed(&ts, deadline).await
}

// ── 各类型测速 ────────────────────────────────────────────────────

async fn test_txiptv(host: &str, deadline: Instant, fetch_ch: bool) -> (f64, Vec<Channel>) {
    if Instant::now() > deadline {
        return (-1.0, vec![]);
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let url = format!("http://{}/iptv/live/1000.json?key=txiptv", host);
    let resp = match make_client(remaining.min(Duration::from_secs(2)))
        .get(&url)
        .send()
        .await
    {
        Ok(r) if r.status() == 200 => r,
        _ => return (-1.0, vec![]),
    };
    let data: Value = match resp.json().await {
        Ok(v) => v,
        Err(_) => return (-1.0, vec![]),
    };

    let mut channels = vec![];
    let mut first_url = String::new();
    if let Some(arr) = data["data"].as_array() {
        for d in arr {
            let name = d["name"].as_str().unwrap_or("").to_string();
            let u = d["url"].as_str().unwrap_or("").to_string();
            if name.is_empty() || u.is_empty() || u.contains(',') {
                continue;
            }
            let full = if u.contains("http") {
                u.clone()
            } else if u.starts_with('/') {
                format!("http://{}{}", host, u)
            } else {
                format!("http://{}/{}", host, u)
            };
            if fetch_ch {
                channels.push(Channel {
                    name,
                    url: full.clone(),
                });
            }
            if first_url.is_empty() {
                first_url = full;
            }
        }
    }
    if first_url.is_empty() {
        return (-1.0, channels);
    }
    let speed = test_stream_url(&first_url, deadline).await;
    (speed, channels)
}

async fn test_hsmdtv(host: &str, deadline: Instant) -> f64 {
    if Instant::now() > deadline {
        return -1.0;
    }
    let url = format!("http://{}{}", host, HSMDTV_TEST_URI);
    test_stream_url(&url, deadline).await
}

async fn test_jsmpeg(host: &str, deadline: Instant, fetch_ch: bool) -> (f64, Vec<Channel>) {
    if Instant::now() > deadline {
        return (-1.0, vec![]);
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let url = format!("http://{}/streamer/list", host);
    let resp = match make_client(remaining.min(Duration::from_secs(2)))
        .get(&url)
        .send()
        .await
    {
        Ok(r) if r.status() == 200 => r,
        _ => return (-1.0, vec![]),
    };
    let list: Vec<Value> = match resp.json().await {
        Ok(v) => v,
        Err(_) => return (-1.0, vec![]),
    };

    let mut channels = vec![];
    let mut first_url = String::new();
    for d in &list {
        let name = d["name"].as_str().unwrap_or("").trim().to_string();
        let key = d["key"].as_str().unwrap_or("").trim().to_string();
        if name.is_empty() || key.is_empty() {
            continue;
        }
        let full = format!("http://{}/hls/{}/index.m3u8", host, key);
        if fetch_ch {
            channels.push(Channel {
                name,
                url: full.clone(),
            });
        }
        if first_url.is_empty() {
            first_url = full;
        }
    }
    if first_url.is_empty() {
        return (-1.0, channels);
    }
    let speed = test_stream_url(&first_url, deadline).await;
    (speed, channels)
}

async fn test_zhgxtv(host: &str, deadline: Instant, fetch_ch: bool) -> (f64, Vec<Channel>) {
    if Instant::now() > deadline {
        return (-1.0, vec![]);
    }
    let remaining = deadline.saturating_duration_since(Instant::now());
    let url = format!("http://{}{}", host, ZHGXTV_INTERFACE);
    let resp = match make_client(remaining.min(Duration::from_secs(5)))
        .get(&url)
        .send()
        .await
    {
        Ok(r) if r.status() == 200 => r,
        _ => return (-1.0, vec![]),
    };
    let body = match resp.text().await {
        Ok(b) => b,
        Err(_) => return (-1.0, vec![]),
    };

    let mut channels = vec![];
    let mut first_url = String::new();
    for line in body.lines() {
        let line = line.trim();
        if !line.contains(',') {
            continue;
        }
        let mut parts = line.splitn(2, ',');
        let name = parts.next().unwrap_or("").trim().to_string();
        let url_part = parts.next().unwrap_or("").trim().to_string();
        let full = if url_part.starts_with("http") {
            if let Ok(p) = Url::parse(&url_part) {
                let mut f = format!("{}://{}{}", p.scheme(), host, p.path());
                if let Some(q) = p.query() {
                    f.push('?');
                    f.push_str(q);
                }
                f
            } else {
                continue;
            }
        } else if url_part.starts_with('/') {
            format!("http://{}{}", host, url_part)
        } else {
            format!("http://{}/{}", host, url_part)
        };
        if fetch_ch {
            channels.push(Channel {
                name,
                url: full.clone(),
            });
        }
        if first_url.is_empty() {
            first_url = full;
        }
    }
    if first_url.is_empty() {
        return (-1.0, channels);
    }
    let speed = test_stream_url(&first_url, deadline).await;
    (speed, channels)
}

// ── 公开接口 ──────────────────────────────────────────────────────

/// 测试单个 API 主机
pub async fn test_api_host_speed(
    host: &str,
    match_type: &str,
    fetch_channels: bool,
) -> (f64, Vec<Channel>) {
    let deadline = Instant::now() + HOST_TIMEOUT;
    match match_type {
        "txiptv" => test_txiptv(host, deadline, fetch_channels).await,
        "hsmdtv" => {
            let spd = test_hsmdtv(host, deadline).await;
            (spd, vec![])
        }
        "jsmpeg" => test_jsmpeg(host, deadline, fetch_channels).await,
        "zhgxtv" => test_zhgxtv(host, deadline, fetch_channels).await,
        _ => (-1.0, vec![]),
    }
}

/// 为已选定的源补抓频道列表
pub async fn fetch_channels_for_source(src: &mut SourceResult) {
    match src.match_type.as_str() {
        "txiptv" | "jsmpeg" | "zhgxtv" => {
            let (_, chs) = test_api_host_speed(&src.host, &src.match_type, true).await;
            src.channels = chs;
        }
        _ => {}
    }
}

/// 并发批量测速所有 API 主机，返回速度 >= SPEED_LOW 的结果
pub async fn run_api_speed_tests(
    items: Vec<serde_json::Map<String, Value>>,
    workers: usize,
) -> Vec<SourceResult> {
    let total = items.len();
    let completed = Arc::new(AtomicUsize::new(0));
    let valid = Arc::new(AtomicUsize::new(0));
    let sem = Arc::new(Semaphore::new(workers));

    print_progress(0, total, 0);

    // 在任务内捕获一次阈值，避免并发下重复读取
    let low = speed_low();
    let mut handles = vec![];
    for item in items {
        let sem = sem.clone();
        let completed = completed.clone();
        let valid = valid.clone();
        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let host = item
                .get("host")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let mt = item
                .get("matchType")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let source = item
                .get("source")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if host.is_empty() {
                return None;
            }
            let (speed, _) = test_api_host_speed(&host, &mt, false).await;
            let c = completed.fetch_add(1, Ordering::Relaxed) + 1;
            let v = if speed >= low {
                valid.fetch_add(1, Ordering::Relaxed) + 1
            } else {
                valid.load(Ordering::Relaxed)
            };
            print_progress(c, total, v);
            if speed >= low {
                Some(SourceResult {
                    host,
                    match_type: mt,
                    source,
                    speed,
                    channels: vec![],
                })
            } else {
                None
            }
        });
        handles.push(handle);
    }

    let mut results = vec![];
    for h in handles {
        if let Ok(Some(r)) = h.await {
            results.push(r);
        }
    }
    println!();
    results
}

/// 进度条打印
pub fn print_progress(completed: usize, total: usize, success: usize) {
    if total == 0 {
        return;
    }
    let bw = 30;
    let ratio = completed as f64 / total as f64;
    let filled = (bw as f64 * ratio) as usize;
    let bar = format!("{}{}", "=".repeat(filled), "-".repeat(bw - filled));
    print!(
        "\r测速进度 [{}] {}/{} ({:5.1}%) 有效源: {}",
        bar,
        completed,
        total,
        ratio * 100.0,
        success
    );
    use std::io::Write;
    let _ = std::io::stdout().flush();
}

/// 为订阅源测速（每个主机测一个样本 URL）
pub async fn test_subscribe_hosts(
    channels: &[Channel],
    workers: usize,
) -> std::collections::HashMap<String, f64> {
    use crate::subscribe::host_key;
    use std::collections::HashMap;

    let mut host_channels: HashMap<String, &Channel> = HashMap::new();
    for ch in channels {
        host_channels.entry(host_key(&ch.url)).or_insert(ch);
    }

    let total = host_channels.len();
    println!("[subscribe] testing {} unique hosts...", total);
    let completed = Arc::new(AtomicUsize::new(0));
    let valid = Arc::new(AtomicUsize::new(0));
    let sem = Arc::new(Semaphore::new(workers));

    print_progress(0, total, 0);

    // 在任务内捕获一次阈值，避免并发下重复读取
    let low = speed_low();
    let mut handles = vec![];
    for (hk, ch) in host_channels {
        let sem = sem.clone();
        let url = ch.url.clone();
        let completed = completed.clone();
        let valid = valid.clone();
        let handle = tokio::spawn(async move {
            let _permit = sem.acquire().await.unwrap();
            let speed = test_one_subscribe_url(&url).await;
            let c = completed.fetch_add(1, Ordering::Relaxed) + 1;
            let v = if speed >= low {
                valid.fetch_add(1, Ordering::Relaxed) + 1
            } else {
                valid.load(Ordering::Relaxed)
            };
            print_progress(c, total, v);
            (hk, speed)
        });
        handles.push(handle);
    }

    let mut speeds = HashMap::new();
    for h in handles {
        if let Ok((hk, spd)) = h.await {
            speeds.insert(hk, if spd < low { -1.0 } else { spd });
        }
    }
    println!();
    speeds
}

async fn test_one_subscribe_url(raw_url: &str) -> f64 {
    let deadline = Instant::now() + HOST_TIMEOUT;
    let lower = raw_url.to_lowercase();
    if lower.contains(".m3u8") || lower.contains("/hls/") || lower.contains("/live/") {
        return test_stream_url(raw_url, deadline).await;
    }
    measure_speed(raw_url, deadline).await
}
