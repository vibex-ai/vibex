# Publishing plugins

The desktop market reads one signed static index. Nothing runs a server: the
index is a JSON file and a detached Ed25519 signature, hosted anywhere that can
serve two files over HTTPS. `plugins.peatboy.com` is the default address; a
build can point elsewhere with `VIBEX_PLUGIN_INDEX`.

## The two sides

- **Authors** build a `.vibexplug` package, sign it with their own release key,
  and print the entry to submit. `vibex-plugin publish` never edits the catalog.
- **Maintainers** review submissions, add them to the catalog repository, and
  rebuild the signed index. `vibex-plugin index build` never touches packages.

That split is deliberate: an author cannot add themselves to the marketplace,
and a maintainer cannot sign a package it did not build.

## Author: `vibex-plugin publish`

```sh
vibex-plugin publish ./my-plugin --key "$PUBLISHER_KEY"
```

It writes `my-plugin.vibexplug` and `my-plugin.vibexplug.sig`, then prints the
catalog entry:

```json
{
  "id": "com.example.widgets",
  "version": "1.2.0",
  "name": "Widgets",
  "license": "MIT",
  "categories": ["panels"],
  "versions": [
    {
      "version": "1.2.0",
      "url": "com.example.widgets-1.2.0.vibexplug",
      "sizeBytes": 12345,
      "sha256": "…",
      "sha512": "…",
      "signature": { "alg": "ed25519", "keyId": "…", "signature": "…" }
    }
  ]
}
```

The `url` is relative to the catalog's artifact base; an absolute `https://`
address works too, and `github:owner/repo` installs the repository's archive at
a pinned commit.

## Catalog repository layout

```text
plugins/<publisher>/<id>/plugin.json   one reviewed entry
publisher-keys.json                    publisher public keys the index vouches for
revocations.json                       blocked plugins and versions
dist/index.json                        built and signed
dist/index.json.sig
```

Each `plugin.json` is a `PluginIndexEntry`: the listing (`entry`), its
`versions`, and optionally `publisherKeyId`, `description`, `readme`,
`screenshots` and `reviews`. `vibex-plugin index validate` reads and checks
every one.

## Maintainer: build and sign

```sh
vibex-plugin index validate --catalog ./catalog
vibex-plugin index build \
  --catalog ./catalog \
  --key "$ROOT_KEY" \
  --revision 42 \
  --publisher-keys ./catalog/publisher-keys.json
```

`index build` writes `catalog/dist/index.json` and `catalog/dist/index.json.sig`
and prints the index's SHA-256 and the signing key id. Publish `dist/` as static
files. The `revision` must increase; the client refuses an index older than the
one it already accepted, so a replayed older index cannot roll a catalog back.

`--generated-at` accepts an RFC3339 timestamp for reproducible builds; it
defaults to the current UTC time.

## Trust

- The app embeds the index root public key at build time (`VIBEX_PLUGIN_ROOT_KEY`
  in CI). Without it, release builds refuse to load an unsigned index; a
  development build may set `VIBEX_PLUGIN_ALLOW_UNVERIFIED_INDEX=1`.
- A version's `signature` is a publisher signature. It verifies against the key
  named by `publisherKeyId` in `publisher-keys.json`; only then does the install
  count as **Verified**. A signature naming a key the index does not carry is
  refused.
- There is no self-asserted "official": the trust tier comes from the signed
  index and the verified signature, never from the entry's own JSON.

## Revocations

`revocations.json` lists blocked plugin ids and versions:

```json
{
  "schema": 1,
  "revision": 3,
  "blockedPlugins": [
    { "id": "com.example.bad", "versions": [], "reason": "abandoned" },
    { "id": "com.example.widgets", "versions": ["1.0.0"], "reason": "unsafe", "advisoryUrl": "https://…" }
  ]
}
```

An empty `versions` list blocks every version. Sign it with the root key and
publish `revocations.json.sig` beside it; the client refreshes it with the
catalog and only accepts a higher `revision`.

## Mirrors

The client tries addresses in order, so a catalog may be mirrored:

1. `https://plugins.peatboy.com/index.json`
2. `https://raw.githubusercontent.com/<owner>/<repo>/dist/index.json`
3. a jsDelivr copy of the same path

Every address must serve the same `index.json.sig`.
