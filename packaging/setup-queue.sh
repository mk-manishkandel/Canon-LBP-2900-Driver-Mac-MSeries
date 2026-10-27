#!/bin/sh
# Create or refresh the CUPS queue "Canon_LBP2900" for a connected printer. Run as root.
PPD=/Library/Printers/PPDs/Contents/Resources/Canon-LBP2900-Rust.ppd.gz
QUEUE=Canon_LBP2900

URI=$(/usr/libexec/cups/backend/lbp2900 2>/dev/null | awk '/^direct lbp2900:/ { print $2; exit }')
if [ -z "$URI" ]; then
  # Keep an existing queue (its URI still works once the printer is back).
  if /usr/bin/lpstat -p "$QUEUE" >/dev/null 2>&1; then
    /usr/sbin/lpadmin -p "$QUEUE" -P "$PPD"
    echo "Printer not connected; updated the driver of existing queue $QUEUE"
    exit 0
  fi
  echo "Canon LBP2900 not found on USB. Connect it, switch it on and try again."
  exit 1
fi

/usr/sbin/lpadmin -p "$QUEUE" -E -v "$URI" -P "$PPD" -D "Canon LBP2900" -L "USB" \
  -o printer-is-shared=false -o PageSize=A4
/usr/sbin/cupsenable "$QUEUE"
/usr/sbin/cupsaccept "$QUEUE"
echo "Printer queue $QUEUE is ready ($URI)"
