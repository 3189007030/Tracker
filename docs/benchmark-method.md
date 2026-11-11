# Tracker 容量与协议验证方法

`tracker-loadgen` 仅测试使用者拥有或明确运营的 Tracker，运行网络测试必须传入 `--i-own-the-target`。工具只连接明确指定的目标，不执行地址扫描。默认负载为 10 次/秒、30 秒、8 个并发和 64 MiB 应用字节预算；上限为 18 GiB。

用户已决定本次仅采用已完成的短时测试，取消一小时公网复测、24 小时观察和其他新增压力测试；相关长时能力保持未验证。下述方法与历史结果用于解释现有证据，不表示继续运行测试。

首先对隔离实例执行真实协议验证，再进行压力测试。公开实例不得预填合成节点；状态规模测试使用无网络的独立进程，清理其全部状态后再发布公开服务。

```text
tracker-loadgen --mode verify --protocol mixed --http-url https://owned.example/announce --udp-target owned.example:6969 --family ipv4 --expected-client-ip "$TRACKER_LOADGEN_PUBLIC_IP" --i-own-the-target
tracker-loadgen --mode verify --protocol https --http-url https://owned.example/announce --family-a ipv4 --family-b ipv6 --expected-peer-a-ip "$TRACKER_LOADGEN_IPV4" --expected-peer-b-ip "$TRACKER_LOADGEN_IPV6" --verify-delay-secs 1800 --i-own-the-target
tracker-loadgen --mode pressure --protocol udp --udp-target owned.example:6969 --family ipv4 --rate 100 --duration 600 --concurrency 32 --unique-hashes 100000 --peers-per-hash 2 --numwant 50 --max-bytes 1073741824 --progress --i-own-the-target
tracker-loadgen --mode pressure --protocol https --http-url https://owned.example/announce --family ipv6 --rate 100 --duration 600 --concurrency 32 --new-connections --i-own-the-target
tracker-loadgen --mode pressure --protocol https --http-url https://owned.example/announce --family ipv4 --resolve-ip "$TRACKER_LOADGEN_S1_IPV4" --rate 100 --duration 60 --i-own-the-target
tracker-loadgen --mode pressure --protocol udp --operation connect --udp-target owned.example:6969 --family ipv4 --rate 100 --duration 60 --i-own-the-target
tracker-loadgen --mode pressure --protocol udp --operation scrape --udp-target owned.example:6969 --family ipv4 --rate 100 --duration 60 --i-own-the-target
tracker-loadgen --state-bench --state-count 100000 --unique-hashes 100000 --state-memory-mib 768
tracker-loadgen --state-bench --state-workload scatter --state-count 100000 --unique-hashes 100 --state-memory-mib 768
tracker-loadgen --state-bench --state-workload churn --state-count 100000 --unique-hashes 1000 --state-memory-mib 768
```

`--operation mixed` 的请求构成为 80% announce、20% scrape；`--protocol mixed` 使用 HTTP 与 UDP 客户端，报告其发送量及 UDP connect 包数。新建连接测试加 `--new-connections`；省略时允许 HTTP 连接复用。每一项应独立报告，不要将 UDP connect 包、一次 announce、HTTP 建连或 TLS 握手当作相同工作量。

`--new-connections` 禁用 HTTP 空闲连接池并要求关闭连接，不显式禁用 TLS 会话恢复。本轮未统计完整握手与恢复握手的比例；HTTPS 新连接结果不能写成每次都执行完整 TLS 握手的容量。

并发窗口默认仍为 8。纯 UDP 压力模式最多 4096，其他模式最多 1024；较大的窗口用于外网 RTT 较高时保持足够在途请求，依然受总速率、时长和字节预算限制。窗口不足会造成发流率低于目标，不能据此认定服务端容量。实际连接、端口、内存和 CPU 仍可能使发流端受限，必须记录每轮使用的窗口大小。

验证模式使用随机资源、两个自身登记端口，检查零节点请求、跨入口双向互见、请求者不返回自身、scrape 数量 2→1→0、stopped 删除和新资源无无关节点；解析器同时拒绝重复节点、非公网节点或超过 numwant 的节点数。仅隔离测试需要 loopback/私网节点时显式加 `--allow-private-test`，报告会记录这一放宽。必须显式提供外部看到的客户端 IP：两者相同时用 `--expected-client-ip`；不同线路分别使用 `--expected-peer-a-ip` 和 `--expected-peer-b-ip`。可选 `--source-a-ip`、`--source-b-ip` 将两个客户端绑定本机已配置的不同出口地址；网络压力模式使用 `--source-ip`。NAT 出口地址与本地绑定地址需分别填写，工具比较完整公网 IP:端口，不能确定真实出口时不会报告互见通过。分别设置 `--family-a ipv4 --family-b ipv6` 可测双栈交叉互见，两个客户端必须都使用 HTTP(S)，因为 UDP 回复只包含该报文的地址族。

