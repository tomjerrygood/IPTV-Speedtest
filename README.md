# IPTV Speed Tester

自动聚合、测速、过滤 IPTV 源，输出可直接导入播放器的 M3U8 / TXT 播放列表。

---

## 功能

- 从 API 接口获取公共 IPTV 主机，并发测速，保留高质量源
- 支持导入自定义订阅（M3U / TXT 格式均可）
- 自动清洗频道名（CCTV 别名归一、卫视排序等）
- 按速度分级（高速 ≥5 MB/s / 普通 ≥1 MB/s / 低速）分组输出
- **Cron 表达式调度**：精确指定每天/每周的测速时间点
- **启动智能跳过**：工作目录已有上次测速结果时，启动不重复测速，直接等待下次 cron
- **时区可配置**：默认 `Asia/Shanghai`，支持任意 IANA 时区
- HTTP 接口实时提供播放列表，测速期间仍正常响应（使用上次结果）
- 全程异步并发（Tokio），内存占用低

---

## HTTP 接口

| 路径 | 说明 |
|---|---|
| `GET /` 或 `/iptv` | M3U8 播放列表 |
| `GET /txt` | TXT 格式播放列表（逗号分隔） |
| `GET /status` | JSON 状态（运行状态 / 上次更新时间） |
| `GET /retest` | 立即触发重新测速（异步，不阻塞响应） |

---

## 快速开始

### 方式一：本地 Rust 编译

**前置要求**：Rust 1.70+

```bash
# 编译
./build.sh

# 运行（默认端口 3030，每天 03:23 上海时间测速）
./build.sh run

# 带自定义订阅和 cron 运行
CRON="0 4 * * *" URL1=http://your-sub-url ./build.sh run
```

### 方式二：Docker

```bash
# 构建镜像并启动容器
./build.sh docker-run

# 指定订阅源和 cron 时间
CRON="30 2 * * *" URL1=http://your-sub ./build.sh docker-run
```

### 方式三：docker-compose（推荐生产环境）

```bash
# 编辑 docker-compose.yml，在 environment 里填写订阅地址和 cron
# 然后启动：
./build.sh compose

# 或直接：
docker compose up -d --build
```

---

## 配置参数

所有参数既可以通过命令行标志，也可以通过环境变量设置。

| 参数 | 环境变量 | 默认值 | 说明 |
|---|---|---|---|
| `--port` | `PORT` | `3030` | HTTP 监听端口 |
| `--workers` | `WORKERS` | `20` | 并发测速数 |
| `--top` | `TOP` | `5` | 每类型保留最优 Host 数 |
| `--per-channel` | `PER_CHANNEL` | `3` | 每频道最多保留源数（**来自不同主机**，供播放器自动切源） |
| `--speed-low` | `SPEED_LOW` | `1.0` | 最低速率阈值 (MB/s)，低于此值丢弃 |
| `--min-resolution` | `MIN_RESOLUTION` | `1080` | 最低分辨率（高度像素），如 1080=仅 FHD 及以上；0=关闭 |
| `--cron` | `CRON` | `23 3 * * *` | 测速时间（cron 5字段：分 时 日 月 周） |
| `--timezone` | `TZ` | `Asia/Shanghai` | 时区（IANA 格式，见下方说明） |
| `--dir` | `DATA_DIR` | 当前目录 | 结果/缓存文件存放目录 |
| `--url1`..`--url20` | `URL1`..`URL20` | — | 自定义订阅源（最多 20 个） |

### 新增参数说明

> `--per-channel <N>`（默认 3）：同一个频道最多保留 **N 个源**，且这 N 个源来自 **N 台不同主机**（同一主机只保留最快的一个）。某台源不稳定/失效时，播放器可自动切换到其余主机源，避免"整台错频"导致的台标与画面错乱。

> `--speed-low <MB/s>`（默认 1.0）：测速后速率低于该值的节目会被**整体舍弃**（连同其台标块，杜绝错位）。例如只要 ≥3MB/s：`--speed-low 3.0`；只要超清源：`--speed-low 10.0`。

