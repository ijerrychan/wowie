# 谷歌翻译插件

插件使用 Google Translate 的 `translateHtml` 接口翻译文本。输入 `gt ` 进入翻译；`tmt ` 仍作为兼容命令。

## 配置

插件内置了该接口使用的 API key，可直接使用。需要替换 key 时，可打开启动器右侧的设置按钮，在 Google Translate 页面填写；也可以在 Wowie 的 `config.json` 中将以下配置合并到顶层 `plugins` 对象。Windows 通常位于 `%APPDATA%\wowie\config.json`：

```json
{
  "plugins": {
    "google-translate": {
      "enabled": true,
      "weight": 0,
      "preferences": {
        "google_api_key": "你的 Google Translate API key"
      }
    }
  }
}
```

原 `tencent-translate` 配置会继续用于启用状态、权重和插件偏好设置；新配置项优先。旧版 TokenHub 的 `api_key`、`model` 和 `endpoint` 不再使用。配置文件保存后由宿主热加载。

## 使用

目标语言默认跟随 Wowie 界面语言；如果源语言与目标语言相同，会自动改为英语或简体中文。源语言默认自动检测。语言菜单支持按名称或代码筛选，并将当前选择置顶和标记。

插件调用 `https://translate-pa.googleapis.com/v1/translateHtml`，请求格式为 Google Translate 网页接口使用的 JSON protobuf。此接口不同于 Google Cloud Translation 的正式 REST API。
