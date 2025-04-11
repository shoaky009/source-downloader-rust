
本项目是对[SourceDownloader](https://github.com/shoaky009/source-downloader)项目的功能进行Rust重写的版本

- **sdk**：定义插件开发需要实现的 trait、模型和工具。
- **plugins/common**：内置插件的集合，实现 sdk 中定义的接口。
- **core**：项目核心功能模块，组合 plugins 提供能力，供应用层调用。
- **applications/web**：最终的 Web 应用入口，依赖 core，提供对外服务。

---
