# sb4a

独立的 sing-box Root 模块。直接读取**标准 `config.json`**，不需要配套 App。

Android 12+ / arm64-v8a。作者：Lin。GPL-3.0-or-later。

---

## 这是什么

一个 Magisk / KernelSU / APatch 模块。装好之后，你把 sing-box 配置交给它，
它就负责把核心跑起来、接管流量、并在开机时自动恢复。

和"App + 模块"那类双端方案的区别：**没有 App**。APK 的价值在于 GUI 和
订阅管理，但那套东西让配置格式、安装流程、更新逻辑全部耦合在一起，改一处
要动两处。这里只做一件事——把一份标准配置稳定地跑起来。

---

## 安装

在 Magisk / KernelSU / APatch 里刷 `sb4a-module-arm64.zip`。

首次安装**不接管网络**，模块启动后保持空闲，直到你提交配置。这是故意的：
装完就断网是最糟糕的体验。

卸载时核心会在 `remove` 标记出现后立即停止，`uninstall.sh` 在下次开机执行，
用户数据 `/data/adb/sb4a` 保留。

---

## 快速开始

```sh
su

# 1. 从桌面端配置转换（可选，见下一节）
sb4a config convert < config.json > portable.json

# 2. 导入
sb4a config import < portable.json

# 3. 启动
sb4a start

# 4. 开机自启（可选）
sb4a autostart on
```

`config import` 接受**裸的** sing-box 配置，自动补模块需要的信封字段。
也接受完整信封（带 `schemaVersion`/`profileId` 的快照），两种都认。

---

## 配置从哪来

这是最实际的问题。按省事程度排序：

### 1. 从 GUI.for.SingBox 借运行时配置（推荐）

GUI for SingBox 在运行时会生成一份展开后的完整配置：

```
~/Library/Application Support/GUI.for.SingBox/sing-box/config.json
```

这是真正在用的那份，不是带订阅占位符的模板。但它有 6 类桌面专有字段，
直接拷到手机会失败，用 `config convert` 转一次：

```sh
sb4a config convert < config.json > portable.json
```

转换器做的事（每一项都会在 stderr 报告）：

| 动作 | 原因 |
|---|---|
| 删除 `$schema`、`http_clients` | sing-box 不认这些字段 |
| 删除 `experimental`（`clash_api` / `cache_file`） | 桌面绑 127.0.0.1:20123；`cache.db` 路径在只读的模块目录里 |
| TUN 改为 `172.19.0.1/30`、`mtu:1500`、`strict_route:false`、`stack:system` | 原配置带 `fc00::/18` IPv6 段，部分运营商下会直接断网；`strict_route` 会和 Android 自己的路由规则打架 |
| 删除 `route.find_process` | 需要平台级连接归属查询，裸模块没有。留着会让进程类规则静默失效 |
| `geosite_cn.srs` / `.../geo/geosite/cn.json` → `geosite:cn` | 桌面规则集文件不在手机上 |
| 剔除无法映射的本地规则集，并清掉引用它的规则 | 比如 GUI 的 `direct.json`，里面有 `process_path: /opt/homebrew/bin/cloudflared`，无法移植 |

无法映射的 **remote** 规则集会保留——核心自己会下载。

实测：一份 123KB / 263 个 outbound / 21 个 rule_set 的真实配置，
转换后 19 个规则集改成别名、2 个丢弃、TUN 建立、出口 IP 正确改变。

### 2. 自己写

最小可用配置大概 30 行：

```json
{
  "log": { "level": "warn" },
  "inbounds": [{
    "type": "tun", "tag": "tun-in",
    "address": ["172.19.0.1/30"], "mtu": 1500,
    "auto_route": true, "strict_route": false, "stack": "system"
  }],
  "outbounds": [
    { "type": "hysteria2", "tag": "proxy",
      "server": "example.com", "server_port": 443,
      "password": "...",
      "tls": { "enabled": true, "server_name": "www.cloudflare.com" } },
    { "type": "direct", "tag": "direct" }
  ],
  "route": { "final": "proxy", "auto_detect_interface": true }
}
```

### 3. Clash 订阅转换

**不推荐**。Clash 订阅里常见的 `ssr`、`mieru` 协议 sing-box 本身不实现，
转换器也无法弥补。先确认你的节点协议是否被支持。

---

## 命令

| 命令 | 作用 |
|---|---|
| `status` | JSON 状态：phase、profile、流量计数 |
| `start` / `stop` / `restart` | 核心生命周期 |
| `config convert < c.json` | 桌面配置转可移植形态，输出到 stdout |
| `config import < c.json` | 导入裸配置或快照 |
| `config validate < c.json` | 校验（含 sing-box schema，无副作用） |
| `config apply < c.json` | 提交配置（与 import 的区别：不包装，要求已是快照或裸配置） |
| `config rollback` | 回滚到上一份配置 |
| `autostart on` / `off` | 开机是否自动启动 |
| `ruleset list` | 列出已缓存的规则集 |
| `ruleset download <family> <code> [url]` | 下载单个规则集 |
| `ruleset update` | 重新下载当前配置引用的全部规则集 |
| `logs` | 最近状态事件 |
| `module` | 模块信息 |

