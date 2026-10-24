# Quire

[English](README.md) | **简体中文**

Quire 是一个用 Rust 编写的净室（clean-room）文字处理与表格编辑器。它包含文档
模型、DOCX/RTF/ODT/TXT 读写、XLSX/CSV 值网格、确定性的断行与分页、SVG 绘制
后端，以及一个原生桌面外壳（`quire-view`，基于 eframe）。

核心 crate 与 UI、平台无关。本工作区没有任何 crate 进行网络 I/O，且每个 crate
都启用了 `#![forbid(unsafe_code)]`。

## 项目状态

核心功能可用，并带有原生编辑器外壳。项目可以构建、测试并生成真实输出，但还
不能完全替代完整的办公套件。

| 模块 | 目前 | 下一步 |
| --- | --- | --- |
| 模型 | 节、段落、run、样式、字体、图片、表格、编号、页眉页脚、页码字段 | 更多字段类型、脚注 |
| 导入 | DOCX、RTF、ODT、TXT，支持段落、run、样式、字体、图片、表格、编号、页眉页脚和 PAGE/NUMPAGES；XLSX/CSV 缓存值导入为工作表网格 | 旧版 .doc/.xls 和 PDF 导入；表格公式 |
| 导出 | CLI 和编辑器可导出 DOCX、RTF、ODT、TXT；编辑器可将 XLSX/CSV 计算值写回 | 完整的 OOXML 往返、表格样式与公式 |
| 排版 | 断行、分页、图片、表格、列表标签、页眉页脚变体、逐页字段 | 行跨页、浮动对象、孤行/寡行控制、增量重排版 |
| 绘制 | SVG 预览，支持逐 run 字体、图片、单元格填充与边框 | GPU（wgpu + vello）、PDF、打印 |
| 编辑 | 文档：光标、选区、输入法、格式、样式、拆分/合并、查找替换、表格单元格与行列编辑、上下文表格工具、页眉底纹、撤销重做、保真保存。表格：单元格与矩形选区编辑、复制剪切粘贴、撤销重做、查找、基础公式、行列插入删除、多工作表标签、保存为 XLSX/CSV | 文档：富文本剪贴板、合并单元格。表格：完整公式库、样式、排序筛选、合并单元格 |
| 界面 | 原生外壳（`quire-view`，eframe）：文档界面带侧栏/检查器、专注模式、连续画布、表格与格式；表格界面带列标行号、公式栏、单元格编辑器、选区、撤销重做与查找工具栏、工作表标签、XLSX/CSV 保存和右键菜单 | 完整命令面板、列宽调整、合并单元格、格式设置 |

## 两种编辑界面

同一个可执行文件包含两个刻意分离的界面：

- **文档界面**：用于 DOCX、DOCM、DOTX、RTF、ODT 和 TXT。Markdown 文件
  （`.md`）按纯文本打开和保存，没有 Markdown 解析器。页面、表格、图片、页眉
  页脚都保留在可重排的文档模型中。
- **表格界面**：用于 XLSX、XLSM 和 CSV。这些文件以完整网格打开，带列标、行号、
  公式/值栏、可编辑单元格和工作表标签。支持四则运算以及
  SUM/AVERAGE/MIN/MAX/COUNT 公式。保存时写回计算后的单元格值；公式、样式、
  合并单元格和图表不会保留。打开 XLSX 后保存会先询问新的输出文件，而不是覆盖
  源文件。

## Windows 安装包

运行 `scripts/package.ps1` 会在 `dist/Quire/` 和
`dist/Quire-0.1.0-windows-x64.zip` 生成可直接运行的安装包：

```
dist/Quire/Quire.exe      双击启动（没有文档时显示欢迎页）
dist/Quire/Install.cmd    可选：安装到 LocalAppData 并注册 .docx
dist/Quire/Uninstall.cmd
dist/Quire/sample.docx
dist/Quire/sample.csv     表格界面演示
dist/Quire/sample.xlsx    双工作表演示
dist/Quire/README.txt
dist/Quire/LICENSE.txt
dist/Quire/NOTICE.txt
```

