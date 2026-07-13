# 仓库指南

## 项目结构与模块组织

`locale-forge` 是使用 Rust 2024 Edition 的二进制 crate。程序入口位于 `src/main.rs`，包信息与依赖统一维护在 `Cargo.toml`。按职责在 `src/` 下拆分模块，例如使用 `src/parser.rs` 或 `src/parser/lexer.rs`。目录模块必须通过同名文件定义，如 `src/parser.rs` 配合 `src/parser/`；不要使用 `src/parser/mod.rs`。

单元测试放在实现文件内，集成测试放在 `tests/*.rs`，测试数据放在 `tests/fixtures/`。构建产物位于 `target/`，不得提交到 Git。

## 构建、测试与开发命令

- `cargo run`：构建并在本地运行程序。
- `cargo check`：快速执行类型和借用检查，不生成最终二进制文件。
- `cargo build --release`：在 `target/release/` 生成优化后的程序。
- `cargo test`：运行单元测试、集成测试和文档测试。
- `cargo fmt --all -- --check`：检查所有 Rust 文件的格式。
- `cargo clippy --all-targets --all-features -- -D warnings`：执行静态检查，并将警告视为错误。

提交前应依次通过格式检查、Clippy 和完整测试。

## 编码风格与命名规范

遵循 `rustfmt` 默认格式，使用四空格缩进，不手动对齐。函数、变量和模块使用 `snake_case`，类型和 trait 使用 `PascalCase`，常量使用 `SCREAMING_SNAKE_CASE`。公共 API 使用 `///` 编写文档；避免无说明的 `panic!`，优先返回明确的错误类型。

优先使用借用和移动语义，避免无意义的 `clone()`：

- 请求对象、局部变量或查询参数后续不再复用时，应按值传递并直接消费；不要因函数签名使用引用而在内部大量克隆。
- 优先直接传入切片、迭代器或已持有的集合，避免预先调用 `to_vec()` 或 `clone()`。
- 仅在跨作用域、多处共享所有权、需要脱离锁保护范围，或业务语义明确要求独立副本时克隆数据。

模块应职责单一、边界清晰。若现有结构妨碍合理实现，可同步重构目录或接口，不要为兼容旧结构引入绕行逻辑。

## 测试规范

单元测试使用 `#[cfg(test)] mod tests` 与实现就近放置；测试名描述可观察行为，例如 `rejects_empty_locale_identifier`。端到端行为通过 `tests/*.rs` 验证。每个缺陷修复都应包含回归测试，并覆盖成功、失败和边界场景。目前未配置覆盖率阈值。

## 提交与合并请求规范

仓库目前没有历史提交可供归纳。提交标题应简短、使用祈使语气，并优先采用 Conventional Commits，例如 `feat: 支持解析区域标识` 或 `fix: 拒绝非法分隔符`。每个提交只处理一个主题，行为变更必须同时包含测试。

合并请求应说明问题、解决方案和验证命令，并关联相关 issue。CLI 行为变化时附上示例终端输出；新增依赖、配置变更或结构调整需要明确说明，避免混入无关重构。