所有命令输出 JSON 信封（`config convert` 除外，它的 stdout 是配置本身，
诊断走 stderr，方便管道）。

### 常用组合

```sh
# 看当前状态
sb4a status | jq .state.phase

# 改配置后重启
sb4a config import < new.json && sb4a restart

# 配置出错就回滚
sb4a config rollback

# 规则集过期了手动刷
sb4a ruleset update
```

---

## 规则集（geosite / geoip / asn）

模块**不内置**规则集。配置里写 `path: "geosite:cn"`，首次启动时自动下载
对应的 `.srs` 并改写为绝对路径。

支持的写法：

| 写法 | 下载地址 |
|---|---|
| `geosite:cn` | `.../geo/geosite/cn.srs` |
| `geoip:cn` | `.../geo/geoip/cn.srs` |
| `asn:AS13335` | `.../asn/AS13335.srs` |
| `geosite-cn` / `asn-AS13335` | 同上，文件名的形式 |

默认镜像 `https://testingcf.jsdelivr.net/gh/MetaCubeX/meta-rules-dat@sing`。
可选数量：geosite 500 个、geoip 260 个、asn 按编号。

### 自定义来源

单条下载：

```sh
sb4a ruleset download geosite cn
sb4a ruleset download geosite cn https://cdn.jsdelivr.net/gh/.../cn.srs
sb4a ruleset download asn AS13335 https://my-mirror/asn/AS13335.srs
```

写进配置（`type: "local"` 的 rule_set 支持 `url` 字段）：

```json
{ "type": "local", "tag": "cn", "path": "geosite:cn",
  "url": "https://my-mirror/srs/cn.srs" }
```

模块用它下载后会**移除 `url` 字段**再交给 sing-box（sing-box 不认这个字段）。
无论是否命中缓存都会移除。

### 安全约束

只接受 `https://`。`http://`、`file://`、带 `@` 的 URL 一律拒绝，返回
`ruleset_url_invalid`。单文件上限 8 MiB。理由是规则集 URL 可能来自别处
拷来的配置，`file://` 能读本地文件，`http://` 可被中间人替换。

下载依赖系统自带的 `/system/bin/curl`（Android 12+ 均有）。

---

## 日志

| 文件 | 内容 | 上限 |
|---|---|---|
| `/data/adb/sb4a/runtime/core.log` | 核心完整输出（stderr） | 4 MiB，保留 2 份（`.1` `.2`） |
| `/data/adb/sb4a/runtime/check.log` | 上次 `--check` 失败原因 | 每次覆盖 |
| `/data/adb/sb4a/events.json` | 状态事件 | 最近 40 条 |

`core.log` 在每次启动核心前按大小轮转：超 4 MiB 就改名 `.1`，旧的 `.1`
变 `.2`，`.2` 丢弃。磁盘占用最多约 12 MiB，不会无限增长。

不直接 truncate 是为了让上一次崩溃的日志活到下次启动之后。

日常建议 `"log": {"level": "warn"}`。`info` 或 `debug` 增长很快——
实测 `info` 下几分钟就几十 KB，一天能到几十 MB。

---

## 路径

| 路径 | 用途 |
|---|---|
| `/data/adb/modules/sb4a` | 代码（`bin/sb4a`、`bin/rootbox`、三个脚本） |
| `/data/adb/sb4a` | 数据，0700，文件 0600 |
| `/data/adb/sb4a/generations/<rev>/run/config.json` | 当前生效配置 |
| `/data/adb/sb4a/generations/<rev>/assets/srs/` | 规则集缓存 |
| `/data/adb/sb4a/state.json` | 状态（可安全删除，会重建） |

---

## 架构

```
sb4a (Rust CLI)
  ├── config transaction  stage → check → 原子切换 → prune
  ├── supervisor          派生独立进程，轮询 ready，解析 stats
  └── ruleset             别名解析 / 下载 / 改写
        │
        └── exec rootbox (Go, sing-box 集成层)
              ├── box.New + Start
              ├── 平台接口（TUN 栈、接口监控）
              └── stdout: WANBOX_STATS:{...}
```

**零 FFI**。Rust 和 Go 之间只有进程边界：`exec rootbox` + 文件信号
（`runtime/stop`、`runtime/ready`）+ stdout 统计协议。这样任何一侧崩溃
都不会把另一侧带下去，也不需要维护 cgo 绑定。

