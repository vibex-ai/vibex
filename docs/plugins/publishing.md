# Publishing plugins

The desktop market reads one signed static index. Nothing runs a server: the
index is a JSON file and a detached Ed25519 signature, served straight out of
the catalog repository on GitHub. There is no dedicated plugins domain — the
two hosts below are the address, and a build can point elsewhere with
`VIBEX_PLUGIN_INDEX`.

## The two sides

- **Authors** build a `.vplug` package, sign it with their own release key,
  and print the entry to submit. `vibex-plugin publish` never edits the catalog.
- **Maintainers** review submissions, add them to the catalog repository, and
  rebuild the signed index. `vibex-plugin index build` never touches packages.

That split is deliberate: an author cannot add themselves to the marketplace,
and a maintainer cannot sign a package it did not build.

## Author: `vibex-plugin publish`

```sh
vibex-plugin publish ./my-plugin --key "$PUBLISHER_KEY"
```

It writes `my-plugin.vplug` and `my-plugin.vplug.sig`, then prints the
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
      "url": "com.example.widgets-1.2.0.vplug",
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

## The official catalog

`vibex-ai/vibex-plugin` is the official catalog. Its `README.md` documents the
submission flow, and `.github/workflows/index.yml` validates every pull request
and builds and signs `dist/` on `main`. A release desktop embeds the matching
root public key and reads the index from:

1. `https://raw.githubusercontent.com/vibex-ai/vibex-plugin/main/dist/index.json`
2. `https://cdn.jsdelivr.net/gh/vibex-ai/vibex-plugin@main/dist/index.json`

`VIBEX_PLUGIN_INDEX` replaces the list with any comma-separated addresses, and
the client tries them in order, so a catalog may be mirrored anywhere. Every
address must serve the same `index.json.sig`.


## Catalogs a reader adds

The sources are fixed — the official index, GitHub topic discovery, and the
catalogs the reader types into **Plugins → Discover → Catalogs you added** — but
only the first two are the application's. A reader may add up to
`MAX_PLUGIN_USER_SOURCES` (8) sources of their own, of two kinds:

**A signed index** is the same document the official catalog publishes, at an
address the reader typed:

- The address must be absolute `https`. A plaintext or local address is
  refused before it is stored, because the catalog is fetched under the same
  public-https policy as the official one.
- The reader supplies the index signer's public key. Leaving it empty means
  accepting an **unsigned** catalog, and the form says so; the acceptance is
  stored, not implied, so a key that later turns out malformed disables the
  source instead of silently reading it unchecked.

**A Git repository** is a repository whose root carries `vibex-plugin.json` —
the same manifest topic discovery reads. It needs no key because it publishes
no signed document:

- The address is `owner/repo` or `https://github.com/owner/repo`; both are
  normalized to `owner/repo`, so the same repository added twice is one row.
  Another host or a deeper path is refused rather than guessed at.
- It yields exactly one entry, marked `Unverified`, and installs from the
  repository's archive at a **pinned commit** — resolved once and reused, so a
  push between preview and install cannot swap what gets installed.
- That path carries no published hash and nothing signs it, so it never
  auto-updates and never becomes `Verified`.

Both kinds share one id rule: `sha256(address)[..8]` in hex, so re-adding the
same address renames and updates the row instead of listing the source twice.

What a user catalog can and cannot claim is the point of the feature:

| It can | It cannot |
| --- | --- |
| List plugins with its own categories, screenshots and README | Mint `Official` or `Verified` — both are rewritten to `Community` |
| Point at a repository instead of building an index at all | Claim a reviewed signature: a repository entry stays `Unverified` |
| Sign its packages with keys it publishes | Borrow the official index's publisher keys; the two lists are stored apart |
| Fail on its own without emptying the catalog | Hide the failure: its reason is appended to the response's `partialReason` |

Entries it lists carry `listingSource: "user"` and a `sourceName`, which the
card and the detail page show, so two catalogs listing the same plugin id are
told apart. Merging is still by plugin id, and the official index wins, then a
user catalog, then topic discovery.

## Ratings

There is no review service yet, so a rating is local: **Plugins → a plugin →
Your rating** stores one to five stars in `plugin_reviews`, keyed by plugin id.
Clicking the star already given clears it. The detail page draws the reader's
own rating beside the catalog's published aggregate rather than folding the two
into one number nobody could verify — `PluginLocalReview` is the shape a
service would later publish, and nothing else has to change when one exists.
