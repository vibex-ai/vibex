#!/usr/bin/env node
// Refreshes the bundled models.dev catalogue that ships with the binary.
//
// The catalogue is the only source of a Provider's model ids: a preset names a
// vendor and an endpoint, never a list. That makes this file load-bearing at
// first launch and offline, so it is refreshed on a schedule rather than only
// when somebody remembers.
//
// Only the providers a preset can point at are kept. models.dev carries every
// vendor it has ever seen, and shipping all of them would put megabytes of
// entries the product can never select into every binary.
//
// Usage:
//   node scripts/sync-model-catalog.mjs            # write the asset
//   node scripts/sync-model-catalog.mjs --check    # fail if it is stale

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const PRESETS_PATH = join(ROOT, "crates/core/src/provider_presets.rs");
const ASSET_PATH = join(ROOT, "crates/model-catalog/assets/models-dev-bundled.json");
const CATALOGUE_URL = "https://models.dev/api.json";

// The fields the editor reads. Everything else in an upstream entry — prices,
// descriptions, knowledge cutoffs — is deliberately dropped, both to keep the
// asset small and to keep money out of the product's contracts.
const MODEL_FIELDS = [
  "name",
  "family",
  "release_date",
  "reasoning",
  "reasoning_options",
  "temperature",
  "tool_call",
  "structured_output",
  "canonical_model_id",
  "status",
  "modalities",
];

function fail(message) {
  console.error(message);
  process.exit(1);
}

/** The catalogue ids the preset table points at, in file order. */
function presetCatalogIds() {
  const source = readFileSync(PRESETS_PATH, "utf8");
  const ids = new Set();
  const pattern = /catalog_id:\s*Some\("([^"]+)"\.to_string\(\)\)/g;
  for (const match of source.matchAll(pattern)) {
    ids.add(match[1]);
  }
  return [...ids].sort();
}

/** One upstream entry, trimmed to the fields the product reads. */
function trimModel(id, model) {
  const row = { id };
  for (const field of MODEL_FIELDS) {
    if (model[field] !== undefined && model[field] !== null) {
      row[field] = model[field];
    }
  }
  const limit = model.limit ?? {};
  const trimmedLimit = {};
  for (const [key, value] of Object.entries(limit)) {
    if (value !== null && value !== undefined) {
      trimmedLimit[key] = value;
    }
  }
  if (Object.keys(trimmedLimit).length > 0) {
    row.limit = trimmedLimit;
  }
  return row;
}

function trimCatalogue(live, wanted) {
  const trimmed = {};
  const missing = [];
  for (const providerId of wanted) {
    const provider = live[providerId];
    if (!provider) {
      missing.push(providerId);
      continue;
    }
    const models = {};
    for (const [modelId, model] of Object.entries(provider.models ?? {})) {
      models[modelId] = trimModel(modelId, model);
    }
    trimmed[providerId] = {
      id: providerId,
      name: provider.name,
      api: provider.api,
      doc: provider.doc,
      models,
    };
  }
  return { trimmed, missing };
}

async function main() {
  const check = process.argv.includes("--check");
  const wanted = presetCatalogIds();
  if (wanted.length === 0) {
    fail("the preset table names no catalogue entry; the asset has no owner");
  }

  const response = await fetch(CATALOGUE_URL);
  if (!response.ok) {
    fail(`models.dev answered ${response.status}; not rewriting the asset`);
  }
  const live = await response.json();
  const { trimmed, missing } = trimCatalogue(live, wanted);
  if (missing.length > 0) {
    // A preset that names an entry the catalogue has dropped would silently
    // offer no models, so the mismatch fails the sync instead of shipping.
    fail(`the preset table names catalogue entries models.dev no longer has: ${missing.join(", ")}`);
  }

  const encoded = `${JSON.stringify(trimmed, null, 2)}\n`;
  if (check) {
    let current = "";
    try {
      current = readFileSync(ASSET_PATH, "utf8");
    } catch {
      fail(`${ASSET_PATH} is missing; run node scripts/sync-model-catalog.mjs`);
    }
    if (current !== encoded) {
      fail(`${ASSET_PATH} is stale; run node scripts/sync-model-catalog.mjs`);
    }
    console.log(`model catalogue is current (${Object.keys(trimmed).length} providers)`);
    return;
  }

  writeFileSync(ASSET_PATH, encoded);
  const modelCount = Object.values(trimmed).reduce(
    (total, provider) => total + Object.keys(provider.models).length,
    0,
  );
  console.log(
    `wrote ${ASSET_PATH}: ${Object.keys(trimmed).length} providers, ${modelCount} models`,
  );
}

await main();