Go 侧只保留 sing-box 集成层（约 260 行），其余全部在 Rust。

### 为什么这样分

- sing-box 是 Go 写的，TUN 栈和平台接口绕不开 Go
- supervisor、配置事务、状态机用 Rust 写，崩溃面小、内存可控
- 两边通过文件 + 标准输出通信，协议简单到可以手写测试替身

---

## 生成

```sh
# Rust CLI（交叉编译到 Android arm64）
bash scripts/build-android.sh

# Go 核心
bash scripts/build-rootbox.sh

# 打包
bash scripts/pack-module.sh
```

产物：`target/sb4a-module-arm64.zip`（约 15 MB）

### 关于交叉编译

`rootbox` 用 `GOOS=android GOARCH=arm64 CGO_ENABLED=0`，产物是链接 bionic
的 PIE（`PT_INTERP = /system/bin/linker64`），但**没有任何 bionic 库依赖**
——纯 Go 静态实现。

`sb4a` 用 Rust 的 `aarch64-linux-android` 目标，链接器是 Rust 自带的
`rust-lld` + 官方 NDK 的 sysroot。NDK 里只有 `linux-x86_64` 的 clang，
但我们**从不执行**它，只读 `sysroot/` 里的库文件——所以 arm64 主机也能
完成构建，不需要 x86_64 机器。

### 编译开关

`build-rootbox.sh` 的 `TAGS` 决定编进哪些协议：

```
with_quic,with_utls,with_wireguard,with_gvisor
```

- `with_quic` → hysteria / hysteria2 / tuic
- `with_utls` → REALITY
- `with_wireguard` → wireguard endpoint
- `with_gvisor` → `stack: gvisor` / `mixed`

### 测试

```sh
bash scripts/test-docker.sh        # fmt + clippy + 单测 + Android 目标检查
bash scripts/test-integration.sh   # 端到端（假 core）
```

两者都在隔离容器里跑（`--network none --cap-drop ALL`），不碰宿主机网络。

---

## 状态与错误码

`phase` 取值：`stopped` / `starting` / `connected` / `failed` / `cleanup_required` / `disabled`。

核心会重试 3 次（1/2/4 秒退避）。**正常退出（exit 0）不重试**——
只有非零退出码才算崩溃。

`status` 会校验记录里的 PID 是否真的还活着。被 `SIGKILL` 的 supervisor
会留下陈旧的 `connected`，`status` 读到这样的记录会当场修正并写回磁盘。

错误码共 46 个，都是稳定的短字符串（如 `config_invalid`、
`ruleset_fetch_failed`、`revision_conflict`）。错误码绝不携带原始文本或
凭据——核心的真实报错在 `runtime/check.log` 和 `runtime/core.log` 里。

---

## 能力与限制

### 支持

- TUN 栈：`system`、`gvisor`、`mixed`（三者均已真机验证）
- 协议：direct、block、selector、urltest、socks、http、shadowsocks、
  trojan、vless、vmess、anytls、wireguard、hysteria/hysteria2/tuic、
  REALITY
- 规则集：geosite / geoip / asn，按需下载，支持自定义 URL
- 配置回滚
- 开机自启（真实重启验证过）

### 不支持

| 项 | 原因 |
|---|---|
| `ssr` / `mieru` | sing-box 本身不实现，需要插件机制 |
| `config convert` 反向（模块 → 桌面） | 没做，也没必要 |
| Clash YAML 直转 | 没做，需要独立转换器 |

### 已验证

真机（小米 13 / Android 17 / arm64）：

- 装包（APatch `apd module install`）
- 配置事务 validate / apply / rollback
- 核心托管 start / stop / restart
- 三种 TUN 栈均连通，`curl` 204
- 规则集按需下载与别名改写
- 真实 hysteria2 节点代理，出口 IP 正确
- 统计上报（5 秒周期）
- 卸载停核心（`remove` 标记）
- supervisor 被 `SIGKILL` 后 core 跟着退出（`parent_death_signal`）
- 真实重启后自动恢复（`service.sh` → `connected`）

### 未验证

- 长时间运行稳定性（> 数小时）
- 在 Magisk / KernelSU 上的安装（只在 APatch 上实测过，三者规范兼容）
- 多设备 / 其他 Android 版本

---

## 配置示例

完整可用的例子在 `examples/` 下（如果有）。核心思路：

1. `inbounds` 只要一个 tun
2. `outbounds` 至少一个 direct + 一个代理
3. `route.final` 指向代理
4. 国内直连用 `rule_set: "geosite:cn"` + `outbound: "direct"`

---

## 许可

GPL-3.0-or-later。见 `LICENSE`。

`bin/rootbox` 由 sing-box 构建，因此包含 sing-box 及其依赖，
许可证全文见 `THIRD-PARTY-NOTICES.md`。
