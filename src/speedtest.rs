use crate::config::*;
use crate::types::{Channel, SourceResult};
use reqwest::Client;
use serde_json::Value;
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

// ── 分辨率探测 ───────────────────────────────────────────────────

/// 从 H.264 比特流中读取 1 bit
struct BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, bit_pos: 0 }
    }
    fn read_bit(&mut self) -> u32 {
        if self.bit_pos >= self.data.len() * 8 {
            return 0;
        }
        let byte = self.data[self.bit_pos / 8];
        let shift = 7 - (self.bit_pos % 8);
        self.bit_pos += 1;
        ((byte >> shift) & 1) as u32
    }
    fn read_bits(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            v = (v << 1) | self.read_bit();
        }
        v
    }
}

/// Exp-Golomb 无符号指数哥伦布编码
fn read_ue(r: &mut BitReader) -> u32 {
    let mut zeros = 0u32;
    while r.read_bit() == 0 {
        zeros += 1;
        if zeros > 31 {
            return 0;
        }
    }
    if zeros == 0 {
        return 0;
    }
    let info = r.read_bits(zeros);
    (1u32 << zeros) - 1 + info
}

/// Exp-Golomb 有符号指数哥伦布编码
fn read_se(r: &mut BitReader) -> i32 {
    let u = read_ue(r);
    if u & 1 == 1 {
        ((u + 1) / 2) as i32
    } else {
        -((u / 2) as i32)
    }
}

/// 解析 H.264 SPS NAL 内容（不含 start code 与 NAL header 0x67）→ (宽, 高)
fn parse_h264_sps(sps: &[u8]) -> Option<(u32, u32)> {
    if sps.len() < 4 {
        return None;
    }
    let profile_idc = sps[0];
    let mut r = BitReader::new(&sps[3..]);

    let _seq_parameter_set_id = read_ue(&mut r);

    let high_profile = matches!(
        profile_idc,
        100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135
    );
    let chroma_format_idc = if high_profile {
        let c = read_ue(&mut r);
        if c == 3 {
            let _separate_colour_plane_flag = r.read_bit();
        }
        let _bit_depth_luma_minus8 = read_ue(&mut r);
        let _bit_depth_chroma_minus8 = read_ue(&mut r);
        let _qpprime_y_zero_transform_bypass = r.read_bit();
        let scaling_matrix_present = r.read_bit();
        if scaling_matrix_present > 0 {
            let n = if c == 3 { 12 } else { 8 };
            for i in 0..n {
                let flag = r.read_bit();
                if flag > 0 {
                    let size = if i < 6 { 16 } else { 64 };
                    let mut last = 8i32;
                    let mut next = 8i32;
                    for _ in 0..size {
                        if next != 0 {
                            let delta = read_se(&mut r);
                            next = (last + delta).clamp(0, 255);
                            last = next;
                        }
                    }
                }
            }
        }
        c
    } else {
        1 // 默认 4:2:0
    };

    let _log2_max_frame_num_minus4 = read_ue(&mut r);
    let pic_order_cnt_type = read_ue(&mut r);
    if pic_order_cnt_type == 0 {
        let _log2_max_poc_lsb_minus4 = read_ue(&mut r);
    } else if pic_order_cnt_type == 1 {
        let _delta_pic_order_always_zero = r.read_bit();
        let _offset_for_non_ref_pic = read_se(&mut r);
        let _offset_for_top_to_bottom = read_se(&mut r);
        let n = read_ue(&mut r);
        for _ in 0..n {
            let _off = read_se(&mut r);
        }
    }
    let _max_num_ref_frames = read_ue(&mut r);
    let _gaps_in_frame_num = r.read_bit();

    let pic_width_mbs = read_ue(&mut r);
    let mut width = (pic_width_mbs + 1) * 16;

    let pic_height_map_units = read_ue(&mut r);
    let frame_mbs_only = r.read_bit();
    let height_mbs = (2 - frame_mbs_only) * (pic_height_map_units + 1);
    let mut height = height_mbs * 16;

    if frame_mbs_only == 0 {
        let _mb_adaptive_frame_field = r.read_bit();
    }
    let _direct_8x8 = r.read_bit();

    // frame_cropping（剔除黑边），chroma 4:2:0 时 CropUnit=2
    if r.read_bit() > 0 {
        let crop_left = read_ue(&mut r);
        let crop_right = read_ue(&mut r);
        let crop_top = read_ue(&mut r);
        let crop_bottom = read_ue(&mut r);
        let crop_unit = if chroma_format_idc == 0 { 1 } else { 2 };
        width = width.saturating_sub((crop_left + crop_right) * crop_unit);
        height = height.saturating_sub((crop_top + crop_bottom) * crop_unit);
    }

    if width == 0 || height == 0 {
        return None;
    }
    Some((width, height))
}

