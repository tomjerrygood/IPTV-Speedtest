# S905 盒子 (Android 7.1 ~ 9.0) 部署指南

本仓库的 GitHub Actions 会将本项目交叉编译为可在 S905 系列电视盒 Android **32 位**系统上直接运行的 ELF 可执行文件。

---

## 构建产物

| 产物 | 架构 | 说明 |
|---|---|---|
| `iptv-speed-tester-armv7.zip` | **armv7 (32-bit)** | S905 盒子 Android 7.1 / 8.1 / 9.0（绝大多数盒子的默认 32 位系统）|

> S905 盒子出厂系统绝大多数为 32 位，armv7 版即通用选择。

---

## 如何获得编译产物

### 方式一：GitHub Actions 手动触发（无需打 tag）

1. 打开仓库页面 → **Actions** → 左侧选择 **Build Android (S905)**
2. 点击 **Run workflow** → 选择分支 → **Run**
3. 等待构建完成（约 10~15 分钟），在 **Artifacts** 区域下载 `iptv-speed-tester-armv7.zip`

### 方式二：打 tag 自动发布

推送 `v*` 格式的 tag（如 `v3.0.0`），工作流会自动编译并上传到 GitHub **Release** 页面：

```bash
git tag v3.0.0
git push origin v3.0.0
```

---

## 在盒子上安装运行

### 前置条件

- 盒子已 **root**（可用 Magisk / 当贝 / 潜龙等方案）
- 安装一个终端模拟器（推荐 **Termux**、**MT管理器** 或 **Terminal Emulator**）
- 使用 `adb` 或 U 盘将 zip 解压后的 `iptv` 文件拷入盒子

### 步骤

```bash
# 1. 将 iptv 推送至 /data/local/tmp（adb 方式）
adb push iptv /data/local/tmp/iptv
adb shell

# 2. 赋予执行权限（在盒子 shell 中）
su
mount -o remount,rw /system
cp /data/local/tmp/iptv /data/iptv
chmod 755 /data/iptv

# 3. 运行（默认端口 3030，每天 03:23 自动测速）
/data/iptv --port 3030 --workers 20 --top 5 --cron "23 3 * * *" --timezone Asia/Shanghai
```

> 盒子 CPU 较弱，建议 `--workers` 保持 10~20，避免过载。

### 开机自启（可选）

`/data` 目录下创建启动脚本 `start_iptv.sh`：

```sh
#!/system/bin/sh
/data/iptv --port 3030 --workers 15 --top 5 --cron "23 3 * * *" --timezone Asia/Shanghai &
```

配合 **Boot Manager / 自启动管理 App** 或 Magisk `service.sh` 实现开机自启。

---

## 验证

浏览器（或盒子上任意设备）访问：

```
http://<盒子IP>:3030/iptv     # M3U8 播放列表
http://<盒子IP>:3030/txt      # TXT 播放列表
http://<盒子IP>:3030/status   # 运行状态 JSON
```

---

## 数据目录

程序在**当前工作目录**（即 `/data` 等运行目录）生成以下文件，重启后若存在则跳过初次测速直接服务：

```
iptv_sources.m3u8    # 上次测速结果（M3U8）
iptv_sources.txt     # 上次测速结果（TXT）
sub_cache_*.txt      # 订阅源缓存
```

使用 `--dir /data/iptv_data` 可指定统一数据目录。

---

## 自定义订阅源

通过环境变量或命令行参数注入最多 20 个订阅地址：

```sh
export URL1="http://your-sub-address1.m3u"
export URL2="http://your-sub-address2.txt"
/data/iptv --port 3030
# 等价于 /data/iptv --url1 http://... --url2 http://...
```

---

## 常见问题

| 现象 | 解决 |
|---|---|
| `sh: /data/iptv: not found` | 确认盒子是 **32 位系统**，使用 armv7 版本；chmod 755 后重试 |
| `No such file or directory` 却文件存在 | 架构不匹配（如盒子为纯 64 位 Android 9），需使用 aarch64 版本 |
| 端口被占用 | 换端口：`--port 8080` |
| 需要联网测速但无外网 | 确认盒子 Wi-Fi/网线连通，DNS 正常 |
