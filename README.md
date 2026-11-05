# 岛仔与小工具 安装包

这个分支只放安装包，不含代码。源码在各仓库的 `claude/ecstatic-curie-10uz49` 分支。

## 文件

- **Daozai-Setup-v0.6.50-x64.exe**：岛仔安装包。含界面缩放、小工具页（2×2 卡片）、语音输入岛上实时显示、亮度提示、Claude 提醒、常驻指示、AutoBoost 联动。双击安装，覆盖旧版，保留设置。
- **VoiceInput-win-x64.zip** / **AutoBrightness-win-x64.zip** / **DeskHub-win-x64.zip**：三个工具的新版本（需要 .NET 10 桌面运行时，和原来一样）。

## 更新三个工具

1. 先在托盘里退出旧版本。
2. 把 zip 解压覆盖到原来的安装目录（例如 `%USERPROFILE%\VoiceInput`）。zip 里不含 config.json、hue.key、doubao.key、模型文件，你的设置和模型都不会被覆盖。
3. 重新启动工具。岛仔开着时，状态会显示在岛上；岛仔没开时一切照旧。

AutoBoost 不需要更新。

## SHA256

```
1124b4c9b20852b4058c10bae913936ac143614d1023e99e83c678062144f123  Daozai-Setup-v0.6.50-x64.exe
c7e6dfea5d3ed4f0ccc46624f892b44fce53111d301b0f0cb73d2fcf95580dab  AutoBrightness-win-x64.zip
f1b98d0e7a875549c59af54a06089a77d7093466de9bd1b7276330e2d913f2c0  DeskHub-win-x64.zip
7f718fe810e326cdd5e2137082a143ca1b6d3b02f5301118b426bfc58f54c15d  VoiceInput-win-x64.zip
```
