# tma-plugins-official

[Tag My Audio](https://github.com/liuyincs/tma) 的官方插件仓库：托管经官方密钥签名的
`.tmap` 插件包与静态 `catalog.json`，同时是插件作者的入口（模板 + 指南）。
TMA 服务端现有内置插件将陆续迁移到本仓库分发。

## 用户：安装官方插件

在 TMA 服务端「插件管理 → 插件仓库」添加本仓库 catalog 地址，即可浏览、安装与升级
官方插件：

```
https://raw.githubusercontent.com/liuyincs/tma-plugins-official/main/catalog.json
```

官方插件使用官方签名密钥签名，对应公钥编译进服务端，无需额外信任配置。

## 插件作者

- 开发指南：[docs/authoring.md](docs/authoring.md)（SDK、manifest 字段、签名打包、
  catalog 发布、`TMA_PLUGIN_PUBKEYS` 第三方信任）
- 起步模板：[templates/plugin-template](templates/README.md)

本仓库的官方插件按 `plugins/<name>/` 组织，发布由 tag `<name>-v<semver>` 触发的 CI
完成（见 `.github/workflows/release.yml`）。第三方作者可参照本文档在自己的仓库
分发插件。

## License

`MIT OR Apache-2.0`（[LICENSE-MIT](LICENSE-MIT) / [LICENSE-APACHE](LICENSE-APACHE)）。