> `--min-resolution <高度>`（默认 1080）：按 HLS 主播放列表 `RESOLUTION` 属性过滤，分辨率为该值及以上的节目才保留。`720`=仅 HD 及以上、`2160`=仅 4K、`0`=关闭。源信息中无分辨率的节目默认保留（避免误杀）。

> `--dir <目录>`：指定结果与缓存文件的存放目录（默认当前工作目录）。`--dir /data/iptv_data` 便于多实例/开机自启统一管理。

### Cron 表达式格式

标准 5 字段，与 Linux crontab 格式一致：

```
分  时  日  月  周
*   *   *   *   *
```

常用示例：

| 表达式 | 含义 |
|---|---|
| `23 3 * * *` | 每天 03:23（默认） |
| `0 4 * * *` | 每天 04:00 |
| `0 2 * * 1` | 每周一 02:00 |
| `30 1 * * 1,4` | 每周一、四 01:30 |

### 时区配置

`--timezone` / `TZ` 接受任意 [IANA 时区名称](https://en.wikipedia.org/wiki/List_of_tz_database_time_zones)：

```bash
--timezone Asia/Shanghai      # 中国标准时间（默认）
--timezone Asia/Hong_Kong     # 香港
--timezone UTC                # 世界协调时
--timezone America/New_York   # 美国东部
--timezone Europe/London      # 英国
```

### 启动行为说明

| 工作目录状态 | 启动时行为 |
|---|---|
| `iptv_sources.m3u8` 和 `iptv_sources.txt` **都存在** | 跳过测速，直接加载缓存，等待下次 cron 触发 |
| 任一文件**不存在** | 立即执行一次测速，生成缓存后再按 cron 定时 |

测速进行期间，HTTP 服务始终返回上一次的结果，不会中断访问。

---

## docker-compose 示例

```yaml
version: "3"
services:
  iptv:
    build: .
    ports:
      - "3030:3030"
    environment:
      PORT: 3030
      WORKERS: 20
      TOP: 5
      CRON: "23 3 * * *"    # 每天 03:23 测速
      TZ: Asia/Shanghai
      URL1: http://your-first-subscribe-url
      URL2: http://your-second-subscribe-url
    volumes:
      - ./data:/app/data     # 缓存文件持久化（重启不重测）
    restart: unless-stopped
```

> **提示**：挂载 `data` 目录后，容器重启时若缓存文件仍在，会直接跳过测速，立即提供上次结果。

---

## 自定义文件

将以下文件放在工作目录（或 Docker 挂载目录）中：

| 文件 | 说明 |
|---|---|
| `channel_list.txt` | 标准频道名列表，每行一个，用于名称归一化 |
| `hsmd_address_list.txt` | 和事猫TV频道地址列表 |
| `iptv_sources.m3u8` | 上次测速结果（存在时启动跳过测速） |
| `iptv_sources.txt` | 上次测速结果（存在时启动跳过测速） |

---

## 目录结构

```
iptv-speed-tester/
├── src/
│   ├── main.rs        # 入口、CLI、cron 调度、HTTP 路由
│   ├── config.rs      # 全局常量
│   ├── types.rs       # 数据结构
│   ├── channel.rs     # 频道名清洗 / 分组 / 排序
│   ├── speedtest.rs   # 异步测速核心
│   ├── subscribe.rs   # 订阅文件下载 & 解析
│   ├── task.rs        # 主调度任务
│   ├── server.rs      # HTTP 处理器
│   └── output.rs      # M3U8 / TXT 生成 & 缓存读写
├── Cargo.toml
├── Dockerfile
├── docker-compose.yml
├── build.sh
└── README.md
```

---

## 从旧版迁移（`--interval` → `--cron`）

旧版使用 `--interval 6h` 指定固定更新间隔，且每次启动都会立即测速。新版改为：

```bash
# 旧
--interval 6h

# 新（等效：每6小时整点测速）
--cron "0 */6 * * *"

# 新（推荐：固定每天凌晨 3:23，避开高峰）
--cron "23 3 * * *"
```

主要行为差异：

| | 旧版 | 新版 |
|---|---|---|
| 调度方式 | 启动后固定间隔循环 | cron 绝对时间触发 |
| 启动测速 | 每次启动必测 | 有缓存则跳过 |
| 时区 | 硬编码上海 | `--timezone` 可配置 |
