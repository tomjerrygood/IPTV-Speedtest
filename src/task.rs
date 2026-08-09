use crate::channel::{
    build_m3u8_entry, clean_channel_name, get_standard_channel_map, map_to_standard_name,
};
use crate::config::{speed_low, API_URL, HSMD_ADDRESS_LIST_FILE};
use crate::output::build_and_write;
use crate::speedtest::{fetch_channels_for_source, run_api_speed_tests, test_subscribe_hosts};
use crate::subscribe::{download_subscribes, host_key, parse_subscribe_file};
use crate::types::{Entry, SourceResult};
use crate::AppState;
use once_cell::sync::Lazy;
use regex::Regex;
use reqwest::Client;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::Semaphore;
use url::Url;

static IS_RUNNING: Lazy<AtomicBool> = Lazy::new(|| AtomicBool::new(false));

pub fn is_running() -> bool {
    IS_RUNNING.load(Ordering::Relaxed)
}

/// 主调度任务
pub async fn run_task(
    state: std::sync::Arc<AppState>,
    workers: usize,
    top_n: usize,
    per_channel: usize,
    urls: Vec<String>,
) {
    if IS_RUNNING
        .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
        .is_err()
    {
        println!("[task] already running, skipping");
        return;
    }
    let start = std::time::Instant::now();
    println!("[task] ── start ──────────────────────────────────────────────");

    let std_map = get_standard_channel_map();
    let mut all_entries: Vec<Entry> = vec![];
    let mut source_idx = 0usize;

    // ── Step 1: 下载订阅文件 ──────────────────────────────────────
    println!("[task] downloading subscribe files...");
    let sub_cache = download_subscribes(&urls).await;

    // ── Step 2 & 3: 获取 + 测速 API 主机 ─────────────────────────
    let api_items = fetch_api_data().await;
    if !api_items.is_empty() {
        println!("[task] speed-testing {} API hosts...", api_items.len());
        let raw_results = run_api_speed_tests(api_items, workers).await;
        let mut top_sources = select_top_sources(raw_results, top_n);
        println!("[task] selected {} API sources", top_sources.len());

        for (idx, src) in top_sources.iter_mut().enumerate() {
            println!(
                "  [api] #{} {:.2}MB/s [{}] {} ({})",
                idx + 1,
                src.speed,
                crate::channel::speed_tier(src.speed),
                src.host,
                src.match_type
            );
            fetch_channels_for_source(src).await;
            let entries = match src.match_type.as_str() {
                "txiptv" | "zhgxtv" | "jsmpeg" => {
                    build_entries(&src.channels, source_idx, src.speed, &std_map)
                }
                "hsmdtv" => process_hsmdtv_channels(&src.host, source_idx, src.speed, &std_map),
                _ => vec![],
            };
            all_entries.extend(entries);
            source_idx += 1;
        }
    }

    // ── Step 4: 测速订阅源 ────────────────────────────────────────
    for (raw_url, cache_path) in &sub_cache {
        let channels = parse_subscribe_file(cache_path);
        if channels.is_empty() {
            println!("[subscribe] no channels parsed from {}", raw_url);
            continue;
        }
        println!(
            "[subscribe] {} channels from {} — testing hosts...",
            channels.len(),
            raw_url
        );
        let host_speeds = test_subscribe_hosts(&channels, workers).await;

        let low = speed_low();
        let mut added = 0usize;
        for ch in &channels {
            let hk = host_key(&ch.url);
            let spd = match host_speeds.get(&hk) {
                Some(&s) if s >= low => s,
                _ => continue,
            };
            let name = map_to_standard_name(&clean_channel_name(&ch.name), &std_map).to_string();
            all_entries.push(Entry {
                content: build_m3u8_entry(&name, &ch.url, spd),
                name,
                url: ch.url.clone(),
                index: source_idx,
                speed: spd,
            });
            added += 1;
        }
        println!("[subscribe] kept {} / {} channels", added, channels.len());
        source_idx += 1;
    }

    if all_entries.is_empty() {
        println!("[task] no entries collected, keeping cache");
        IS_RUNNING.store(false, Ordering::Release);
        return;
    }

    // ── Step 5: 按分辨率过滤（可选，--min-resolution）─────────────
    let min_res = crate::config::min_resolution();
    if min_res > 0 {
        let before = all_entries.len();
        all_entries = filter_entries_by_resolution(all_entries, min_res).await;
        println!(
            "[task] resolution filter (>= {}p): kept {} / dropped {}",
            min_res,
            all_entries.len(),
            before - all_entries.len()
        );
    }

    if all_entries.is_empty() {
        println!("[task] no entries after filtering, keeping cache");
        IS_RUNNING.store(false, Ordering::Release);
        return;
    }

    // ── Step 6: 构建并写入输出 ────────────────────────────────────
    let update_time = chrono::Local::now();
    let (m3u8, txt) = build_and_write(all_entries, update_time, per_channel);

    {
        let mut guard = state.data.write().await;
        guard.m3u8 = m3u8;
        guard.txt = txt;
        guard.last_run = update_time.format("%Y-%m-%d %H:%M:%S").to_string();
    }

    IS_RUNNING.store(false, Ordering::Release);
    println!("[task] done — elapsed {}s", start.elapsed().as_secs());
}

