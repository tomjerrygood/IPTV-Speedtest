mod channel;
mod config;
mod output;
mod server;
mod speedtest;
mod subscribe;
mod task;
mod types;

use crate::config::{data_path, init_data_dir, CACHE_M3U8, CACHE_TXT, DEFAULT_SUB_URL, VERSION};
use crate::output::read_cache;
use axum::{routing::get, Router};
use chrono_tz::Tz;
use clap::Parser;
use cron::Schedule;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use tokio::sync::RwLock;

// ── CLI 参数 ──────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(version = VERSION, about = "IPTV Speed Tester & Aggregator")]
struct Cli {
    /// HTTP 监听端口
    #[arg(long, env = "PORT", default_value_t = 3030)]
    port: u16,

    /// 并发测速工作数
    #[arg(long, env = "WORKERS", default_value_t = 20)]
    workers: usize,

    /// 每种类型保留前 N 个源
    #[arg(long = "top", env = "TOP", default_value_t = 5)]
    top_n: usize,

    /// Cron 表达式（5字段: 分 时 日 月 周），例如 "23 3 * * *"
    #[arg(long, env = "CRON", default_value = "23 3 * * *")]
    cron: String,

    /// 时区，例如 Asia/Shanghai、UTC、America/New_York
    #[arg(long, env = "TZ", default_value = "Asia/Shanghai")]
    timezone: String,

    /// 结果文件存放目录（不指定则使用当前工作目录）
    #[arg(long, env = "DATA_DIR")]
    dir: Option<PathBuf>,

    #[arg(long = "url1", env = "URL1")]
    url1: Option<String>,
    #[arg(long = "url2", env = "URL2")]
    url2: Option<String>,
    #[arg(long = "url3", env = "URL3")]
    url3: Option<String>,
    #[arg(long = "url4", env = "URL4")]
    url4: Option<String>,
    #[arg(long = "url5", env = "URL5")]
    url5: Option<String>,
    #[arg(long = "url6", env = "URL6")]
    url6: Option<String>,
    #[arg(long = "url7", env = "URL7")]
    url7: Option<String>,
    #[arg(long = "url8", env = "URL8")]
    url8: Option<String>,
    #[arg(long = "url9", env = "URL9")]
    url9: Option<String>,
    #[arg(long = "url10", env = "URL10")]
    url10: Option<String>,
    #[arg(long = "url11", env = "URL11")]
    url11: Option<String>,
    #[arg(long = "url12", env = "URL12")]
    url12: Option<String>,
    #[arg(long = "url13", env = "URL13")]
    url13: Option<String>,
    #[arg(long = "url14", env = "URL14")]
    url14: Option<String>,
    #[arg(long = "url15", env = "URL15")]
    url15: Option<String>,
    #[arg(long = "url16", env = "URL16")]
    url16: Option<String>,
    #[arg(long = "url17", env = "URL17")]
    url17: Option<String>,
    #[arg(long = "url18", env = "URL18")]
    url18: Option<String>,
    #[arg(long = "url19", env = "URL19")]
    url19: Option<String>,
    #[arg(long = "url20", env = "URL20")]
    url20: Option<String>,
}

impl Cli {
    fn collect_urls(&self) -> Vec<String> {
        let opts: &[&Option<String>] = &[
            &self.url1,
            &self.url2,
            &self.url3,
            &self.url4,
            &self.url5,
            &self.url6,
            &self.url7,
            &self.url8,
            &self.url9,
            &self.url10,
            &self.url11,
            &self.url12,
            &self.url13,
            &self.url14,
            &self.url15,
            &self.url16,
            &self.url17,
            &self.url18,
            &self.url19,
            &self.url20,
        ];
        let mut urls: Vec<String> = opts
            .iter()
            .filter_map(|o| o.as_deref().map(str::to_string))
            .collect();
        // 内置默认订阅（若非空）
        let default_url = DEFAULT_SUB_URL.trim();
        if !default_url.is_empty() {
            urls.push(default_url.to_string());
        }
        // 过滤空串，避免对空地址发起订阅请求
        urls.retain(|u| !u.trim().is_empty());
        urls
    }
}

// ── 共享状态 ──────────────────────────────────────────────────────

#[derive(Default)]
pub struct SharedData {
    pub m3u8: String,
    pub txt: String,
    pub last_run: String,
}

pub struct AppState {
    pub data: RwLock<SharedData>,
    pub workers: usize,
    pub top_n: usize,
    pub urls: Vec<String>,
}

