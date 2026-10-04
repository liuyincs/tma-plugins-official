# tma-plugin-sdk

`tma-plugin-sdk` provides the Tag My Audio plugin ABI types, guest helpers, exported plugin macros, and optional `.tmap` packaging helpers.

The SDK currently supports host ABI `HOST_ABI` **1.6**. The minimum supported Rust version follows the Rust edition 2024 toolchain used by the workspace.

Use the `package` feature only in packaging tools. Plugin crates do not need signature, archive, or host runtime dependencies.

```toml
tma-plugin-sdk = "0.2"
```

The `plugin!` and `scrape_plugin!` macros export the standard WASM entry points and host function declarations. On `wasm32`, the SDK supplies the Extism PDK through `tma_plugin_sdk::__rt::extism_pdk`.
