#!/bin/sh
# Screenshot a web page at this pane's exact pixel size and show it with the
# kitty graphics protocol, so it fills the pane edge to edge at 1:1.
# usage: webshot.sh URL
url=$1
d=$(dirname "$0")
chrome="/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"

# A fresh pane is 80x24 until the app lays it out, so poll Rex for the real size.
printf 'Waiting for pane size ...'
n=0
while :; do
  size=$(rex block call -s "$REX_SESSION" -b "$REX_BLOCK" size) || exit 1
  case $size in *'"desired":[{'*) break ;; esac
  n=$(( n + 1 )); [ "$n" -ge 40 ] && break
  sleep 0.25
done
echo "size after $n polls: $size" > "$d/webshot-size.log"
printf '\r\033[2K'
cols=$(printf '%s' "$size" | jq -r .columns)
rows=$(printf '%s' "$size" | jq -r .rows)
cw=$(printf '%s' "$size" | jq -r .cell_width_px)
ch=$(printf '%s' "$size" | jq -r .cell_height_px)
r=$(( rows - 1 ))   # leave the last row for the cursor

# Cell sizes are in device pixels; the page is laid out in points (2x display).
cssw=$(( cols * cw / 2 ))
cssh=$(( r * ch / 2 ))
echo "pane ${cols}x${rows} cells of ${cw}x${ch}px -> viewport ${cssw}x${cssh}" > "$d/webshot.log"

printf 'Rendering %s ...' "$url"
"$chrome" --headless=new --disable-gpu --hide-scrollbars --force-device-scale-factor=2 \
  --window-size="$cssw,$cssh" --virtual-time-budget=8000 \
  --user-data-dir="$d/chrome-profile" --screenshot="$d/fit.png" "$url" >> "$d/webshot.log" 2>&1
[ -s "$d/fit.png" ] || exit 1
printf '\r\033[2K'

base64 -i "$d/fit.png" | tr -d '\n' | fold -w 4096 | awk -v first="a=T,f=100,q=2,c=$cols,r=$r" '
  { chunk[NR] = $0 }
  END {
    for (i = 1; i <= NR; i++) {
      more = (i < NR) ? 1 : 0
      if (i == 1) printf "\033_G%s,m=%d;%s\033\\", first, more, chunk[i]
      else        printf "\033_Gm=%d;%s\033\\", more, chunk[i]
    }
  }'
