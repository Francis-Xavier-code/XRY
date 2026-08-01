# 希尔娅 App R8 混淆规则
# Flutter/Dart 代码由 dart --obfuscate 处理（CI 构建参数）
# 保留 Flutter 引擎与插件入口
-keep class io.flutter.** { *; }
-dontwarn io.flutter.**
