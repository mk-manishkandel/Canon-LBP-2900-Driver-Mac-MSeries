#!/bin/sh
# Remove the LBP2900 Rust driver. Run with: sudo /Library/Printers/LBP2900/uninstall.sh
set -e
ID=io.github.mk-manishkandel.lbp2900
pkill -x lbp2900-app 2>/dev/null || true
/usr/sbin/lpadmin -x Canon_LBP2900 2>/dev/null || true
rm -f /usr/libexec/cups/backend/lbp2900
rm -f /Library/Printers/PPDs/Contents/Resources/Canon-LBP2900-Rust.ppd.gz
rm -f "/Library/LaunchAgents/$ID.app.plist"
rm -rf "/Applications/LBP2900 Driver.app"
rm -rf /Library/Printers/LBP2900
pkgutil --forget "$ID" >/dev/null 2>&1 || true
echo "LBP2900 driver removed (logs kept in /Users/Shared/LBP2900)."
