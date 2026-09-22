# 腾讯翻译插件

插件通过腾讯 TokenHub 的 Hy-MT2 专用翻译接口翻译文本。腾讯云传统机器翻译 `TextTranslate` 接口已移除，新接入建议使用 TokenHub 的 `hy-mt2-plus`、`hy-mt2-pro` 或 `hy-mt2-lite`。

## 配置

先在 TokenHub 控制台开通服务并创建 API Key，然后在 Wowie 的 `config.json` 中保留现有配置，并将以下内容合并到顶层 `plugins` 对象。Windows 通常位于 `%APPDATA%\wowie\config.json`：

```json
{
  "plugins": {
    "tencent-translate": {
      "enabled": true,
      "weight": 0,
      "preferences": {
        "api_key": "你的 TokenHub API Key",
        "model": "hy-mt2-plus",
        "endpoint": "https://tokenhub.tencentmaas.com/v1/api/translations"
      }
    }
  }
}
```

`api_key` 必填。`model` 可选，默认 `hy-mt2-plus`，也可设为 `hy-mt2-pro` 或 `hy-mt2-lite`。`endpoint` 可选；中国大陆默认地址为 `https://tokenhub.tencentmaas.com/v1/api/translations`，国际接入可使用 `https://tokenhub-intl.tencentcloudmaas.com/v1/api/translations`。也可以填写 `/v1` 基地址，插件会自动补上翻译接口路径。配置文件保存后由宿主热加载。

Hy-MT2 单次输入上限为 4,000 tokens；超过上限的文本需要拆成多次翻译。

配置好后，在启动器中输入 `tmt ` 进入腾讯翻译。翻译目标默认跟随 Wowie 界面语言；如果检测到原文已经是该语言，会自动改译为英语或简体中文。也可以在目标语言菜单中选定固定语言，当前选择会置顶并标记。

## 语言选择

- 源语言默认自动检测，也可以从菜单中选择。
- 目标语言默认自动选择：使用 Wowie 界面语言作为目标；源文与目标相同时，在英语和简体中文之间切换。
- 语言菜单只列出 Hy-MT2 支持的语种，并支持按名称或语言代码筛选；当前选择始终显示在列表首位。

插件图标是随插件本地打包的腾讯蓝色翻译图标，供命令入口和翻译结果复用。

官方文档：

- [腾讯机器翻译快速入门与 TokenHub 迁移说明](https://cloud.tencent.com/document/product/551/104415)
- [TokenHub 混元调用指南与 Hy-MT2 翻译接口](https://cloud.tencent.com/document/product/1823/132252)
- [TokenHub 模型列表](https://cloud.tencent.com/document/product/1823/130051)
