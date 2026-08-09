use crate::channel::{base_group, build_m3u8_entry, channel_sort_key};
use crate::config::{data_path, CACHE_M3U8, CACHE_TXT, EPG_URL};
use crate::types::Entry;
use std::collections::HashMap;
use std::fs;

/// 四个固定分组（固定顺序）
static GROUPS: &[&str] = &["央视频道", "卫视频道", "其他频道"];

/// 聚合所有条目、去重、排序，写入文件，返回 (m3u8, txt)
pub fn build_and_write(
    all_entries: Vec<Entry>,
    update_time: chrono::DateTime<chrono::Local>,
    per_channel: usize,
) -> (String, String) {
    // ── 按频道名分组 ─────────────────────────────────────────────
    let mut by_name: HashMap<String, Vec<Entry>> = HashMap::new();
    for e in all_entries {
        by_name.entry(e.name.clone()).or_default().push(e);
    }

    // ── 排序频道名（按分类+子序号）─────────────────────────────
    let mut all_names: Vec<String> = by_name.keys().cloned().collect();
    all_names.sort_by(|a, b| {
        let (a0, a1, a2) = channel_sort_key(a);
        let (b0, b1, b2) = channel_sort_key(b);
        a0.cmp(&b0)
            .then(a1.partial_cmp(&b1).unwrap_or(std::cmp::Ordering::Equal))
            .then(a2.cmp(&b2))
    });

    // ── 每个频道去重、按速度排序并限制输出源数（--per-channel）──
    //     舍弃的源连同其 #EXTINF(台标) 块一并消失，从根上杜绝
    //     "废弃台标对应下一个节目" 的错位问题；
    //     保留源均已通过 probe_stream 有效性验证（SPS/直链）
    let per_channel = per_channel.max(1);
    for entries in by_name.values_mut() {
        // 1) URL 完全去重
        let mut seen = std::collections::HashSet::new();
        entries.retain(|e| seen.insert(e.url.clone()));
        // 2) 按速度降序，速度相同则按 index 升序
        entries.sort_by(|a, b| {
            b.speed
                .partial_cmp(&a.speed)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.index.cmp(&b.index))
        });
        // 3) 按主机去重：同一频道内每台主机只保留最快的一个源。
        //    保证 per_channel 个备胎源来自不同主机——单台主机整体错频/失效时
        //    其余主机源仍正确；否则 3 个备胎可能全来自同一错频主机导致台标错乱
        let mut seen_hosts = std::collections::HashSet::new();
        entries.retain(|e| seen_hosts.insert(host_of_url(&e.url)));
        entries.truncate(per_channel);
    }

    let ts = update_time.format("%Y-%m-%d %H:%M:%S").to_string();
    let dummy_name = format!("更新时间: {}", ts);
    const DUMMY_URL: &str = "http://127.0.0.1/";

    // ── M3U8 ─────────────────────────────────────────────────────
    let mut m3u8_lines: Vec<String> = vec![
        format!("#EXTM3U x-tvg-url=\"{}\"", EPG_URL),
        format!("#EXT-X-UPDATED: {}", ts),
    ];
    // 按分组顺序输出
    for grp in GROUPS {
        for name in &all_names {
            if base_group(name) != *grp {
                continue;
            }
            if let Some(entries) = by_name.get(name) {
                for e in entries {
                    // 现场重建配对行：#EXTINF 与 URL 原子一致，杜绝缓存错位导致台标错乱
                    m3u8_lines.push(build_m3u8_entry(&e.name, &e.url, e.speed));
                }
            }
        }
    }
    m3u8_lines.push(format!(
        "#EXTINF:-1 group-title=\"更新时间\",{}\n{}",
        dummy_name, DUMMY_URL
    ));
    // 每个块均为 "#EXTINF…\nURL"，块间换行连接并以尾随换行收尾，
    // 保证播放器端 #EXTINF(台标) 与 URL 逐行成对解析
    let m3u8 = m3u8_lines.join("\n") + "\n";
    let _ = fs::write(data_path(CACHE_M3U8), &m3u8);

    // ── TXT ──────────────────────────────────────────────────────
    let mut group_lines: HashMap<&str, Vec<String>> = HashMap::new();
    for name in &all_names {
        let grp = base_group(name);
        if let Some(entries) = by_name.get(name) {
            for e in entries {
                group_lines
                    .entry(grp)
                    .or_default()
                    .push(format!("{},{}", e.name, e.url));
            }
        }
    }

    let mut txt_parts: Vec<String> = vec![];
    for grp in GROUPS {
        let lines = group_lines.get(*grp).cloned().unwrap_or_default();
        if lines.is_empty() {
            continue;
        }
        txt_parts.push(format!("{},#genre#", grp));
        txt_parts.extend(lines);
        txt_parts.push(String::new());
    }
    txt_parts.push("更新时间,#genre#".to_string());
    txt_parts.push(format!("{},{}", dummy_name, DUMMY_URL));
    let txt = txt_parts.join("\n");
    let _ = fs::write(data_path(CACHE_TXT), &txt);

    println!(
        "[output] m3u8 {} bytes  txt {} bytes  channels {}",
        m3u8.len(),
        txt.len(),
        all_names.len()
    );
    (m3u8, txt)
}

/// 提取 URL 主机名；解析失败时用完整 URL 作为 key（保证不同 URL 互不去重）
fn host_of_url(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(|h| h.to_string()))
        .unwrap_or_else(|| url.to_string())
}

/// 读取缓存文件
pub fn read_cache() -> (String, String) {
    let m3u8 = fs::read_to_string(data_path(CACHE_M3U8)).unwrap_or_default();
    let txt = fs::read_to_string(data_path(CACHE_TXT)).unwrap_or_default();
    (m3u8, txt)
}
