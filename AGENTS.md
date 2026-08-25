# KMA Audio Agent Guide

## Scope

- Rust 服务独占 FLOW 8 USB Audio/MIDI；不提供入站 HTTP。
- Server 是路由、录音开关、Mixer desired state 和 session 的唯一事实来源。
- Linux 音频实现通过 `linux-audio` feature 构建；核心协议与状态机必须可在无硬件环境测试。

## Invariants

- 版本固定为 `0.0.0`，目标为 Debian 13 amd64/J1900 通用 `x86-64`，禁止 `target-cpu=native`。
- 音频固定 48 kHz；录音固定双声道 S32LE spool，最终 FLAC 的 ch1/2 分别为 Mic1/2。
- `commandId` 幂等，旧 `routeEpoch` 与旧 Mixer revision 不得写硬件。
- Stop 和关闭录音丢弃当前 take；Next、自然结束和断线超时保存。
- 未校准可播放和写 MIDI，但不得生成标记为同步的录音。
- Token、Authorization、pairing code 不得进入远程日志或错误上下文。

## Commands

```bash
cargo fmt --check
cargo test
cargo clippy --all-targets -- -D warnings
cargo build --release --features linux-audio
cargo deb --no-build
```
