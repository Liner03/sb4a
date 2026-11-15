#!/system/bin/sh
MODDIR=${0%/*}
[ "$#" -eq 0 ] || exit 2
[ -x "$MODDIR/bin/sb4a" ] || exit 1
exec "$MODDIR/bin/sb4a" __internal boot
