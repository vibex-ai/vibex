//! The plugin publishing tool.
//!
//! ```text
//! vibex-plugin pack <dir> [--output <file>]      build a .vplug package
//! vibex-plugin verify <package>                  read a package's manifest
//! vibex-plugin hash <file>                       print sha256 and sha512
//! vibex-plugin keygen [--output <file>]          make a signing key pair
//! vibex-plugin sign <package> --key <seed>       sign a package with a release key
//! vibex-plugin sign-index <index.json> --key <seed-base64> [--output <file>]
//! vibex-plugin publish <dir> --key <seed>        sign and describe a submission
//! vibex-plugin index build --catalog <dir> --key <seed>   build and sign the catalog
//! vibex-plugin index validate --catalog <dir>    check a catalog without signing
//! ```
//!
//! `publish` is the author's side of the market: it builds the package,
//! signs it, and prints the index entry a submission carries. `index build` is
//! the maintainer's side: it reads the catalog repository's entries and writes
//! the signed `dist/index.json`. An author's tool cannot add itself to the
//! marketplace, and the maintainer's tool cannot sign a package.

use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use ed25519_dalek::{SigningKey, VerifyingKey};
use sha2::{Digest, Sha256};
use vibex_plugin::market::{PublisherKey, encode_verifying_key, sign_document};
use vibex_plugin::{
    build_index_file, load_catalog_entries, read_package_manifest, sha256_hex, sha512_hex,
    utc_now_rfc3339, write_package, write_signed_index,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("error: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = arguments.first().map(String::as_str) else {
        usage();
        return Ok(());
    };
    match command {
        "pack" => pack(&arguments[1..]),
        "verify" => verify(&arguments[1..]),
        "hash" => hash(&arguments[1..]),
        "keygen" => keygen(&arguments[1..]),
        "sign" => sign(&arguments[1..]),
        "sign-index" => sign_index(&arguments[1..]),
        "publish" => publish(&arguments[1..]),
        "index" => index_command(&arguments[1..]),
        "--help" | "-h" | "help" => {
            usage();
            Ok(())
        }
        other => Err(format!("unknown command `{other}`")),
    }
}

fn usage() {
    println!(
        "vibex-plugin <command>\n\n\
         commands:\n\
           pack <dir> [--output <file>]                          build a .vplug package\n\
           verify <package>                                      read a package's manifest\n\
           hash <file>                                           print sha256 and sha512\n\
           keygen [--output <file>]                              make a signing key pair\n\
           sign <package> --key <seed-base64> [--output <file>]  sign a package\n\
           sign-index <index.json> --key <seed-base64> [--output <file>]\n\
           publish <dir> --key <seed-base64> [--output <file>]   build and sign a submission\n\
           index build --catalog <dir> --key <seed-base64> [--revision N] [--generated-at RFC3339] [--publisher-keys <file>]\n\
           index validate --catalog <dir>                        check a catalog without signing\n"
    );
}

fn option(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|argument| argument == name)
        .and_then(|index| args.get(index + 1))
        .cloned()
}

fn positional(args: &[String]) -> Option<String> {
    args.iter()
        .find(|argument| !argument.starts_with("--"))
        .cloned()
}

fn pack(args: &[String]) -> Result<(), String> {
    let directory = positional(args).ok_or("pack needs a plugin directory")?;
    let output = option(args, "--output").map(PathBuf::from);
    let path = write_package(std::path::Path::new(&directory), output.as_deref())
        .map_err(|error| error.to_string())?;
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    println!("package: {}", path.display());
    println!("sha256:  {}", sha256_hex(&bytes));
    println!("size:    {}", bytes.len());
    // A sanity check: the archive carries a manifest.
    let manifest = read_package_manifest(&bytes).map_err(|error| error.to_string())?;
    println!("id:      {}", manifest.id);
    println!("version: {}", manifest.version);
    Ok(())
}