// ── 内部辅助 ──────────────────────────────────────────────────────

async fn fetch_api_data() -> Vec<serde_json::Map<String, Value>> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .unwrap();
    for attempt in 1..=3 {
        println!("[api] fetch attempt {}: {}", attempt, API_URL);
        if let Ok(resp) = client.get(API_URL).send().await {
            if resp.status() == 200 {
                if let Ok(data) = resp.json::<Value>().await {
                    if let Some(results) = data["results"].as_array() {
                        let out: Vec<_> = results
                            .iter()
                            .filter_map(|r| r.as_object().cloned())
                            .collect();
                        println!("[api] received {} hosts", out.len());
                        return out;
                    }
                }
            }
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    println!("[api] fetch failed after 3 retries");
    vec![]
}

fn select_top_sources(mut results: Vec<SourceResult>, top_n: usize) -> Vec<SourceResult> {
    results.sort_by(|a, b| {
        b.speed
            .partial_cmp(&a.speed)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    let mut selected_hosts = std::collections::HashSet::new();
    let mut final_results: Vec<SourceResult> = vec![];

    // 每种类型至少保留一个
    for mt in &["txiptv", "hsmdtv", "zhgxtv", "jsmpeg"] {
        if let Some(r) = results
            .iter()
            .find(|r| r.match_type == *mt && !selected_hosts.contains(&r.host))
        {
            selected_hosts.insert(r.host.clone());
            final_results.push(r.clone());
        }
    }
    // 填充至 top_n
    for r in &results {
        if final_results.len() >= top_n {
            break;
        }
        if !selected_hosts.contains(&r.host) {
            selected_hosts.insert(r.host.clone());
            final_results.push(r.clone());
        }
    }
    final_results.sort_by(|a, b| {
        b.speed
            .partial_cmp(&a.speed)
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    final_results
}

fn build_entries(
    channels: &[crate::types::Channel],
    idx: usize,
    speed: f64,
    std_map: &std::collections::HashMap<String, String>,
) -> Vec<Entry> {
    channels
        .iter()
        .map(|ch| {
            let name = map_to_standard_name(&clean_channel_name(&ch.name), std_map).to_string();
            Entry {
                content: build_m3u8_entry(&name, &ch.url, speed),
                name,
                url: ch.url.clone(),
                index: idx,
                speed,
            }
        })
        .collect()
}

static RE_URL: Lazy<Regex> = Lazy::new(|| Regex::new(r"(http://[^\s]+)").unwrap());
static RE_ID: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*\d+\s+").unwrap());

/// 按最低分辨率过滤节目 + 流有效性验证：
/// - HLS 源：SPS 实测分辨率 >= min_h 才保留，解析失败/不可播即剔除
/// - 非 HLS 直链：收到有效媒体数据即保留（(0,0) 分辨率未知），死链/错误页剔除
async fn filter_entries_by_resolution(entries: Vec<Entry>, min_h: u32) -> Vec<Entry> {
    use crate::speedtest::probe_stream;

    let total = entries.len();
    let sem = Arc::new(Semaphore::new(32));
    let mut handles = Vec::with_capacity(total);
    for (i, e) in entries.iter().enumerate() {
        let url = e.url.clone();
        let sem = sem.clone();
        handles.push((
            i,
            tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                probe_stream(&url).await
            }),
        ));
    }

    let mut kept = Vec::with_capacity(total);
    let mut dropped = 0usize;
    for (i, h) in handles {
        match h.await.ok().flatten() {
            Some((0, 0)) => kept.push(entries[i].clone()), // 有效直链，分辨率未知，保留
            Some((_w, hgt)) if hgt > 0 && hgt < min_h => dropped += 1,
            Some(_) => kept.push(entries[i].clone()), // 分辨率达标
            None => dropped += 1, // 探测失败（HLS SPS 解析失败 / 直链死链或错误页）→ 一律剔除
        }
    }
    let _ = total;
    println!(
        "[task] stream probe done: kept {} / dropped {}",
        kept.len(),
        dropped
    );
    kept
}

fn process_hsmdtv_channels(
    host: &str,
    source_index: usize,
    speed: f64,
    std_map: &std::collections::HashMap<String, String>,
) -> Vec<Entry> {
    let Ok(data) = std::fs::read_to_string(HSMD_ADDRESS_LIST_FILE) else {
        println!("[hsmd] {} not found, skipping", HSMD_ADDRESS_LIST_FILE);
        return vec![];
    };
    let mut entries = vec![];
    for line in data.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Some(loc) = RE_URL.find(line) else {
            continue;
        };
        let url_in_file = loc.as_str();
        let before = &line[..loc.start()];
        let name_raw = RE_ID
            .replace(before, "")
            .replace("（默认频道）", "")
            .trim()
            .to_string();
        let name = map_to_standard_name(&clean_channel_name(&name_raw), std_map).to_string();
        let Ok(p) = Url::parse(url_in_file) else {
            continue;
        };
        let new_url = format!("http://{}{}", host, p.path());
        entries.push(Entry {
            content: build_m3u8_entry(&name, &new_url, speed),
            name,
            url: new_url,
            index: source_index,
            speed,
        });
    }
    entries
}