`--verify-delay-secs` 可设置 0–1800 秒真实等待，不受压力模式 `--duration` 控制。等待时两个原始客户端均不发送刷新；等待结束先由第三个自身端点观察原始两者，再验证双方互见，避免先重新登记一方掩盖过期。开始和结束分别输出不含 hash/IP 的阶段和时间，报告单独给出 `real_wait_seconds`。Ctrl-C 中断等待会执行有界的停止清理，异常路径也会为停止通知预留预算并尽力清理；通知失败时结果不算通过。节点端口只用于本次测试，不代表真的提供文件数据。

输出为逐行 JSON。`attempted` 统计已发起逻辑操作，`completed` 是已得到成功或失败结果的操作，进行中的操作暂不计入；`protocol_success_rate` 使用成功数 / 已完成数，样本数为零时为 null。`protocol_success` 要求解析出正确的 Tracker 协议回复；HTTP 200 中的 bencode 失败不会计为成功。UDP 必须匹配 action 和 transaction ID。状态码、协议错误、超时、传输错误分别统计。`latency_p*` 包括成功及失败操作，`success_latency_p*` 只包含协议成功操作；二者都是发流端端到端时间，不能当作源站纯处理时间。延迟使用固定 1 ms 桶，p50/p95/p99 为桶上界；60000 表示 60 秒及以上。`generator_cpu_cores` 是发流进程从测量开始消耗的 CPU 核数平均值（平台无法读取时为 null）。`possible_generator_or_target_saturation` 仅提示发流率未达目标，需结合客户端 CPU、网络、目标端资源判断瓶颈。中间进度是并发快照，最终统计在所有已发起操作结束后汇总。

字节硬预算覆盖 HTTP 请求目标与响应正文、UDP 应用数据；它不包含 HTTP 头、TLS、IP/TCP、重传或运行库已缓冲但未读的数据。`tx_application_mbps` 和 `rx_application_mbps` 分别是上述已计入字节 / 实际运行时间得到的平均 Mbps，并非网卡带宽。非 2xx 收到响应头即计为对应 HTTP 错误，不等待错误页面正文。并发请求开始前预留最大响应空间，预算不足时停止新增请求；高并发且预算很小时，在途预约可能使测试提前停止并留下未使用预算。S1 上须另外读取网卡收发计数，累计主动测试流量接近 18 GiB 时立即停止，以保留 20 GiB 公网总预算余量。

UDP 客户端最多缓存 connection ID 55 秒；正常业务拒绝不会触发重握手。疑似旧 cookie 的请求超时后，每套接字最多每分钟主动刷新一次；连接建立连续失败使用有界指数退避。重复旧 transaction ID 的数据报单独计数并最多丢弃四个，避免迟到包被误报为当前成功。

已完成的短时阶梯记录实际成功率、p95/p99、客户端及服务端 CPU、RSS、交换、网卡字节和 PPS、队列、过载响应、清理耗时及互见结果。一小时复测和 24 小时观察已取消，低负载观察也不能表述为满载长稳。

新连接异常还可以用独立的健康请求做 A/B/A 对照，但要将它与 Tracker announce 压力分开报告。[现有 IPv4 健康路径对照](../benchmarks/http-connection-diagnostics.json) 中，服务器关闭、客户端关闭、再次服务器关闭三轮的 HTTP 200 数分别为 9,540 / 10,292、11,710 / 11,875、9,461 / 10,239；对应比例约为 92.6934%、98.6105%、92.4016%。这说明对照中存在可重复的差异，不证明故障根因；记录未捕获 FIN 方向，SYN/RST 包含重传且不能等同唯一连接数。[本地源地址绑定短测](../benchmarks/ipv4-source-binding.json) 首次严格验证失败、重试通过，其后 12,000 次操作中 10,759 次成功、1,241 次超时；短测及源地址绑定均不能独立证明完整路径或十分钟容量。

