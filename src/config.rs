use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Duration;

pub const VERSION: &str = "3.0.0";

// 文件名（不含目录，目录由 data_dir() 决定）
pub const CACHE_M3U8: &str = "iptv_sources.m3u8";
pub const CACHE_TXT: &str = "iptv_sources.txt";
pub const CHANNEL_LIST_FILE: &str = "channel_list.txt";
pub const HSMD_ADDRESS_LIST_FILE: &str = "hsmd_address_list.txt";

// 远程端点
pub const API_URL: &str = "https://iptvs.pes.im";
pub const EPG_URL: &str = "https://epg.zsdc.eu.org/t.xml";
pub const LOGO_BASE_URL: &str =
    "https://ghfast.top/https://raw.githubusercontent.com/Jarrey/iptv_logo/main/tv/";
pub const DEFAULT_SUB_URL: &str = "";

// IPTV 类型路径
pub const ZHGXTV_INTERFACE: &str = "/ZHGXTV/Public/json/live_interface.txt";
pub const HSMDTV_TEST_URI: &str = "/newlive/live/hls/1/live.m3u8";

// 速度分级 (MB/s)
pub const SPEED_HIGH: f64 = 5.0;
pub const SPEED_MID: f64 = 1.0;

// 最低速率阈值默认值 (MB/s)：低于此值的源/节目会被舍弃
pub const DEFAULT_SPEED_LOW: f64 = 5.0;

// 最低速率阈值 (MB/s)，由 CLI 参数 --speed-low 设置，运行前初始化一次
static SPEED_LOW: OnceLock<f64> = OnceLock::new();

/// 初始化最低速率阈值，main() 解析参数后调用一次
pub fn init_speed_low(v: f64) {
    let _ = SPEED_LOW.set(v);
}

/// 当前最低速率阈值 (MB/s)
pub fn speed_low() -> f64 {
    *SPEED_LOW.get().unwrap_or(&DEFAULT_SPEED_LOW)
}

// 最低分辨率阈值默认值（0 = 关闭）
pub const DEFAULT_MIN_RESOLUTION: u32 = 0;

// 最低分辨率阈值（高度像素），由 CLI 参数 --min-resolution 设置，运行前初始化一次
static MIN_RESOLUTION: OnceLock<u32> = OnceLock::new();

/// 初始化最低分辨率阈值，main() 解析参数后调用一次
pub fn init_min_resolution(v: u32) {
    let _ = MIN_RESOLUTION.set(v);
}

/// 当前最低分辨率阈值（高度像素，0 = 关闭）。仅保留源信息中分辨率为该值及以上的节目。
pub fn min_resolution() -> u32 {
    *MIN_RESOLUTION.get().unwrap_or(&DEFAULT_MIN_RESOLUTION)
}

// 超时 / 批次
pub const HOST_TIMEOUT: Duration = Duration::from_secs(15);
pub const SUB_TIMEOUT: Duration = Duration::from_secs(10);
pub const SPEED_TEST_SECS: Duration = Duration::from_secs(8);
pub const BATCH_SIZE: usize = 60;

// ── 运行时数据目录 ────────────────────────────────────────────────

static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

/// 在 main() 解析完参数后调用一次，之后不可再改。
/// `dir` 为 None 时使用当前工作目录。
pub fn init_data_dir(dir: Option<&Path>) {
    let path = match dir {
        Some(p) => p.to_path_buf(),
        None => std::env::current_dir().expect("cannot determine current directory"),
    };
    // 目录不存在则自动创建
    if !path.exists() {
        std::fs::create_dir_all(&path)
            .unwrap_or_else(|e| panic!("cannot create data dir {:?}: {}", path, e));
    }
    DATA_DIR.set(path).ok(); // 忽略重复 set（测试场景）
}

/// 返回数据目录，必须在 init_data_dir() 之后调用。
pub fn data_dir() -> &'static PathBuf {
    DATA_DIR.get().expect("data_dir not initialized; call init_data_dir() first")
}

/// 拼出数据目录下的完整路径
pub fn data_path(filename: &str) -> PathBuf {
    data_dir().join(filename)
}