/// 在字节流中定位 00 00 01 或 00 00 00 01 起始码，返回 (起始码长度, NAL 头位置)
fn find_start_code(buf: &[u8], from: usize) -> Option<(usize, usize)> {
    let mut i = from;
    while i + 3 <= buf.len() {
        if buf[i] == 0 && buf[i + 1] == 0 {
            if buf[i + 2] == 1 {
                return Some((3, i + 3));
            }
            if i + 4 <= buf.len() && buf[i + 2] == 0 && buf[i + 3] == 1 {
                return Some((4, i + 4));
            }
        }
        i += 1;
    }
    None
}

/// 从单行 STREAM-INF 中解析 RESOLUTION=WxH
fn parse_resolution_from_line(line: &str) -> Option<(u32, u32)> {
    let idx = line.find("RESOLUTION=")?;
    let after = &line[idx + "RESOLUTION=".len()..];
    let value: String = after
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == 'x')
        .collect();
    let mut it = value.splitn(2, 'x');
    Some((it.next()?.parse().ok()?, it.next()?.parse().ok()?))
}

/// 解析 master playlist 的 RESOLUTION 属性，返回最大档位
fn parse_master_resolution(body: &str) -> Option<(u32, u32)> {
    body.lines()
        .filter_map(parse_resolution_from_line)
        .max_by_key(|&(w, h)| w * h)
}

/// 从 master playlist 提取 RESOLUTION 最大档位对应的变体 URL
fn master_top_variant_url(master_url: &str, body: &str) -> Option<(u32, u32, String)> {
    let parsed = Url::parse(master_url).ok()?;
    let origin = format!("{}://{}", parsed.scheme(), parsed.host_str()?);
    let base = &master_url[..master_url.rfind('/')? + 1];
    let lines: Vec<&str> = body.lines().collect();
    let mut best: Option<(usize, u32, u32)> = None;
    for (i, line) in lines.iter().enumerate() {
        if !line.contains("#EXT-X-STREAM-INF") {
            continue;
        }
        if let Some((w, h)) = parse_resolution_from_line(line) {
            if best.map_or(true, |(_, bw, bh)| (w * h) > (bw * bh)) {
                best = Some((i, w, h));
            }
        }
    }
    let (li, w, h) = best?;
    let vline = lines.get(li + 1)?.trim();
    if vline.is_empty() || vline.starts_with('#') {
        return None;
    }
    let vurl = if vline.starts_with("http") {
        vline.to_string()
    } else if vline.starts_with('/') {
        format!("{}{}", origin, vline)
    } else {
        format!("{}{}", base, vline)
    };
    Some((w, h, vurl))
}

/// 对 media playlist 变体 URL：GET 后解析第一个 TS 分片 SPS（验证真实可播）
async fn probe_variant_sps(variant_url: &str) -> Option<(u32, u32)> {
    let client = make_client(Duration::from_secs(4));
    let resp = client.get(variant_url).send().await.ok()?;
    if resp.status() != 200 {
        return None;
    }
    let body = resp.text().await.ok()?;
    if body.contains("#EXT-X-STREAM-INF") {
        // 变体又套 master（少见）：递归取最高档
        let (_, _, v2) = master_top_variant_url(variant_url, &body)?;
        return probe_variant_sps(&v2).await;
    }
    probe_resolution_from_ts(variant_url, &body).await
}