[IPv4 代理出口对照](../benchmarks/ipv4-egress-comparison.json)先通过一次严格源地址验证，随后 HTTP announce 在目标 100 次/秒下完成 59,438 次操作，其中 28,706 次成功、30,732 次超时，成功率 48.2957%，未达到稳定门槛。源地址验证只证明该次检查的观察，不能覆盖整段压力运行或唯一确定原因；测试后路由已恢复。

HTTP/HTTPS 可用 `--resolve-ip` 指定连接 IP；URL 中的主机名、HTTP Host、TLS SNI 和证书验证保持不变，UDP 目标仍通过 `--udp-target` 指定。双 HTTP 跨族验证可分别使用 `--resolve-a-ip`、`--resolve-b-ip`；地址必须匹配客户端地址族。输出只标记覆盖是否启用，不输出 IP。该选项只覆盖域名解析，不修改系统路由，也不能排除本机透明代理或网络层转发；需独立核实实际出口并与经代理结果分开记录。

`--state-bench` 在单进程使用虚构私网地址和逻辑时钟，只测状态准入、600/1800 秒互见及 256 条预算的增量过期；输出计数、状态估算、Linux RSS 与清理时长。它不发送网络请求，不是 HTTP/UDP 吞吐或 S1 实际网络容量。达到引擎内存或条目上限会提前停止并显示原因。

默认 `baseline` 的所有登记发生在逻辑时间 1 秒；`--state-workload scatter` 在逻辑时间 1 至 1800 秒间单调分散登记，同一 hash 轮流加入节点，并在各蜂群交替使用 IPv4、IPv6 与做种/下载角色。600/1800 秒检查从最后一次登记尝试之后计算；清理时间在最后登记尝试后的最长 TTL 之后。JSON 分别输出最后成功登记和最后尝试时间；容量拒绝后的检查以尝试时间为起点，避免逻辑时钟倒退。时间均为模拟，不是真实等待。

可选 `--state-workload churn` 按 2、3、4、10、100、1000、4000 节点轮流顺序膨胀蜂群，更新部分节点角色，再停止除一个节点外的其余节点，从而跨蜂群重用引擎槽位；地址交替使用 IPv4/IPv6。逻辑时钟依据操作进度线性推进（公式见 JSON），使登记时间和过期时间分散，但不代表现实等待。`--state-count` 在此模式限制累计首次准入，不等于同时在线节点数；`--unique-hashes` 限制依次处理的蜂群数，预算不够时最后一个蜂群可能只有部分节点。`full_size_swarms`、成功计数、停止原因和清理前后 `engine_stats` 共同说明实际覆盖。每 1000 次操作采样当前 RSS；`admission_phase_vmhwm_kib` 是最后一次膨胀结束前的进程累计 VmHWM，`full_run_vmhwm_kib` 在整个清理流程结束后采集，`rss_after_cleanup_kib` 是清理后当前 RSS。Linux 以外可能为 null；VmHWM 为进程生命周期高水位，采样 RSS 可能漏掉短时峰值，引擎 `estimated_bytes` 是准入估算，即使为零也不代表真实内存归零。基线模式保留原有 `peak_rss_kib` 字段，并额外报告全流程 VmHWM 和清理后 RSS。

Churn 的不同随机 hash 按引擎内部哈希概率分散到 shard，外部工具不保证连续蜂群位于同一 shard；确定性 shard 复用由引擎内部回归测试覆盖。

当前候选的[密集状态试验](../benchmarks/state-1024-dense-candidate-d427.json)在 1024 MiB 准入估算预算下接纳 200 万节点 / 2000 蜂群，准入阶段与全流程 VmHWM 分别约 455.84 和 463.12 MiB；清理后 RSS 约 120.74 MiB。[单热点试验](../benchmarks/state-1024-hot-candidate-d427.json)接纳 1 万节点，全流程 VmHWM 约 5.82 MiB。这些是逻辑时钟下的独立状态试验，不能外推到其他分布或网络吞吐。

另一次[200 万节点网络试验](../benchmarks/full-state-network.json)以隔离 IPv4 UDP 回环和私网测试模式运行 600.022 秒，9,038,216 次回复均有效；目标 16,000 次/秒，实际约 15,063.13 次/秒，低于目标的 95%，不计为该目标稳定档通过。满状态平台仅持续约 468.609 秒，采样 RSS 峰值 460,068 KiB（约 449.29 MiB），未观察到 OOM 或重启。合成节点并非独立设备，发流与服务共享两核；该结果不代表公网容量或服务器最大上限。

## S1 隔离测试顺序

