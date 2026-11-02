#!/bin/zsh
# Build a fresh FYLO root for the fylo link conformance runs:
# `products` (head schema name, price?, group?) with 60 documents, and an
# unschematized `notes`. Usage: make-root.sh <fylo-binary> <root>
set -eu
fylo=$1
root=$2
schemas=${root}.schemas
rm -rf "$root" "$schemas"
mkdir -p "$root" "$schemas/products/history"
print -r -- '{"name":"^.+$","price?":"^-?[0-9.]+$","group?":"^.+$"}' > "$schemas/products/history/v1.schema.json"
print -r -- '{"current":"v1","versions":[{"v":"v1"}]}' > "$schemas/products/manifest.json"
request() { "$fylo" exec --request "$1" --root "$root" > /dev/null; }
request '{"op":"createCollection","collection":"products"}'
request '{"op":"createCollection","collection":"notes"}'
"$fylo" schema push products --schema-dir "$schemas" --root "$root" > /dev/null
for n in $(seq 0 59); do
    request "{\"op\":\"putData\",\"collection\":\"products\",\"data\":{\"name\":\"item-$(printf %04d $n)\",\"price\":$((n % 20)),\"group\":\"g$((n % 3))\"}}"
done
echo "root ready: $root"
