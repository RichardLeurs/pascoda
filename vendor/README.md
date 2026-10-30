### Vendored sources (in `vendor/`, not committed)

| Folder | What | Source | Used for |
|---|---|---|---|
| `vendor/fpc` | the Free Pascal compiler (`bin/fpc`) and its precompiled units, with licences and a source offer | copied from an installed Free Pascal 3.2.2 by `packaging/fpc-bundle.sh` | compiling Pascal; wins over an `fpc` on the `PATH` |
| `vendor/pasls` | Pascal language server | github.com/genericptr/pascal-language-server | completion, go-to-definition |
| `vendor/lazarus` | Lazarus source tree (tested at commit `2dbcbed8`) | gitlab.com/freepascal.org/lazarus/lazarus | `pasls` CodeTools backend and the LCL for GUI apps |
| `vendor/fpcsrc` | FPC 3.2.2 `rtl` and `packages` (tag `release_3_2_2`) | gitlab.com/freepascal.org/fpc/source | lets the language server read standard units, and the database and FPCUnit sources |

Set them up from the project root. Install Free Pascal first (`brew install fpc`), because `pasls` is built with it
and `vendor/fpc` is copied from it:

```sh
mkdir -p vendor && cd vendor
git clone --depth 1 --recurse-submodules https://github.com/genericptr/pascal-language-server.git pasls
git clone --depth 1 --filter=blob:none https://gitlab.com/freepascal.org/lazarus/lazarus.git lazarus
git clone --depth 1 --branch release_3_2_2 --filter=blob:none --sparse \
  https://gitlab.com/freepascal.org/fpc/source.git fpcsrc
(cd fpcsrc && git sparse-checkout set rtl packages)
(cd pasls/src && LAZARUSDIR="$PWD/../../lazarus" bash build_fpc.sh)
cd .. && packaging/fpc-bundle.sh vendor/fpc     # after the clones: it takes the licence texts from them
```

The language server binary ends up in `vendor/pasls/dist/aarch64-darwin/pasls`. Without it the
IDE still works; completion is simply off. Without `vendor/lazarus`, GUI programs cannot be
built. Without `vendor/fpc` the IDE uses the `fpc` on your `PATH` (or `PASCODA_FPC`), so a checkout also
works with only `brew install fpc`; `vendor/fpc` makes it use the same compiler and units as the shipped app.