`dist/` 是生成目录，不纳入 Git。`Quire.exe` 使用静态 CRT 构建，只依赖 Windows
系统 DLL。双击即可启动并打开文件选择框；把 `.docx` 拖到 exe 上或通过命令行传入
文件路径也可以。打开 `sample.csv` 可以试用表格界面。发布版使用 Windows GUI
子系统，不会弹出控制台窗口。

`Install.cmd` 会把程序复制到 `%LOCALAPPDATA%\Programs\Quire`，注册 `.docx` 的
“打开方式”、右键菜单和开始菜单快捷方式。Windows 10/11 不允许程序静默替换默认
程序；安装器会打开设置页，由你选择 Quire 作为 `.docx` 的默认程序。

可执行文件没有代码签名，SmartScreen 可能会提示确认（“更多信息” -> “仍要运行”）。

## 构建与测试

要求：Rust 1.85 或更新版本（edition 2024）。库 crate 可跨平台；打包的桌面外壳
面向 Windows 10/11 x64。

```powershell
cargo test --workspace
cargo run -p quire-cli -- --help
cargo run -p quire-ui --bin quire-view
```

在 API 稳定之前，所有 crate 都保持 `publish = false`。

## 试用 CLI

```powershell
# 创建示例文档
cargo run -p quire-cli -- new sample.docx

# 查看信息与文本
cargo run -p quire-cli -- info sample.docx
cargo run -p quire-cli -- text sample.docx

# 排版并输出 SVG 预览
cargo run -p quire-cli -- render sample.docx -o sample.svg

# 打开原生编辑器
cargo run -p quire-ui --bin quire-view -- sample.docx

# 安全往返（遇到不支持的内容会拒绝）
cargo run -p quire-cli -- rewrite sample.docx -o rewritten.docx
# 绕过保真检查的诊断往返
cargo run -p quire-cli -- rewrite sample.docx -o forced.docx --force

# 在第 1 个段落的第 6 个字符处插入文本
cargo run -p quire-cli -- insert sample.docx -o edited.docx --paragraph 1 --offset 6 --text "real "
```

用浏览器打开 `sample.svg`。默认每个文本 run 都带 `textLength`，因此预览会保持
排版使用的确定性字宽。传入 `--system-fonts` 可以用已安装字体测量，并让查看器
按字体的自然字宽渲染字形。

## 保存安全

文档写入器**拒绝**保存包含无法回写内容的文档，而不是静默丢弃数据：

```
$ quire rewrite document.docx -o out.docx
Error: document uses content that cannot be written back yet:
fields, hyperlinks, unsupported run properties
```

`--force` 会绕过检查，仅用于诊断。未解析的部分（theme、fontTable、docProps、
customXml、embeddings 等）会原样保留，其 content type 和关系会合并进重新生成的
包中。

不要对生产文档使用强制路径；默认的保真检查路径才是安全路径。读取和渲染都是
非破坏性的。

## 已知限制

### 文档模型与读写

- 除 `PAGE`/`NUMPAGES` 之外的字段没有建模；页眉页脚之外的页码字段会阻止严格
  保存。其他字段类型会作为可见文本保留。
- 脚注、尾注、批注和修订没有建模。
- 只写入段落样式和字符样式；表格样式和编号样式会被丢弃。
- 超链接、书签、内容控件、符号、ruby 和 alternate content 没有建模。
- 只写入第一个节，因此多个节以及逐节的页眉页脚组合会合并。
- 浮动/锚定图形、旧式 drawing 和文本框没有建模。
- 通过段落样式定义的编号（`w:pStyle` 加样式级 `numPr`）不会应用；只读取直接
  的 `w:numPr`。
- 不支持的 `pPr`/`rPr`/`sectPr`/表格属性子元素（例如制表位、keep-with-next、
  分栏和表格样式）不会写入。字体族、复杂文种字号、复杂文种粗体/斜体和语言
  已支持。