fn verify(args: &[String]) -> Result<(), String> {
    let path = positional(args).ok_or("verify needs a package file")?;
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    let manifest = read_package_manifest(&bytes).map_err(|error| error.to_string())?;
    println!("id:          {}", manifest.id);
    println!("name:        {}", manifest.name);
    println!("version:     {}", manifest.version);
    println!("license:     {}", manifest.license);
    println!("entry:       {}", manifest.entry);
    println!("sha256:      {}", sha256_hex(&bytes));
    println!("sha512:      {}", sha512_hex(&bytes));
    let permissions = manifest.permissions();
    if permissions.is_empty() {
        println!("permissions: none");
    } else {
        println!("permissions:");
        for permission in permissions {
            match permission.scope {
                Some(scope) => println!("  - {} ({:?}, {scope})", permission.key, permission.risk),
                None => println!("  - {} ({:?})", permission.key, permission.risk),
            }
        }
    }
    Ok(())
}

fn hash(args: &[String]) -> Result<(), String> {
    let path = positional(args).ok_or("hash needs a file")?;
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    println!("sha256: {}", sha256_hex(&bytes));
    println!("sha512: {}", sha512_hex(&bytes));
    Ok(())
}

fn keygen(args: &[String]) -> Result<(), String> {
    let mut rng = rand_core::OsRng;
    let key = SigningKey::generate(&mut rng);
    let private = BASE64.encode(key.to_bytes());
    let public = encode_verifying_key(&key.verifying_key());
    let key_id = key_id(&key.verifying_key());
    match option(args, "--output") {
        Some(path) => {
            let contents = format!("private={private}\npublic={public}\nkeyId={key_id}\n");
            std::fs::write(&path, contents).map_err(|error| error.to_string())?;
            println!("wrote {path}");
        }
        None => {
            println!("private: {private}");
            println!("public:  {public}");
            println!("keyId:   {key_id}");
        }
    }
    Ok(())
}

/// Reads a signing key from its base64 seed.
fn signing_key(seed: &str) -> Result<SigningKey, String> {
    let seed = BASE64
        .decode(seed.trim())
        .map_err(|_| "the signing key is not base64".to_string())?;
    let seed: [u8; 32] = seed
        .try_into()
        .map_err(|_| "the signing key must be a 32-byte seed".to_string())?;
    Ok(SigningKey::from_bytes(&seed))
}

fn sign(args: &[String]) -> Result<(), String> {
    let path = positional(args).ok_or("sign needs a package file")?;
    let key = option(args, "--key").ok_or("sign needs --key <seed-base64>")?;
    let signing_key = signing_key(&key)?;
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    let signature = vibex_plugin::market::sign_package(&bytes, &signing_key);
    let output = option(args, "--output").unwrap_or_else(|| format!("{path}.sig"));
    std::fs::write(&output, &signature).map_err(|error| error.to_string())?;
    println!("signature: {output}");
    println!("alg:       ed25519");
    println!("keyId:     {}", key_id(&signing_key.verifying_key()));
    println!("sha256:    {}", sha256_hex(&bytes));
    Ok(())
}

/// Builds, signs and describes one submission.
///
/// The JSON printed at the end is the entry to submit: it names the package,
/// its hashes, the key that signed it and the permissions it asks for. The
/// index maintainer reviews it and rebuilds the catalog; this tool never edits
/// the catalog itself.
fn publish(args: &[String]) -> Result<(), String> {
    let directory = positional(args).ok_or("publish needs a plugin directory")?;
    let key = option(args, "--key").ok_or("publish needs --key <seed-base64>")?;
    let signing_key = signing_key(&key)?;
    let output = option(args, "--output").map(PathBuf::from);
    let path = write_package(std::path::Path::new(&directory), output.as_deref())
        .map_err(|error| error.to_string())?;
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    let manifest = read_package_manifest(&bytes).map_err(|error| error.to_string())?;
    let signature = vibex_plugin::market::sign_package(&bytes, &signing_key);
    let signature_path = path.with_extension("vplug.sig");
    std::fs::write(&signature_path, &signature).map_err(|error| error.to_string())?;

    let entry = serde_json::json!({
        "id": manifest.id,
        "version": manifest.version,
        "name": manifest.name,
        "license": manifest.license,
        "summary": manifest.description,
        "homepage": manifest.homepage,
        "categories": manifest.normalized_categories(),
        "versions": [{
            "version": manifest.version,
            "url": path.file_name().map(|name| name.to_string_lossy().to_string()),
            "sizeBytes": bytes.len(),
            "sha256": sha256_hex(&bytes),
            "sha512": sha512_hex(&bytes),
            "minShell": manifest.shell_version,
            "signature": {
                "alg": "ed25519",
                "keyId": key_id(&signing_key.verifying_key()),
                "signature": signature,
            },
        }],
    });
    println!("package:   {}", path.display());
    println!("signature: {}", signature_path.display());
    println!("sha256:    {}", sha256_hex(&bytes));
    println!();
    println!("{}", serde_json::to_string_pretty(&entry).unwrap());
    Ok(())
}

