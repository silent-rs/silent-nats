# silent-nats

## 项目概述

本项目是 Silent Odyssey 协议验证路线图的第二阶段，目标是在 Silent 框架和 Tokio 运行时基础上，实现一个轻量级且兼容 NATS 协议的服务器。该服务器用于验证异步事件路由、发布/订阅机制以及队列分组等核心能力，基于 Rust 语言环境构建。

## 协议范围

当前服务器支持 NATS 协议的核心子集，包含以下命令和消息：

- `INFO`
- `CONNECT`
- `PING` / `PONG`
- `SUB` / `UNSUB`
- `PUB`
- `MSG`

暂未实现 JetStream 及其他高级扩展功能。

## 架构设计

项目建议的文件与模块结构如下：

```
src/
├── main.rs
├── wire.rs         # NATS 文本协议解析与封包
├── server.rs       # 订阅路由与消息分发
├── subjects.rs     # 通配符匹配 (* 和 >)
└── client.rs       # 连接与会话管理
```

此模块化设计便于职责分离，提升代码可维护性和扩展性。

## 使用示例

启动 Silent NATS 服务器，并使用官方 `nats` CLI 连接：

```bash
cargo run
```

在另一个终端发布消息：

```bash
nats pub test.hello "Hello from Silent NATS"
```

订阅主题模式：

```bash
nats sub test.>
```

使用队列分组，多个订阅者会在同一队列内进行负载均衡：

```bash
nats sub --queue workers test.>
```

以上示例演示了基础的发布/订阅功能及通配符支持。

## 测试与压测

可以使用自定义的 Rust 或 Shell 脚本进行发布与订阅的吞吐量和延迟测试。此外，也可以使用官方 [`nats cli`](https://github.com/nats-io/natscli) 的 `bench` 命令对本服务器进行性能评测，例如：

```bash
nats bench test --pub 10 --sub 2 --msgs 100000
```

## 当前功能与路线图

- ✅ 基础协议支持：`INFO`、`CONNECT`、`PING/PONG`、`SUB/UNSUB`、`PUB`、`MSG`
- ✅ 队列分组支持：同一队列名称的订阅者按轮询顺序接收消息
- ❌ JetStream 及高级 NATS 功能（未来规划）
