# bakery

## Building site

```shell
bakery ./site
```

## Building on file changes

```shell
bakery ./site --watch
```

## Previewing site with live reload

```shell
npm install -g browser-sync
browser-sync start --server ./target --watch --port 8080
```