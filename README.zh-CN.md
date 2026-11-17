# Tracker Observatory

Tracker Observatory 是一个 Rust 工作区，包含三个独立服务：原生 BitTorrent Tracker `tracker-server` 的 HTTP/UDP 通告共用分片内存蜂群；观测器 `observatory` 探测公开 Tracker、发布只读数据；`tracker-web` 提供双语网站和公开 API。另有运维负载生成工具 `tracker-loadgen` 和共享协议、状态、数据模型库。线上：[Tracker Observatory](<https://tracker.augrains.com/>)。

[English](<README.md>)

## 它做什么

- **原生 Tracker：** HTTP/UDP 通告与本站 Scrape 计数。
- **观测器：**探测公开 Tracker，保留私有测量证据。
- **网站与订阅：**首页、列表、来源、声明；原生统计读取已发布的分钟汇总，不因每次访问查询原生服务。六种订阅 `trackers.txt`、`trackers_best.txt`、`trackers_all.txt`、`trackers_udp.txt`、`trackers_http.txt`、`trackers_https.txt` 均有 txt/JSON/RSS 格式，按端点证据入选，每个服务一条，证据不足可为空。
- **公开 API：**端点状态、观测与汇总历史，见 [API 说明](<docs/api.md>)。

## 它不做什么 / 边界

- 通告只返回本站同资源的真实登记节点，不查询其他 Tracker。登记和完成计数驻留内存，随相应蜂群或进程结束而清除。
- 未知保持未知，空列表不凑数。两个样本池均有上限，数量取决于来源可用性；故障时不虚构公开样本。论坛来源配置私有；幽灵样本命中只作私有诊断，不影响订阅资格。
- 节点观测不请求文件分块；握手成功或声称拥有位图，不证明内容可下载。
- 节点 IP/端口会返回给通告同资源的其他客户端；不承诺匿名或法律豁免。网站与公开 API 不展示单个样本、磁力链接、原始节点地址或普通客户端资源 hash。
- 构建成功、短测或列表检测不证明持续承载能力或第三方收录。历史验证只适用所记部署、测试条件，不代表当前源码状态或性能。
- 不要对第三方做负载测试，也不要让两个观测器同时写同一数据目录。

## 公开数据的口径

- IPv4/IPv6 分开判定；HTTP 状态、Tracker 协议结果、本地故障互不等同。
- 地址族状态描述最近一次观测的新鲜度；24h 成功率只计历史已完成观测，未测、本地故障、未发现地址不入分母。“已过期”或“未测”可与 100% 同时出现。
- 同协议、端口、路径下，同源的域名与字面 IP 端点合并为一个规范端点；仅规范端点进入列表、订阅并接受探测。别名保留详情跳转和可选 `alias_of`。
- 可选 `target_ip` 是实际连接的公网 Tracker 服务器地址，不是节点地址；可区分 DNS 陈旧、地址变化与超时。

## 构建与验证

用 rustup 安装 Rust，[rust-toolchain.toml](<rust-toolchain.toml>) 固定工具链和组件；内置 SQLite 构建还需 C 编译器和常用构建工具。在仓库根目录执行：

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --release --locked
```

测试用固定样本和本机回环模拟，不依赖线上 Tracker 或生产主机；安装依赖须能访问配置的 Rust 包源。

## 配置与部署

按实际环境复制、修改[配置示例](<config/>)，检查监听地址与来源设置。部分示例键和网站链接绑定原站；换用域名前读[部署指南](<docs/deployment.md>)、[API](<docs/api.md>)、[发布打包](<docs/release-packaging.md>)和[开源审查](<docs/open-source-review.md>)。

## 文档导航

从[文档索引](<docs/README.md>)开始。

- [CHANGELOG.md](<CHANGELOG.md>)：源码改了什么。
- [各版本记录](<docs/releases/README.md>)：每个版本的验证与部署证据。
- [发布流程](<docs/release-process.md>)：如何记录与发布版本。
- [容量研究](<docs/capacity/>)：方法、实测边界与请求量 / 状态容量。

部署实际运行的版本，只以它的 `/healthz` 为准。

## 贡献、安全与许可

请阅读[贡献指南](<CONTRIBUTING.md>)、[安全说明](<SECURITY.md>)和[第三方许可声明](<docs/third-party-notices.txt>)。项目代码使用 [MIT 许可证](<LICENSE>)，内置组件另有各自的许可说明。