/// The maintainer's side: validate or build the catalog repository's index.
fn index_command(args: &[String]) -> Result<(), String> {
    match args.first().map(String::as_str) {
        Some("build") => index_build(&args[1..]),
        Some("validate") => index_validate(&args[1..]),
        _ => Err("index needs `build` or `validate`".to_string()),
    }
}

fn index_validate(args: &[String]) -> Result<(), String> {
    let catalog = option(args, "--catalog").ok_or("index validate needs --catalog <dir>")?;
    let entries = load_catalog_entries(Path::new(&catalog)).map_err(|error| error.to_string())?;
    println!("catalog: {}", Path::new(&catalog).display());
    println!("entries: {}", entries.len());
    for entry in &entries {
        println!(
            "  {} {} ({} version{})",
            entry.entry.id,
            entry.entry.latest_version.as_deref().unwrap_or("-"),
            entry.versions.len(),
            if entry.versions.len() == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

fn index_build(args: &[String]) -> Result<(), String> {
    let catalog = option(args, "--catalog").ok_or("index build needs --catalog <dir>")?;
    let key = option(args, "--key").ok_or("index build needs --key <seed-base64>")?;
    let signing_key = signing_key(&key)?;
    let revision = match option(args, "--revision") {
        Some(value) => value
            .parse::<u64>()
            .map_err(|_| "--revision must be a non-negative integer".to_string())?,
        None => 1,
    };
    let generated_at = option(args, "--generated-at").unwrap_or_else(utc_now_rfc3339);
    let publisher_keys: Vec<PublisherKey> = match option(args, "--publisher-keys") {
        Some(path) => {
            let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
            serde_json::from_slice(&bytes)
                .map_err(|error| format!("{path} is not a publisher key list: {error}"))?
        }
        None => Vec::new(),
    };
    let entries = load_catalog_entries(Path::new(&catalog)).map_err(|error| error.to_string())?;
    let index = build_index_file(entries, revision, generated_at, publisher_keys, 0);
    let path = write_signed_index(Path::new(&catalog), &index, &signing_key)
        .map_err(|error| error.to_string())?;
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    println!("index:     {}", path.display());
    println!("revision:  {revision}");
    println!("keyId:     {}", key_id(&signing_key.verifying_key()));
    println!("sha256:    {}", sha256_hex(&bytes));
    Ok(())
}

fn sign_index(args: &[String]) -> Result<(), String> {
    let path = positional(args).ok_or("sign-index needs an index file")?;
    let key = option(args, "--key").ok_or("sign-index needs --key <seed-base64>")?;
    let signing_key = signing_key(&key)?;
    let bytes = std::fs::read(&path).map_err(|error| error.to_string())?;
    let signature = sign_document(&bytes, &signing_key);
    let output = option(args, "--output").unwrap_or_else(|| format!("{path}.sig"));
    std::fs::write(&output, signature.trim()).map_err(|error| error.to_string())?;
    println!("signature: {output}");
    println!("keyId:     {}", key_id(&signing_key.verifying_key()));
    println!("sha256:    {}", sha256_hex(&bytes));
    Ok(())
}

/// The first eight bytes of the key's SHA-256, as hex.
fn key_id(key: &VerifyingKey) -> String {
    let digest = Sha256::digest(key.to_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