- 导出 ODT 时会拒绝带编号的文档，而不是把列表拉平（列表编号尚未写入 ODT）。
- 不支持旧版二进制 `.doc`/`.xls` 和 PDF 导入。

### 排版与绘制

- 表格行不跨页，表头行不会重复。
- 单元格矩形独立绘制，因此纵向合并保留文字，但不会绘制成一个合并单元格。
- 未实现自动列宽；网格宽度按版面宽度缩放。
- 嵌套表格按扁平单元格内容读取，不参与排版。
- 列表标签以内联分隔符放置，而不是真正的制表位，因此编号与正文的间距是近似值。
- 列表重启规则是简化版：切换 `numId` 会重启列表，较浅层级前进时较深层级会重置。
- 页眉页脚距离固定为 36 pt；不读取 `w:pgMar` 中的页眉页脚值。高于页边距的
  页眉页脚会与正文重叠。
- 断行按段落缓存，但每次编辑后分页仍会遍历整个文档；页面纹理按页面指纹失效，
  而不是按字节范围标脏。
- 页面图像按需栅格化；缓存保留 16 个完整页面和 80 个缩略图，因此长文档回滚时
  会重新渲染页面。
- `SystemMetrics` 使用 rustybuzz 处理每个 token（字距和连字），但只支持从左到右
  的文本。尚未实现 bidi 和复杂文种重排序。

### 编辑

- 编号段落和页眉页脚文本还不能点击编辑。
- 跨表格单元格删除选区只清除文字，不合并单元格。
- 应用段落样式会替换段落属性和 run 格式；直接格式与继承格式没有分开跟踪，因此
  应用样式是破坏性的。
- 查找替换只扫描顶层正文段落，且只做纯文本匹配；还没有正则、全词或页眉/表格
  查找。
- 剪贴板只支持纯文本。富文本和内部片段剪贴板尚未实现。格式编辑会重建 run 并
  丢失命名 run 样式（`rStyle`）；格式只作用于选区，不会为接下来输入的字符设置
  样式。

### 表格

- 表格支持是值网格：支持四则运算和 SUM/AVERAGE/MIN/MAX/COUNT 公式，但没有完整
  公式库、样式、合并单元格、图表、冻结窗格、排序和筛选。
- 保存 XLSX 只写入计算后的单元格值。
- 演示文稿不在范围内。

## 架构

```
quire-base     单位、ID、几何、共享错误
quire-model    文档树与格式
quire-io       DOCX/RTF/ODT/TXT 与 XLSX/CSV 读写
quire-text     分词与字体度量
quire-layout   断行与分页
quire-paint    设备无关的绘制场景（目前 SVG，后续 GPU/PDF）
quire-edit     编辑操作与撤销
quire-cli      命令行装配
quire-ui       原生外壳（页面栅格化 + quire-view 可执行文件）
```

依赖方向由 crate 边界强制保证：核心与 UI 无关、与平台无关。本工作区没有任何
crate 进行网络 I/O，所有 crate 都设置了 `#![forbid(unsafe_code)]`。

## 明确的非目标

- 不做账号、登录、云同步或会员功能。
- 不做广告、遥测、分析或“推荐”。
- 不做专有文档扩展、DRM 或应用商店集成。
- 不复制任何现有办公套件的代码、字符串、图标或视觉资源。见
  [`CLEANROOM.md`](CLEANROOM.md)。

## 文档

- [`CONTRIBUTING.md`](CONTRIBUTING.md) — 构建、测试和提交规则
  （[简体中文](CONTRIBUTING.zh-CN.md)）
- [`CLEANROOM.md`](CLEANROOM.md) — 净室与来源说明政策
  （[简体中文](CLEANROOM.zh-CN.md)）

## 许可

Apache License 2.0。见 [`LICENSE`](LICENSE) 和 [`NOTICE`](NOTICE)。

除非你明确声明，否则任何有意提交以纳入本项目的贡献，均按 Apache License 2.0
授权，不附加任何额外条款或条件（Apache-2.0 第 5 条）。
