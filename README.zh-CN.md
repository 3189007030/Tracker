# Tracker Observatory

一个使用 Rust 实现的 BitTorrent Tracker 与观测项目。原生 Tracker 的 HTTP 和 UDP 共用分片内存蜂群；独立观测器检测公开 Tracker，并为双语网站发布只读数据。

工作区包含 `tracker-server`、`observatory`、`tracker-web` 和 `tracker-loadgen`，以及共享的协议、状态和数据模型库。前三者是独立服务，负载生成器是运维工具。

## 行为与边界

- 通告只返回本站同一资源中真实登记的节点，不向其他 Tracker 查询。Scrape 计数只反映本站；登记状态和完成计数驻留内存，会随相应蜂群或进程结束而清除。
- 观测器分别检测 IPv4 和 IPv6，区分 HTTP 状态、Tracker 协议结果和本地故障。未知保持未知，空列表不凑数。
- 节点握手和位图观测不请求文件分块；握手成功或节点声称拥有位图，并不能证明内容可以下载。
- 节点 IP 和端口会返回给通告同一资源的其他客户端。原生登记信息驻留内存，观测器保存私有测量证据；项目不承诺匿名或法律豁免。网站和公开 API 不展示单个样本、资源磁力链接、原始节点地址或普通客户端资源 hash。
- 两个样本池均有上限，实际数量取决于来源是否可用；来源故障时不虚构样本。论坛来源设置由部署者私下管理。
- 网站提供首页、列表、来源和声明，支持观测及文本/JSON/RSS 订阅。网站读取已发布的分钟级汇总，不因每次访问查询原生统计。

线上服务：[Tracker Observatory](https://tracker.augrains.com/)。[1.0.0](https://tracker.augrains.com/releases/v1.0.0/)、[1.0.1](https://tracker.augrains.com/releases/v1.0.1/) 和 [1.0.2](https://tracker.augrains.com/releases/v1.0.2/) 是历史部署材料，不能代表当前源码或性能。构建成功、短时测试或一次列表检测均不能证明持续承载能力或第三方收录。

## 构建与验证

通过 rustup 安装 Rust；工具链和组件由 `rust-toolchain.toml` 固定。内置 SQLite 构建还需要 C 编译器和常用构建工具。在仓库根目录执行：

```sh
cargo fmt --all -- --check
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --release --locked
```

测试使用固定样本和本机回环模拟，不要求访问线上 Tracker 或生产服务器。安装依赖需要访问配置的 Rust 包源。不要对第三方服务进行负载测试。

配置示例位于 `config/`。部署前请复制并按实际环境修改，检查监听地址和来源设置，并阅读[部署指南](docs/deployment.md)、[API](docs/api.md)和[发布打包](docs/release-packaging.md)。部分示例配置和网站链接目前仍绑定原站；使用其他域名前请阅读[开源审查](docs/open-source-review.md)。不要让两个观测器同时写入同一数据目录。

另见[贡献指南](CONTRIBUTING.md)、[安全说明](SECURITY.md)、[开源审查](docs/open-source-review.md)和[第三方许可声明](docs/third-party-notices.txt)。项目代码使用 [MIT 许可证](LICENSE)，内置组件另有各自的许可说明。
