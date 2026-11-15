#!/system/bin/sh
SKIPMOUNT=true
[ "$BOOTMODE" = true ] || abort '请在 Magisk / KernelSU / APatch 管理器中安装'
[ "$ARCH" = arm64 ] || abort '此模块只包含 arm64-v8a'
[ "$API" -ge 31 ] || abort '需要 Android 12 或更高版本'

set_perm_recursive "$MODPATH" 0 0 0755 0600
set_perm "$MODPATH/bin/sb4a" 0 0 0700
set_perm "$MODPATH/bin/rootbox" 0 0 0700
set_perm "$MODPATH/service.sh" 0 0 0700
set_perm "$MODPATH/uninstall.sh" 0 0 0700

ui_print '━━━━━━━━ sb4a 已安装 ━━━━━━━━'
ui_print '首次安装不接管网络。'
ui_print '导入配置：sb4a config import < config.json'
ui_print '启动核心：sb4a start'
ui_print '开机自启：sb4a autostart on'
ui_print '查看状态：sb4a status'
ui_print '规则集下载：sb4a ruleset download geosite cn'
ui_print '详细说明见源码仓库 README.md'
