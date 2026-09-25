# 哪吒 Agent Rust 重构版

本项目以 [nezhahq/agent](https://github.com/nezhahq/agent) 的 `5225d9b` 提交为兼容目标，使用原版哪吒 dashboard/service（测试提交 `9dab6a5`）进行联调。项目仍在开发，**尚未达到 100% 兼容，不应当作已验收的原版替代品**。

[GenshinMinecraft/nezha-agent-rs](https://github.com/GenshinMinecraft/nezha-agent-rs) 用于参考设计，但其 protobuf 与当前 service 不兼容，例如旧版 `ReportSystemState` 是单次 RPC，而当前接口是双向流。本项目从现行 Go Agent 的 `proto/nezha.proto` 生成 Rust 绑定。

## 分平台结构

`src/platform/mod.rs` 在编译时选择目标平台。Linux 的 systemd、GPU、hwmon、信号、文件系统、虚拟化识别、DNS、原生 ICMP 和传输实现在 `src/platform/linux.rs` 及 `src/platform/linux/`；Windows 的 SCM、Job Object、GPU、WMI、磁盘 API、文件系统和传输实现在 `src/platform/windows.rs` 及 `src/platform/windows/`。macOS 代码位于 `src/platform/macos.rs` 及 `src/platform/macos/`，暂不计入进度，也不发布 macOS 二进制。共享层只处理协议、配置、任务编排和监控；目标专属依赖放在 Cargo 对应目标依赖段。平台代码隔离完成度目前约为 **90%**，共享文件系统编排仍有目标条件分支。当前阶段优先完成 Linux，Windows 继续保持构建和共用代码回归。

## Linux / Windows 进度

进度只统计 Linux 和 Windows。验收基准是原版 dashboard 的实际交互或协议测试；仅能编译的代码不计完成。下表是功能验收分数，并非代码行数比例。

| 平台 | 验收进度 | 已验证的关键流程 |
| --- | ---: | --- |
| Linux / WSL | `[#########-] 91.25%` | 原版 dashboard 全流程、终端中配置重载、systemd 生命周期、GPU/温度模拟数据、锚定的 MCP 文件系统与传输、自定义 DNS 与原生 ICMP 联调 |
| Windows MSVC | `[#########-] 86.25%` | 本机测试、原版 dashboard 工作流、终端中配置重载、100 MiB MCP 传输、逐路径磁盘 API；SCM 生命周期尚缺提权验收 |
| 两端平均 | `[#########-] 88.75%` | 只取上述两端的算术平均；macOS 不参与 |

| 验收项目 | 满分 | 两端平均得分 | 主要依据或剩余问题 |
| --- | ---: | ---: | --- |
| 当前 protobuf RPC 与字段编号 | 8 | 8 | 现行 schema 和 wire tag 测试 |
| 配置、凭据和 TLS | 10 | 9 | YAML、`NZ_` 环境覆盖、UUID、四种鉴权头、可信及显式跳过校验的 TLS、dashboard 配置下发与重连；Linux 自定义 DNS 经原版 dashboard 联调；并发重载仍未完整验收 |
| 主机和状态上报 | 15 | 14 | WSL/Windows 与 Go 探针比对，NVIDIA/Intel 及 hwmon 模拟数据；Linux 虚拟化角色已按上游探针规则扩展，WSL 原版 dashboard 收到 `kvm` 和相同的 Virtual CPU 描述；真实 Linux GPU、可访问的 Windows 温度传感器仍未验收 |
| GeoIP | 5 | 4 | 本地 IPv4 和失败回退经原版 dashboard 验证；公网 IPv6 未验收 |
| HTTP、ICMP、TCP 和命令任务 | 15 | 14.25 | 原版 dashboard 服务监控、Linux 原生 ICMP 的 IPv4/IPv6 回环及 service 上报、HTTPS 证书信息、命令执行及超时清理；公网 ICMP、边界错误和告警副作用未覆盖 |
| 终端、NAT、旧文件管理器流 | 20 | 16.5 | 双平台实际 WebSocket/IOStream 流程、调整终端大小、上传过量与截断场景；更多半关闭、取消和故障注入未覆盖 |
| MCP 执行与文件系统 | 10 | 9.5 | 原版 dashboard `server.exec` 和 `fs.list/read/write/delete`；符号链接、junction 和目录锚定测试；Linux 最终文件打开增加 inode 比对与有界重试；并发子树变更仍未完整验收 |
| 远程配置、传输与自更新 | 10 | 9.5 | 原版 dashboard 配置下发、0 至 100 MiB 传输、哈希和更新；Linux 上传的提前 EOF、超额帧、停滞流取消清理已有故障注入测试；完整网络半关闭与更多更新失败路径未覆盖 |
| 服务管理与发布 | 5 | 3 | WSL systemd 完整生命周期；Windows SCM 需提权环境；发布工作流尚无实际发布验收 |
| Linux / Windows 对齐 | 2 | 1 | 双平台编译和 dashboard 流程已通过，部分真实硬件指标未验收 |
| **合计** | **100** | **88.75** | |

当前分数没有因为新加的交互式 `edit` 和镜像源选择而上调：两项还没有完成原版交互或真实镜像更新验收。

## 两端尚未完成的功能

以下区分实现缺口和验收缺口。Windows 虚拟化字段返回空值与当前上游 Go 依赖一致，不能误记为 Rust 独有的缺失功能。

| Linux：91.25% | 尚需完成 |
| --- | --- |
| 实现与行为 | hostfs 写入、传输和递归删除从 Linux 父目录 descriptor 锚点出发，读取有 inode 校验，MCP 写入和传输的临时文件在提交、清理前校验 inode，提交拒绝已有目标类型；检查与 rename/unlink 之间仍有并发替换窗口，上游 adversarial 套件尚未完整移植。终端、NAT、旧文件管理器、MCP 传输的停滞对端、取消及半关闭场景仍需扩展故障注入；HTTP/ICMP/TCP 错误细节和证书告警副作用仍需比对 |
| 已实现但未充分验收 | 新增 `edit` 的完整真人交互；Gitee/AtomGit 的真实 Rust 发布镜像；真实 AMD/Intel GPU 与温度传感器、公网 IPv6/ICMP、受限 ICMP 权限环境、并发配置重载与传输轮换、更新故障恢复、实际发布工作流 |

| Windows：86.25% | 尚需完成 |
| --- | --- |
| 实现与行为 | 并发修改父目录/子树期间的递归删除和最终目标替换；hostfs 模式位与 ACL 对齐；各流的停滞、取消和半关闭场景；HTTP/ICMP/TCP 错误细节 |
| 已实现但未充分验收 | 新增 `edit` 的完整真人交互；Gitee/AtomGit 的真实 Rust 发布镜像；提权后的 SCM 安装/启动/重启/停止/卸载、允许访问的 WMI 温度和更多 GPU 厂商、公网 IPv6、更新故障恢复、实际发布工作流 |

## 二进制大小

Release 配置启用 `opt-level=z`、fat LTO、单 codegen unit、符号剥离和 `panic=abort`。以下是 2026-09-24 本轮功能变更后在本机的实测值。

| 目标 | 文件 | 大小 |
| --- | --- | ---: |
| `x86_64-unknown-linux-gnu`（WSL） | `target/release/nezha-agent-rust` | 4,415,080 字节（4.21 MiB） |
| `x86_64-pc-windows-msvc` | `target/x86_64-pc-windows-msvc/release/nezha-agent-rust.exe` | 3,225,600 字节（3.08 MiB） |
| `powerpc-unknown-linux-musl`（静态链接） | `target/powerpc-unknown-linux-musl/release/nezha-agent-rust` | 4,194,336 字节（4.00 MiB） |

CI 所用工具链和链接器不同，产物大小可能变化。

OpenBSD ARMv5 的实验性静态构建使用 `scripts/build-openbsd-arm-lowisa.sh armv5` 和本地重建的 OpenBSD 7.7 ARMv5TE sysroot。当前产物已通过 ELF 静态链接、EABI5/soft-float、ARMv5TE 标签和链接检查，但尚未在 OpenBSD ARMv5 实机运行。其缺失的原子操作由单核系统上的全局 SWP 锁实现；若信号处理函数在同一线程持锁期间重入这些操作，可能自旋死锁。因此不能仅凭交叉编译和 ELF 检查认定运行兼容。

PowerPC 静态版使用 WSL、nightly Rust（含 `rust-src`）和 Zig 0.13 构建：

```sh
ZIG=/path/to/zig bash scripts/build-powerpc-musl.sh
```

该产物为 32 位大端 PowerPC ELF，无动态装载器和共享库依赖；已在 WSL 中用 QEMU 用户态执行 `--version` 和 `--help`。QEMU 使用宿主内核，尚不能证明它在目标 Fedora 16 / Linux 2.6.32 和 APM867xx 实机上可运行。

## 使用与配置

### 一键安装

`agent.sh` 从本项目的 `v2.1.0` Release 下载并校验静态二进制。每个架构优先使用对应的 `UPX-` 资产；Release 没有该资产，或压缩版下载后不能运行时，自动改用同架构原始 ELF。下载前会校验固定的 `SHA256SUMS.txt` 摘要，下载后还会检查 ELF 机器类型并执行 `--version`。

```sh
NZ_SERVER=dashboard.example:8008 NZ_CLIENT_SECRET=secret sh agent.sh
```

脚本支持 systemd、OpenWrt procd、OpenRC、SysV、cron，以及 BusyBox `init`。BusyBox `init` 若在 `rcS` 完成前不会处理 `respawn`，脚本会在 `rcS` 中安装一个幂等的后台启动钩子；它等待持久目录中的监护脚本出现，再由监护脚本负责重启 Agent。可用 `NZ_INIT_SYSTEM=busybox-rcs` 和 `NZ_BUSYBOX_RCS_PATH=/etc/init.d/rcS` 显式指定该模式。`sh agent.sh uninstall` 会移除服务、监护文件和它写入的 `rcS` 钩子。

```sh
cargo build
NZ_SERVER=dashboard.example:8008 NZ_CLIENT_SECRET=your-secret cargo run
cargo run -- edit --config /absolute/path/config.yml
```

默认配置路径是可执行文件旁的 `config.yml`；`--config` 指定其他 YAML 文件，`--server`、`--client-secret`、`--uuid` 可覆盖连接参数。缺少 UUID 时会生成并保存。`NZ_` 环境变量覆盖同名 YAML 字段。交互式 `edit` 可选择网卡、磁盘分区并编辑 DNS、UUID、GPU、温度和调试开关；选择列表留空表示监控全部，DNS 输入 `-` 表示清空。修改后需重启 Agent。

自更新从 `rust_update_manifest_url` 读取 Rust 发布清单；正式发布的二进制默认使用其 GitHub release 清单，本地构建若未设置构建时 `NZ_RUST_UPDATE_MANIFEST_URL` 则没有默认源。YAML 显式设为空串会禁用默认更新。配置 `rust_gitee_update_manifest_url` 或 `rust_atomgit_update_manifest_url` 后，原有 `use_gitee_to_upgrade` / `use_atomgit_to_upgrade` 开关会优先选择相应的 Rust 镜像清单；未配置镜像时会提示并退回其他已配置源，绝不下载原版 Go 二进制。镜像清单及资源应使用 HTTPS，或仅在本地测试时使用回环 HTTP。更新资源按 Rust 目标架构选择，限制为 256 MiB，并校验 SHA-256 和版本；`disable_auto_update`、`disable_force_update` 分别控制自动与 dashboard 强制更新。

Linux 服务命令：

```sh
sudo ./target/debug/nezha-agent-rust service install -c /absolute/path/config.yml
sudo ./target/debug/nezha-agent-rust service start -c /absolute/path/config.yml
sudo ./target/debug/nezha-agent-rust service stop -c /absolute/path/config.yml
sudo ./target/debug/nezha-agent-rust service uninstall -c /absolute/path/config.yml
```

Windows 使用同样的 `service install/start/restart/stop/uninstall` 子命令操作 SCM，需要相应权限。macOS 暂不纳入本项目的兼容进度。

## 原版 dashboard 联调

本地 WSL 使用 `upstream-service` 中的原版哪吒 dashboard 源码。它需要 Swagger 生成文件和嵌入式前端占位文件；先将 `tests/dashboard-tls-config.example.yaml` 复制为 `tests/dashboard-tls-config.yaml`，再生成临时测试证书。数据库、证书和测试二进制都在 Git 忽略范围内。

```sh
cp tests/dashboard-tls-config.example.yaml tests/dashboard-tls-config.yaml
openssl req -x509 -newkey rsa:2048 -nodes -keyout tests/ca.key -out tests/ca.crt -days 1 -subj /CN=NezhaLocalTestCA -addext basicConstraints=critical,CA:TRUE -addext keyUsage=critical,keyCertSign,cRLSign
openssl req -newkey rsa:2048 -nodes -keyout tests/localhost.key -out tests/localhost.csr -subj /CN=localhost -addext subjectAltName=DNS:localhost,IP:127.0.0.1 -addext basicConstraints=critical,CA:FALSE -addext extendedKeyUsage=serverAuth
openssl x509 -req -in tests/localhost.csr -CA tests/ca.crt -CAkey tests/ca.key -CAcreateserial -out tests/localhost.crt -days 1 -copy_extensions copy
cd upstream-service
mkdir -p cmd/dashboard/user-dist cmd/dashboard/admin-dist
touch cmd/dashboard/user-dist/placeholder.txt cmd/dashboard/admin-dist/placeholder.txt
go run github.com/swaggo/swag/cmd/swag@v1.16.6 init --pd -d cmd/dashboard -g main.go -o cmd/dashboard/docs --requiredByDefault
go build -o ../tests/nezha-dashboard ./cmd/dashboard
../tests/nezha-dashboard -c ../tests/dashboard-tls-config.yaml -db ../tests/dashboard.db
```

另开一个 WSL shell，在项目根目录运行：

```sh
SSL_CERT_FILE=tests/ca.crt NZ_TLS=true cargo run -- --server 127.0.0.1:18539 --client-secret integration-only-secret-0123456789 --uuid 00000000-0000-0000-0000-000000000326
```

`tests/probe_dashboard.py` 通过 `--uuid` 寻找 dashboard 内的 Agent，支持 `--check-monitor`、`--check-geoip`、`--check-monitors`、`--check-command`、`--check-nat`、`--check-terminal`、`--check-file-manager`、`--check-exec`、`--check-fs`、`--check-transfer`、`--check-transfer-large`、`--apply-report-delay` 等选项。Windows Agent 的文件检查需传入 Windows 临时目录 `--agent-temp-dir`。NAT 检查需要 `127.0.0.1:18540` 的临时 HTTP 服务。监控和 NAT 测试创建的 dashboard 记录在结束后删除。Windows 测试配置 `tests/windows-agent-config.example.yml` 使用显式的 `insecure_tls=true`，只用于本机测试。

此前已通过：原版 dashboard 上的双平台注册、状态与 GeoIP 上报、远程配置、HTTP/ICMP/TCP、旧命令、NAT、终端与尺寸调整、旧文件管理器、MCP 执行/文件系统、0 至 100 MiB 的 MCP 上传下载与哈希/令牌错误路径。WSL 原版 Go 主机探针和 Windows Go 主机探针用于比较字段；HTTPS 测试比较了证书颁发者和到期时间。WSL systemd 的系统/用户服务生命周期已通过，Windows SCM 因当前测试会话未提权尚未完成。真实硬件、并发文件变更和多种流故障仍按上表记录为缺口。

2026-09-25 最近一次 Linux 优先回归：WSL 74 项、Windows 42 项测试通过，另各 1 项 HTTPS 夹具测试跳过；两端严格 Clippy 和格式检查通过。原版 dashboard 接受 Linux Agent 使用本地自定义 DNS 的注册与 `ReportConfig`，NAT HTTP 转发、PTY 终端、旧文件管理器 WebSocket，以及 HTTP/ICMP/TCP 监控结果。Linux ICMP 改为目标专属的原生 socket 探测，不再依赖外部 `ping` 可执行文件；IPv4、IPv6 回环均通过 WSL 实测，原版 dashboard 收到新实现的 ICMP 成功上报。TCP 监控与 NAT 的 `host:port` 解析增加了双平台 IPv6 方括号及畸形地址测试。此前通过的 MCP `fs.*`、普通及 100 MiB MCP HTTP 传输继续作为回归基线。Linux MCP 写入和传输的临时文件名被替换时拒绝提交且不清理替换者；旧文件管理器列目录使用条目类型快照。Linux HTTP 监控已对齐上游请求头并按块丢弃响应体；终端按上游顺序选择 shell 和环境变量。并发子树变更、检查与提交间的竞态和完整网络半关闭仍未验收。

工作区里的 `upstream-agent`、`upstream-service`、`reference-rs` 是研究副本，根仓库已忽略它们。
