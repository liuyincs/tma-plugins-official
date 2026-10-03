# plugins/

每个子目录是一个官方插件（独立 workspace，从 `templates/plugin-template` 复制起步）。

发布约定：tag `<目录名>-v<semver>`（如 `template-v0.1.0`）触发 CI 构建、签名、
创建 GitHub Release 并把条目回写 `catalog.json`（见
`../.github/workflows/release.yml` 与 `../docs/authoring.md`）。
