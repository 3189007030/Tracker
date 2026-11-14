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
- 列表页支持边输入边筛选：短暂停顿后向同源地址请求服务端渲染的结果片段，只替换结果区域；没有 JavaScript 时同一个 GET 表单照常提交并整页渲染。分页提供带省略号的页码链接、上一页/下一页、跳转页码输入框和每页 25/50/100 条选择；`q`、`protocol`、`page`、`limit` 一起保留在 URL 中，筛选或翻页通过 fetch 和 History API 更新，不整页刷新。
- 同一 origin 的重复端点——同一 scheme、端口和路径下的域名地址与字面 IP 地址——归并为一个规范端点：只发布并探测规范端点，别名不进入列表和订阅，详情页保留跳转，也不再探测，避免同一 Tracker 来源收到重复通告。公开端点 JSON 新增可选 `alias_of`。
- 详情页的观测失败原因使用本地化说法，适用时带上状态信息（如 `HTTP 429 · 频率限制`、`超时`、`DNS 解析失败`、`HTTP 502 · 网关错误`），不再直接显示内部类别字符串；并展示探测实际连接的解析后公网地址（观测 JSON 中为可选 `target_ip`）。该地址是 Tracker 服务器地址，不是节点地址，可区分 DNS 陈旧或地址变化与单纯超时。结果为未发现地址的观测在“有效 Tracker 回复”和“原因”列显示破折号（—），不再显示“未知”或内部代码。
- 地址族状态（成功/已过期/未知 / 未测/未发现地址）描述最近一次观测的新鲜度；24h 成功率是历史统计，只计已完成观测，未测、本地故障和未发现地址都不进入分母。页面会明确说明这一点以及成功率背后的成功/已完成次数（例如 12/12），因此已过期或未测的地址族可以同时显示 100%。

线上服务：[Tracker Observatory](https://tracker.augrains.com/)。[1.0.0](https://tracker.augrains.com/releases/v1.0.0/)、[1.0.1](https://tracker.augrains.com/releases/v1.0.1/) 和 [1.0.2](https://tracker.augrains.com/releases/v1.0.2/) 是历史部署材料，不能代表当前源码或性能。1.2.x 的列表、详情和 schema 改动存在于当前源码，验证与部署回执另见 [1.2.1 验证](docs/validation-1.2.1.md) 与 [1.2.0 验证](docs/validation-1.2.0.md)。构建成功、短时测试或一次列表检测均不能证明持续承载能力或第三方收录。

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

原生通告优化与实测边界：[1.0.4 验证](docs/validation-1.0.4.md)、[参考项目研究](docs/optimization-1.0.4.md)及[日请求量与状态容量](docs/billion-request-envelope.md)。
