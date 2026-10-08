#!/usr/bin/env bash
# 把 packaging/scoop/voicefox.json 刷成最新一次 GitHub Release 的 Windows 包。
#
# 清单里的 version / url / hash 必须三者同时正确：scoop 会在安装时校验
# sha256，手工改版本号而忘了换 hash 只会让用户装不上。所以这里直接从
# Releases API 取资产地址、下载并算 hash，不做任何字符串拼接猜测。
#
# 依赖：bash、curl、sha256sum、python3（ubuntu-latest 与本地桌面都自带）。
# 环境变量（都有默认值，本地直接跑即可）：
#   VOICEFOX_REPO          仓库        默认 emoeem/voicefox
#   VOICEFOX_WINDOWS_ASSET 资产名      默认 voicefox-windows-x86_64.zip
#   GITHUB_TOKEN           可选，提 API 限额（CI 里由 Actions 注入）
#
# 用法：packaging/scoop/update-manifest.sh [清单路径]
# 清单已是最新时打印一行说明并以 0 退出（幂等，定时任务不会空提交）。

set -euo pipefail

repo="${VOICEFOX_REPO:-emoeem/voicefox}"
asset_name="${VOICEFOX_WINDOWS_ASSET:-voicefox-windows-x86_64.zip}"
manifest="${1:-packaging/scoop/voicefox.json}"

if [ ! -f "$manifest" ]; then
    echo "找不到清单：$manifest" >&2
    exit 1
fi

curl_headers=(-H "Accept: application/vnd.github+json")
if [ -n "${GITHUB_TOKEN:-}" ]; then
    curl_headers+=(-H "Authorization: Bearer ${GITHUB_TOKEN}")
fi

release_json="$(curl -fsSL "${curl_headers[@]}" "https://api.github.com/repos/${repo}/releases/latest")"

# 输出「tag\t下载地址」，两个字段各占一行交给 bash 切分。
info="$(
    printf '%s' "$release_json" | python3 -c '
import json
import sys

release = json.load(sys.stdin)
asset_name = sys.argv[1]
assets = {asset["name"]: asset["browser_download_url"] for asset in release["assets"]}
if asset_name not in assets:
    sys.exit("最新 release " + release["tag_name"] + " 里没有资产 " + asset_name)
print(release["tag_name"] + "\t" + assets[asset_name])
' "$asset_name"
)"
tag="${info%%$'\t'*}"
asset_url="${info#*$'\t'}"

version="${tag#v}"
current="$(python3 -c 'import json, sys; print(json.load(open(sys.argv[1], encoding="utf-8"))["version"])' "$manifest")"

if [ "$current" = "$version" ]; then
    echo "清单已是 v$version，无需更新"
    exit 0
fi

archive="$(mktemp)"
trap 'rm -f "$archive"' EXIT
echo "下载 $asset_url"
curl -fsSL --retry 3 --retry-delay 2 -o "$archive" "$asset_url"
hash="$(sha256sum "$archive" | cut -d' ' -f1)"

python3 - "$manifest" "$version" "$asset_url" "$hash" <<'PY'
import json
import sys

path, version, url, digest = sys.argv[1:5]
with open(path, encoding="utf-8") as handle:
    manifest = json.load(handle)
manifest["version"] = version
manifest["architecture"]["64bit"]["url"] = url
manifest["architecture"]["64bit"]["hash"] = digest
with open(path, "w", encoding="utf-8") as handle:
    json.dump(manifest, handle, ensure_ascii=False, indent=4)
    handle.write("\n")
print(f"清单更新为 v{version}（sha256 {digest}）")
PY
