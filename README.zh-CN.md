# Navarch

> **Navarch** /ˈnævɑːrk/（读作 NAV-ark），古希腊语，意为"舰队司令"。

一个总控，多台机器，多个编码 Agent。

Navarch 让一个总控会话把工作派给你任意一台机器上的编码 Agent（Claude Code、Codex 等），把上一个 Agent 的结果交给下一个，只在需要你拍板时才来问你。

[English](README.md)

## 当前状态

**0.1.0 开发版本。接口和文件格式仍可能调整，加密设计尚未经过独立安全审计。**

目前已有：同一个程序里的 Hub、Worker 和中继；一次性配对码配对；加密中继与可选直连；每台 Worker 的本地权限策略；通过 MCP 提供的 Hub 工具与完成事件；发布打包和用户级服务安装工具。使用方法见 [docs/](docs/)：[Worker](docs/worker.md)、[Hub](docs/hub.md)、[中继](docs/relay.md)、[安全](docs/security.md)、[设计](docs/DESIGN.md)（文档为英文）。

发布包、源码构建、服务、升级与卸载方法见[安装说明](docs/install.md)。

## 快速开始

从源码构建，或按[安装说明](docs/install.md)使用已发布的安装包：

```sh
cargo build --locked --release -p navarch-cli
mkdir -p "$HOME/.local/bin"
cp target/release/navarch "$HOME/.local/bin/navarch"
export PATH="$HOME/.local/bin:$PATH"
navarch --version
navarch hub init
navarch worker init
# 在 ~/.navarch/worker*.toml 中配置已安装的 ACP 适配器和允许访问的目录。
navarch hub serve
# 在另一终端中：
navarch worker serve
# 在另一终端中：
navarch hub status
claude mcp add --transport stdio navarch -- navarch hub mcp
```

通过 [Hub 设置](docs/hub.md)启用完成通知频道、`wait_for_events` 或
`navarch hub watch`。需要后台运行时，先用
`navarch service install hub --dry-run` 预览，再安装 Hub 与 Worker 服务。

## 解决什么问题

你有几台机器（台式机、笔记本、云服务器），也不止一个编码 Agent。现在你要自己找到对的那个会话，手动在 Agent 之间搬运上下文，再回头看有没有做完。

用 Navarch，你只跟一个总控说话。它判断你说的是哪个项目、哪个任务，选合适的 Agent 和机器，把活派出去，做完向你汇报。

## 工作方式

```
        你（手机或电脑）
              │
              ▼
   ┌──────────── Hub ────────────┐
   │  总控会话                    │   每人一个
   │  navarch hub：任务卡、角色、  │   全部状态都在这里
   │  路由表、机器表               │
   └──────────────┬──────────────┘
                  │ 主动往外连，端到端加密
                  ▼
             ┌─ Relay ─┐              只转发密文
             └────┬────┘              不存任何数据
      ┌───────────┼───────────┐
      ▼           ▼           ▼
   Worker      Worker      Worker     每台机器一个
      │           │           │
     ACP         ACP         ACP      本机，标准输入输出
      ▼           ▼           ▼
  Claude Code   Codex    其他 Agent
```

| 组件 | 跑在哪 | 做什么 |
|---|---|---|
| `navarch hub` | 总控会话所在的机器 | 保存全部状态；向总控提供工具：列出机器、派活、给已有会话追加、取消 |
| `navarch worker` | 每台干活的机器 | 主动往外连；通过 ACP 新建、续接、打断 Agent 会话；上报本机能做什么 |
| `navarch relay` | 任意一台有公网地址的主机 | 按机器 ID 转发加密消息；看不到内容，不落盘 |

三者是同一个程序的三个子命令。

## 设计原则

1. **单入口。** 你只跟总控说话，不用自己找会话。
2. **总控只调度，不干活。** 它的上下文始终很小。
3. **按强项派活。** 哪类任务交给哪个 Agent，是一张可以改的表。
4. **交接靠文件，不靠对话。** 每个任务一张任务卡，每一步留一份简短结果。任何会话丢了，工作都能接着做。
5. **关键的事由你决定。** 合并、上线、花钱、删除，都等你确认。
6. **默认只往外连。** 中继连接无需入站端口；只有明确配置直连时，Hub 才开启监听端口。

## 关键选型

- **通过 [Agent Client Protocol](https://agentclientprotocol.com) 驱动 Agent。** Worker 是一个 ACP 客户端。支持新的 Agent 只需要接入它的 ACP 适配器，不用另写一套对接。
- **用一次性配对码加入。** 在 Hub 上运行 `navarch pair`，在新机器上输入配对码。不需要账号，不需要 IP 地址。
- **端到端加密并签名。** 设计上不信任中继。中继被攻破，也读不到任务内容，冒充不了你的 Hub。
- **Worker 自己把关。** 每台机器自己决定允许哪些目录、哪些 Agent、哪些操作，不管 Hub 要求什么。
- **不提供远程执行任意命令。** Worker 只接受"让某个 Agent 做某个任务"，不接受"执行这条命令"。
- **状态只放在 Hub。** 不依赖外部数据库或存储服务。

## 安全

Navarch 的本质是让另一台机器替你执行工作，请按这个性质对待它。

- 只配对你自己控制的机器。
- 用权限尽可能小的账号运行 Worker。
- 不要在生产服务器上运行 Worker。
- 使用别人运营的中继时，对方能看到哪些机器在何时互相通信，但看不到内容。

威胁模型见 [docs/security.md](docs/security.md)。加密设计尚未经过独立审计。发现安全问题请私下报告，不要提公开 issue。

## 路线图

- [x] 单机：Hub 加一个本机 Worker，通过 ACP 驱动 Agent
- [x] 中继、配对与端到端加密
- [x] 提供给总控的 Hub 工具
- [x] 任务完成时主动通知总控
- [x] 同一局域网内直连
- [x] 打包、安装脚本、用户级服务
- [ ] 独立的安全评审

## 不做什么

- 不做聊天应用，也不做新的 Agent。Navarch 只负责协调已有的 Agent。
- 不做保存你代码或任务历史的托管控制面。
- 不做通用的远程执行工具。

## 协议

[Apache-2.0](LICENSE)

Navarch 是独立项目，与 Anthropic、OpenAI、Zed Industries 没有隶属或背书关系。Claude Code 与 Codex 分别是其所有者的产品。