// ── 程序入口 ──────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // ── 初始化数据目录（必须最先做）────────────────────────────
    init_data_dir(cli.dir.as_deref());
    println!("[main] data dir: {}", config::data_dir().display());

    let urls = cli.collect_urls();

    // 解析时区
    let tz: Tz = cli.timezone.parse().unwrap_or_else(|_| {
        eprintln!(
            "[warn] unknown timezone '{}', falling back to Asia/Shanghai",
            cli.timezone
        );
        "Asia/Shanghai".parse().unwrap()
    });

    // 解析 cron 表达式（cron crate 需要 6 字段，前置 "0 " 补秒位）
    let cron_expr = format!("0 {}", cli.cron.trim());
    let schedule = Schedule::from_str(&cron_expr).unwrap_or_else(|e| {
        eprintln!("[error] invalid cron expression '{}': {}", cli.cron, e);
        std::process::exit(1);
    });

    println!(
        "IPTV Aggregator v{}  port={}  workers={}  top={}  cron=\"{}\"  tz={}",
        VERSION, cli.port, cli.workers, cli.top_n, cli.cron, tz
    );
    println!("Subscribe URLs ({}):", urls.len());
    for (i, u) in urls.iter().enumerate() {
        println!("  {}. {}", i + 1, u);
    }

    // 检查是否存在上次测速结果
    let cache_m3u8 = data_path(CACHE_M3U8);
    let cache_txt = data_path(CACHE_TXT);
    let cache_exists = cache_m3u8.exists() && cache_txt.exists();

    // 恢复缓存，让 HTTP 服务立刻可用
    let (m3u8, txt) = read_cache();

    // 确定 last_run 初始值
    let last_run_init = if cache_exists {
        get_file_mtime(&cache_m3u8.to_string_lossy())
            .unwrap_or_else(|| "cached (unknown time)".to_string())
    } else {
        "Never".to_string()
    };

    let state = Arc::new(AppState {
        data: RwLock::new(SharedData {
            m3u8,
            txt,
            last_run: last_run_init,
        }),
        workers: cli.workers,
        top_n: cli.top_n,
        urls: urls.clone(),
    });

    if cache_exists {
        println!(
            "[main] cache files found ({} + {}), skipping startup speed-test — waiting for cron.",
            cache_m3u8.display(),
            cache_txt.display()
        );
    } else {
        println!("[main] no cache found, running initial speed-test immediately.");
        let st = state.clone();
        let us = urls.clone();
        let (w, t) = (cli.workers, cli.top_n);
        tokio::spawn(async move { task::run_task(st, w, t, us).await });
    }

    // ── Cron 调度循环 ─────────────────────────────────────────────
    {
        let st = state.clone();
        let us = urls.clone();
        let (w, t) = (cli.workers, cli.top_n);

        tokio::spawn(async move {
            loop {
                // 以指定时区的当前时间为基准求下一触发点
                let now_utc = chrono::Utc::now();
                let now_tz = now_utc.with_timezone(&tz);

                let next_opt = schedule.after(&now_tz).next();
                let Some(next_time) = next_opt else {
                    eprintln!("[cron] schedule exhausted, stopping.");
                    break;
                };

                // 转回 UTC 计算等待秒数
                let next_utc = next_time.with_timezone(&chrono::Utc);
                let wait_secs = (next_utc - now_utc).num_seconds().max(0) as u64;

                println!(
                    "[cron] next run at {} ({}) — sleeping {}s ({:.1}h)",
                    next_time.format("%Y-%m-%d %H:%M:%S %Z"),
                    tz,
                    wait_secs,
                    wait_secs as f64 / 3600.0
                );

                tokio::time::sleep(tokio::time::Duration::from_secs(wait_secs)).await;

                println!("[cron] triggered — starting speed-test...");
                let st2 = st.clone();
                let us2 = us.clone();
                tokio::spawn(task::run_task(st2, w, t, us2));
            }
        });
    }

    // ── HTTP 路由 ─────────────────────────────────────────────────
    let app = Router::new()
        .route("/", get(server::handle_m3u8))
        .route("/iptv", get(server::handle_m3u8))
        .route("/txt", get(server::handle_txt))
        .route("/status", get(server::handle_status))
        .route("/retest", get(server::handle_force_retest))
        .with_state(state);

    let addr = format!("0.0.0.0:{}", cli.port);
    println!("[main] listening on http://{}", addr);
    let listener = match tokio::net::TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!(
                "[error] failed to bind {}: {}",
                addr, e
            );
            eprintln!("[error] the port may already be in use by another instance.");
            eprintln!("[error] fix: kill the old process (pkill -f iptv-speed-tester)");
            eprintln!("[error]   or start with a different port: --port <PORT>");
            std::process::exit(1);
        }
    };
    if let Err(e) = axum::serve(listener, app).await {
        eprintln!("[error] server error: {}", e);
        std::process::exit(1);
    }
}

/// 读取文件最后修改时间，格式化为本地时间字符串
fn get_file_mtime(path: &str) -> Option<String> {
    use std::time::UNIX_EPOCH;
    let meta = std::fs::metadata(path).ok()?;
    let mtime = meta.modified().ok()?;
    let secs = mtime.duration_since(UNIX_EPOCH).ok()?.as_secs() as i64;
    let dt = chrono::DateTime::<chrono::Utc>::from_timestamp(secs, 0)?;
    let local = dt.with_timezone(&chrono::Local);
    Some(local.format("%Y-%m-%d %H:%M:%S").to_string())
}