| 阶段 | 独立实例与工作负载 | 用途和边界 |
| --- | --- | --- |
| 协议前置 | 在隔离端口分别测 IPv4/IPv6、HTTP/UDP 与跨入口互见；执行真实 600 秒和 1800 秒延迟验证 | 要求正确的客户端 IP:端口、空 hash、stopped、scrape 2→1→0；不得把逻辑时钟结果写成真实等待通过 |
| 状态规模 | `--state-bench` 依次设 1万、10万、30万、100万、200万节点；各用同数量不同 hash，再测 1 万节点 / 1 hash 热点 | 记录实际接纳数、拒绝原因、峰值 RSS、增量清理耗时；按配置内存上限提前停止 |
| 本机环回 | 另启隔离服务监听 127.0.0.1 / ::1 和独立 HTTP/UDP 端口；服务端与客户端均显式启用私网测试 | 分测 UDP connect/announce/scrape、HTTP keep-alive/新建连接及混合；发流和服务共享 S1 两核，只作为同机对照 |
| Windows→S1 | Windows 对隔离端口发流，HTTPS 使用原始域名和可信证书；需要时使用 `--resolve-ip` | 原有 TUN 路径经 S2；后来为 S1 两个目的地址设置有期限的物理接口路由并独立核验出口。两种路径分别记录，`--resolve-ip` 或路径标签本身不证明直连 |
| 长稳 | 原一小时公网复测和 24 小时观察已按用户要求取消 | 未验证；短时测试不能替代长时间持续负载 |

已完成的矩阵包括低速预检和每秒 100、200、400、800 等短时档位。一小时复测现已取消。不同负载的 `/stats.json`、进程和网卡计数应按具体阶段解读；重置隔离实例只清除其合成状态，不代表生产实例变化或优化。保留配置、版本、编译参数、具体链路、状态量、客户端和服务端 CPU、内存、PPS、网卡字节增量、成功率、业务拒绝、429、UDP 丢包和发流速率。

旧内部阶段曾发生监控中断，不能记为已完成；[重试记录](../benchmarks/benchmark-controller-retry.json)显示，后续仅处置隔离测试状态，首次重置后的准备失败后改用 IPv6 SSH 控制通道重试，原生服务身份未改变。计划重置后的低 RSS 不能作为优化证据。

后续[六档隔离回环矩阵](../benchmarks/internal-matrix.json)已串行完成：UDP 密集、热点和冷门各以 8000 次/秒为目标，IPv6 HTTP 新连接目标 1000、IPv4 HTTPS 新连接目标 500、IPv4 HTTP/UDP 混合目标 8000；每档至少 600 秒，协议成功率均为 100%，未记录 OOM 或重启。HTTPS 档实际发起约 495.27 次/秒，其他档的实测速率和分位延迟见记录。套件启用私网测试放宽，客户端与服务共享 S1 两核，不是公网容量或服务器最大上限；六个不同负载的串行总时长也不是某一种负载一小时确认。一小时公网复测和修正后的 24 小时观察已取消，均未验证。

服务端 `service_time` 是 HTTP announce/scrape 与已收到 UDP 报文的混合内部处理时间，包括限流和提前拒绝，但不包含 socket 发送和网络传输；不要把它当作成功响应端到端分位。记录测试前后桶计数差并说明覆盖范围；`maintenance` 给出增量清理调用数、累计和最大耗时，`udp_send_errors`、`http_busy` 单独统计发送失败和并发拒绝。服务端 UDP 字节与客户端应用字节都不替代 S1 外网网卡 RX+TX 增量。

独立于项目编解码库的 Rust 报文检查器见 [validation/README.md](../validation/README.md)。它只连接回环隔离端口，单独构造 BEP 报文和解析回复，验证二进制查询、长度与 cookie 边界以及跨入口真实互见；不能替代公网或长时间测量。编译安排在压力阶段之间，避免将编译 CPU 混入容量结果。

20 GiB 是所有公网测试的总预算而非每次运行预算。以同一隔离网卡测试前 RX+TX 为基准，持续按秒计增量，达到 18 GiB 立即停止发流；包括失败、重传和通过 S2 转发产生的 S1 实际外网流量。预算计算使用小样本实测每次操作的网卡字节，乘以计划请求数并留至少 20% 余量。本次不再启动新阶段。举例：假设每次总网卡流量为 2 KiB，100/1000/5000 次每秒各跑十分钟约消耗 0.11/1.14/5.72 GiB；这只是规划算术，必须用实测取代。相同假设下，18 GiB 均分 24 小时仅约 109 次/秒，实际可用值还要扣除前序测试和余量。