/// 解析单码率 media playlist：下载 TS 分片，做 TS 解复用后解析 H.264 SPS 获取真实分辨率
async fn probe_resolution_from_ts(url: &str, playlist: &str) -> Option<(u32, u32)> {
    use futures_util::StreamExt;

    let ts_url = {
        let parsed = Url::parse(url).ok()?;
        let origin = format!("{}://{}", parsed.scheme(), parsed.host_str()?);
        let base = &url[..url.rfind('/')? + 1];
        let mut found = None;
        for line in playlist.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            found = Some(if line.starts_with("http") {
                line.to_string()
            } else if line.starts_with('/') {
                format!("{}{}", origin, line)
            } else {
                format!("{}{}", base, line)
            });
            break;
        }
        found?
    };

    // 只下载前 1MB（SPS 位于首个 GOP 开头）
    let client = make_client(Duration::from_secs(6));
    let resp = client.get(&ts_url).send().await.ok()?;
    if resp.status() >= reqwest::StatusCode::BAD_REQUEST {
        return None;
    }
    let mut buf = Vec::with_capacity(1024 * 1024);
    let mut stream = resp.bytes_stream();
    while buf.len() < 1024 * 1024 {
        match tokio::time::timeout(Duration::from_secs(3), stream.next()).await {
            Ok(Some(Ok(chunk))) => buf.extend_from_slice(&chunk),
            _ => break,
        }
    }
    if buf.len() < 188 * 2 {
        return None;
    }

    // TS 同步：找到两个连续 0x47 包
    let mut sync = None;
    for i in 0..buf.len() - 376 {
        if buf[i] == 0x47 && buf[i + 188] == 0x47 {
            sync = Some(i);
            break;
        }
    }
    let mut pos = sync?;

    // 遍历 TS 包：定位 PUSI（payload 起始）包，在其 payload 中找 H.264 SPS（NAL type 7）
    while pos + 188 <= buf.len() {
        if buf[pos] != 0x47 {
            // 失步，重新同步
            if pos >= buf.len().saturating_sub(376) {
                return None;
            }
            sync = None;
            for i in pos..buf.len() - 376 {
                if buf[i] == 0x47 && buf[i + 188] == 0x47 {
                    sync = Some(i);
                    break;
                }
            }
            let Some(s) = sync else { return None };
            pos = s;
            continue;
        }
        let b1 = buf[pos + 1];
        let b3 = buf[pos + 3];
        let pusi = b1 & 0x40 != 0;
        let has_af = b3 & 0x20 != 0;
        let has_payload = b3 & 0x10 != 0;
        let af_len = if has_af {
            buf.get(pos + 4).copied().unwrap_or(0) as usize
        } else {
            0
        };
        let payload_start = pos + 4 + af_len;
        if pusi && has_payload && payload_start < pos + 188 {
            let window = &buf[payload_start..(pos + 188).min(buf.len())];
            // 窗口内循环查找 SPS 起始码；PES 头（00 00 01 E0）会被低 5 位检查排除
            let mut find_from = 0usize;
            loop {
                let Some((_, nal_start)) = find_start_code(window, find_from) else {
                    break;
                };
                let Some(&nal_byte) = window.get(nal_start) else {
                    break;
                };
                if nal_byte & 0x1f != 7 {
                    find_from = nal_start + 1;
                    continue;
                }
                // 拼接 SPS：本包剩余 + 后续包直至遇到新起始码或满 192 字节
                let mut sps = window[nal_start + 1..].to_vec();
                let mut p2 = pos + 188;
                while sps.len() < 192 && p2 + 188 <= buf.len() {
                    let b3b = buf[p2 + 3];
                    let has_afb = b3b & 0x20 != 0;
                    let afb = if has_afb {
                        buf.get(p2 + 4).copied().unwrap_or(0) as usize
                    } else {
                        0
                    };
                    let pl = p2 + 4 + afb;
                    if pl < buf.len() {
                        // 下一包 payload 起始是新起始码则停止（SPS 已结束）
                        let a = buf.get(pl).copied().unwrap_or(0);
                        let b = buf.get(pl + 1).copied().unwrap_or(0);
                        let c = buf.get(pl + 2).copied().unwrap_or(0);
                        let d = buf.get(pl + 3).copied().unwrap_or(0);
                        if (a == 0 && b == 0 && c == 1)
                            || (a == 0 && b == 0 && c == 0 && d == 1)
                        {
                            break;
                        }
                        let end = (p2 + 188).min(buf.len());
                        sps.extend_from_slice(&buf[pl..end]);
                    }
                    p2 += 188;
                }
                if sps.len() >= 4 {
                    if let Some(r) = parse_h264_sps(&sps) {
                        return Some(r);
                    }
                }
                // 解析失败（可能拼到错误 NAL），尝试窗口内下一个候选
                find_from = nal_start + 1;
            }
        }
        pos += 188;
    }
    None
}

