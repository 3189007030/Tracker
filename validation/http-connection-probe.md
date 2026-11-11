# HTTP 新连接关闭方向诊断

`http_connection_probe.rs` 是独立 Rust 标准库程序，仅向命令行指定的 IP 字面量 `:18080` 请求固定的 `GET /healthz`。它不使用 hash、query、peer ID，也不访问或验证 BT announce；结果不能作为 Tracker 吞吐或容量证据。

代码由主代理审阅并编译后才能执行；本文编写时没有编译、联网或做 A/B 实测。

## 编译与离线自检

在 Linux 仓库根目录，使用已安装的工具链：

```sh
rustc --edition=2024 -D warnings -O validation/http_connection_probe.rs -o /tmp/http-connection-probe
/tmp/http-connection-probe --self-test
```

Windows GNU 交叉构建示例，需系统已配置相应 target 和链接器：

```sh
rustc --edition=2024 -D warnings -O --target x86_64-pc-windows-gnu -C linker=x86_64-w64-mingw32-gcc-posix validation/http_connection_probe.rs -o /tmp/http-connection-probe.exe
```

自检预期输出 `{"self_test":true}` 且退出码为 0。它只验证 HTTP 状态行、Content-Length、重复长度、chunked、截断及尺寸等解析边界；不启动网络连接，也不替代两端运行验证。

## 执行约定

必须给出 IP 字面量和明确的拥有目标确认开关，并显式选择一种关闭模式。`--target` 只接受 18080 端口；IPv6 必须使用方括号。以下仅为本机示例：

```sh
/tmp/http-connection-probe --target 127.0.0.1:18080 --i-own-the-target --closure-mode server-close
/tmp/http-connection-probe --target 127.0.0.1:18080 --i-own-the-target --closure-mode client-close
```

`server-close` 发送 `Connection: close`，读取完整定长响应及 EOF；`client-close` 不请求服务端关闭，读取完整定长响应后调用 `shutdown(Both)`。两者分别尝试让服务端和客户端先关闭，但不能仅由 API 调用断定 TCP 线上 FIN 的实际方向；服务端提前关闭、未读数据和 RST 都可能影响结果。外部计数器或报文观察需另行验证，两个模式之间的差异也不能单独证明故障归因。

可选参数：`--rate 1..100`（默认 100 次/秒）、`--duration 1..120`（默认 120 秒）、`--threads 1..128`（默认 64）。全部线程共享全局单调时间槽，不足以按时发出的槽会跳过；大量连接同时停滞可能让实际速率远低于目标，应查看 `actualrps`。所有请求和响应合计最多 8 MiB；单连接从 connect 到读取、关闭的总限时为 5 秒，响应头最多 8 KiB、响应体最多 16 KiB，禁止分块或缺少长度。正常调度下总运行上限为请求时长加 8 秒；系统调度或内核故障无法提供硬实时保证。线程未在截止前结束时程序非零退出，不应采纳该次快照作为完整结果。

标准输出只含一行数字与固定枚举值：`attempted` 为已启动连接数，`completed` 为符合报文和该关闭模式的完整响应数，`http200` 为其中 HTTP 200 数，`successrate` 是 `http200 / attempted`，`actualrps` 是 `attempted / elapsed`。`connect_timeout` 只统计连接建立超时，`other_timeout` 统计建立后发送、读取或等待 EOF 超时，`io_error` 包括其他套接字错误和触及应用字节上限，`invalid_reply` 为报文或响应体不完整等格式错误。`bytes` 是实际由程序写入和读取的应用字节，不包含 TCP/IP、TLS 或链路开销。

应用预算耗尽或线程未完整退出时，结果仍提供脱敏计数，但退出码非零，不能称为完整测试。参数错误为退出码 2；网络错误本身保留在分类计数中，不自动判定诊断失败。输出不包含目标地址、原始报文或系统错误文本。测试必须由主代理纳入现有防火墙、带宽预算和互斥控制，并分开对两种模式采集；本程序不会修改这些控制。
