#!/bin/sh
# NoralWeb terminal istemcisi (Linux): `noral` komutunu kurar.
# Kullanim: curl -fsSL https://raw.githubusercontent.com/samansarmasik-alt/NoralWeb/main/install.sh | sh
# Test: INSTALL_DIR=/tmp/noraltest sh install.sh  (PATH degismez)
set -e
TAG="v0.37.0"
ASSET="noral-cli-linux-v37"
URL="https://github.com/samansarmasik-alt/NoralWeb/releases/download/$TAG/$ASSET"
DIR="${INSTALL_DIR:-$HOME/.local/bin}"
mkdir -p "$DIR"
echo "indiriliyor: $URL"
curl -fsSL -o "$DIR/noral" "$URL"
chmod +x "$DIR/noral"
"$DIR/noral" --version
case ":$PATH:" in
  *":$DIR:"*) echo "PATH'te zaten var." ;;
  *) echo "NOT: $DIR PATH'te yok — ekle: export PATH=\"\$DIR:\$PATH\"" ;;
esac
echo 'kullanim: noral "sorgu"  |  noral --agent "soru"  |  noral --testmode'
