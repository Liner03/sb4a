#!/system/bin/sh
MODDIR=${0%/*}
[ -x "$MODDIR/bin/sb4a" ] || exit 1
exec "$MODDIR/bin/sb4a" stop
