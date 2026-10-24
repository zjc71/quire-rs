# 贡献指南

[English](CONTRIBUTING.md) | **简体中文**

感谢你的帮助。以下规则用于保持项目可维护，并确保法律上干净。

## 开始之前

- 阅读 [`CLEANROOM.md`](CLEANROOM.md)。
- 超出简单 bug 修复的改动，请先开 issue 说明行为以及证据来源（规范、观察或
  设计）。
- 阅读 [`README.md`](README.md#架构) 中的架构概览。

## 开发

```powershell
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
```

要求：

- 库 crate 中不要使用 `unwrap`/`expect`；返回 `Result`。
- 除非某个 crate 明确需要平台 API，否则保持 `#![forbid(unsafe_code)]`。
- 排版和导出必须确定性：不使用时钟、随机数或哈希表遍历顺序。
- 新的格式行为需要测试；影响排版时还需要 golden fixture。
- 除指定的 shell 和后端 crate 外，其他 crate 不得依赖 UI 或平台。

## Pull Request

- 每个 PR 只做一个逻辑改动。
- 在描述中说明证据来源。
- 包含测试。
- 不要提交生成文件、`target/` 或第三方资源。

## 提交信息

使用简短的祈使句标题，可选正文解释“为什么”。示例：

```
layout: keep closing punctuation on the previous line

Implements the basic 避头 rule from the East Asian line-breaking table.
Spec: UAX #14 section 5.2.
```

## 许可

提交 pull request 即表示你同意你的贡献按 Apache License 2.0 授权，不附加任何
额外条款或条件（Apache-2.0 第 5 条）。见 [`LICENSE`](LICENSE)。