/// 探测节目真实分辨率 (宽, 高)。
///
/// 两级探测：
/// 1. master playlist：`#EXT-X-STREAM-INF:...RESOLUTION=WxH`（多码率精确属性）
/// 2. 单码率 media playlist：下载首个 TS 分片头部，解析 H.264 SPS 还原分辨率
///
/// 非 HLS 直链（flv/ts）或无法解析的返回 None。
pub async fn probe_resolution(url: &str) -> Option<(u32, u32)> {
    // 非 HLS 直链（flv/ts 等）跳过探测，避免整段下载浪费时间
    let lower = url.to_lowercase();
    if !(lower.contains(".m3u8") || lower.contains("/hls/") || lower.contains("/live/")) {
        return None;
    }
    let client = make_client(Duration::from_secs(4));
    let resp = client.get(url).send().await.ok()?;
    if resp.status() != 200 {
        return None;
    }
    let body = resp.text().await.ok()?;

    // 1) master playlist：取最大分辨率变体并下载其 TS 分片验证真实可播（SPS 解析成功才算有效内容）
    if body.contains("#EXT-X-STREAM-INF") {
        let (w, h, vurl) = master_top_variant_url(url, &body)?;
        return probe_variant_sps(&vurl).await.filter(|&(rw, rh)| {
            // 变体 SPS 实测分辨率与属性一致（允许 16 像素倍差取整误差）
            let ok = rw <= w + 16 && rh <= h + 16 && rw >= w.saturating_sub(16) && rh >= h.saturating_sub(16);
            if !ok {
                println!("[probe] variant {}x{} declared vs {}x{} actual — reject", w, h, rw, rh);
            }
            ok
        });
    }
    // 2) 单码率 media playlist：解析 TS 分片 SPS
    probe_resolution_from_ts(url, &body).await
}

/// 直链（flv/ts/mp4 等非 HLS）快速有效性验证：下载前 256KB，确认返回真实媒体数据
async fn probe_direct_stream(url: &str) -> Option<(u32, u32)> {
    use futures_util::StreamExt;

    let client = make_client(Duration::from_secs(6));
    let resp = client.get(url).send().await.ok()?;
    if resp.status() >= reqwest::StatusCode::BAD_REQUEST {
        return None;
    }
    let mut buf = Vec::with_capacity(256 * 1024);
    let mut stream = resp.bytes_stream();
    while buf.len() < 256 * 1024 {
        match tokio::time::timeout(Duration::from_secs(3), stream.next()).await {
            Ok(Some(Ok(chunk))) => buf.extend_from_slice(&chunk),
            _ => break,
        }
    }
    if buf.is_empty() {
        return None;
    }
    // HTTP 200 但返回 HTML/XML 错误页 → 无效源
    if buf[0] == b'<' {
        return None;
    }
    let lower = url.to_lowercase();
    if lower.ends_with(".flv") || buf.starts_with(b"FLV") || lower.ends_with(".ts") {
        return Some((0, 0)); // 有效媒体直链，分辨率未知
    }
    // 其他直链：收到 ≥64KB 非页面数据视为有效流
    if buf.len() >= 64 * 1024 {
        return Some((0, 0));
    }
    None
}

/// 探测节目流有效性 + 真实分辨率。
/// - HLS：解析 TS 分片 SPS 精确认证（同时验证真实可播），返回 (宽, 高)
/// - 非 HLS 直链：有效性验证通过返回 (0, 0)（分辨率未知），死链/错误页返回 None
pub async fn probe_stream(url: &str) -> Option<(u32, u32)> {
    let lower = url.to_lowercase();
    if lower.contains(".m3u8") || lower.contains("/hls/") || lower.contains("/live/") {
        return probe_resolution(url).await;
    }
    probe_direct_stream(url).await
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
