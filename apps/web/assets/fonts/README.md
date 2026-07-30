# Inter Variable Web Runtime Fonts

These SFNT files are deterministic decompressions of the WOFF2 assets from
`@fontsource-variable/inter@5.2.8`, already locked by `pnpm-lock.yaml` and used
by the desktop GPUI shell. The locked Web font parser rejects WOFF2, so the
Gate embeds the equivalent decompressed font data.

Regenerate with FontTools:

```bash
fonttools ttLib.woff2 decompress \
  node_modules/.pnpm/@fontsource-variable+inter@5.2.8/node_modules/@fontsource-variable/inter/files/inter-latin-wght-normal.woff2 \
  -o apps/web/assets/fonts/inter-latin-wght-normal.ttf
fonttools ttLib.woff2 decompress \
  node_modules/.pnpm/@fontsource-variable+inter@5.2.8/node_modules/@fontsource-variable/inter/files/inter-latin-ext-wght-normal.woff2 \
  -o apps/web/assets/fonts/inter-latin-ext-wght-normal.ttf
```

The Inter fonts remain licensed under the SIL Open Font License 1.1 in `OFL.txt`.

`wqy-microhei.ttc` is the bundled CJK fallback used when the Inter family does
not contain a glyph. It is WenQuanYi Micro Hei 0.2.0-beta, licensed under
Apache-2.0; the complete license text is in `APACHE-2.0.txt`.
