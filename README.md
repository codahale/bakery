# bakery

## Building on file changes

```shell
find . -type d -name target -prune -o -print | entr bakery .
```

## Previewing site with live reload

```shell
npm install -g browser-sync
cd target && browser-sync start -s -w --port 8080
```