use std::{env, fs, io::Read as _, path::PathBuf, process::ExitCode};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use ed25519_dalek::{Signer as _, SigningKey};
use semver::Version;
use sha2::{Digest as _, Sha256, Sha512};
use url::Url;
use vibex_app_update::{
    InstallMode, UpdateArtifact, UpdateChannel, UpdateManifest, verify_manifest,
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("vibex-update-manifest: {message}");
            ExitCode::FAILURE
        }
    }
}

struct Arguments {
    channel: UpdateChannel,
    version: Version,
    published_at: String,
    output: PathBuf,
    signature_output: PathBuf,
    artifacts: Vec<ArtifactArgument>,
}

struct ArtifactArgument {
    os: String,
    arch: String,
    package: String,
    install_mode: InstallMode,
    path: PathBuf,
    url: Url,
}

fn run() -> Result<(), String> {
    let arguments = parse_arguments(env::args().skip(1).collect())?;
    let signing_key = signing_key_from_environment()?;
    let tag = format!("v{}", arguments.version);
    let artifacts = arguments
        .artifacts
        .into_iter()
        .map(|artifact| build_artifact(artifact, &tag))
        .collect::<Result<Vec<_>, _>>()?;
    let manifest = UpdateManifest {
        schema: 1,
        channel: arguments.channel,
        version: arguments.version,
        tag: tag.clone(),
        published_at: arguments.published_at,
        minimum_updater_version: "1".to_string(),
        notes_url: Url::parse(&format!(
            "https://github.com/vibex-ai/vibex/releases/tag/{tag}"
        ))
        .expect("release notes URL is valid"),
        artifacts,
    };
    let mut raw = serde_json::to_vec_pretty(&manifest)
        .map_err(|_| "manifest JSON could not be encoded".to_string())?;
    raw.push(b'\n');
    let signature = signing_key.sign(&raw);
    let signature_base64 = BASE64_STANDARD.encode(signature.to_bytes());
    let public_key_base64 = BASE64_STANDARD.encode(signing_key.verifying_key().to_bytes());
    verify_manifest(
        &raw,
        &signature_base64,
        &public_key_base64,
        arguments.channel,
        &tag,
    )
    .map_err(|error| format!("generated manifest failed validation: {}", error.code))?;
    write_atomic(&arguments.output, &raw)?;
    write_atomic(
        &arguments.signature_output,
        format!("{signature_base64}\n").as_bytes(),
    )?;
    println!("update_public_key={public_key_base64}");
    Ok(())
}

fn parse_arguments(values: Vec<String>) -> Result<Arguments, String> {
    let mut channel = None;
    let mut version = None;
    let mut published_at = None;
    let mut output = None;
    let mut signature_output = None;
    let mut artifacts = Vec::new();
    let mut values = values.into_iter();
    while let Some(flag) = values.next() {
        let value = values
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        match flag.as_str() {
            "--channel" => channel = Some(parse_channel(&value)?),
            "--version" => {
                version = Some(
                    Version::parse(&value)
                        .map_err(|_| "--version is not valid SemVer".to_string())?,
                )
            }
            "--published-at" => published_at = Some(value),
            "--output" => output = Some(PathBuf::from(value)),
            "--signature-output" => signature_output = Some(PathBuf::from(value)),
            "--artifact" => artifacts.push(parse_artifact(&value)?),
            _ => return Err(format!("unknown argument {flag}")),
        }
    }
    let channel = channel.ok_or_else(|| "--channel is required".to_string())?;
    let version = version.ok_or_else(|| "--version is required".to_string())?;
    if !channel.accepts(&version) {
        return Err("--version does not belong to --channel".to_string());
    }
    if artifacts.is_empty() {
        return Err("at least one --artifact is required".to_string());
    }
    Ok(Arguments {
        channel,
        version,
        published_at: published_at.ok_or_else(|| "--published-at is required".to_string())?,
        output: output.ok_or_else(|| "--output is required".to_string())?,
        signature_output: signature_output
            .ok_or_else(|| "--signature-output is required".to_string())?,
        artifacts,
    })
}

fn parse_channel(value: &str) -> Result<UpdateChannel, String> {
    match value {
        "stable" => Ok(UpdateChannel::Stable),
        "rc" => Ok(UpdateChannel::Rc),
        "preview" => Ok(UpdateChannel::Preview),
        _ => Err("--channel must be stable, rc, or preview".to_string()),
    }
}

fn parse_artifact(value: &str) -> Result<ArtifactArgument, String> {
    let parts = value.split('|').collect::<Vec<_>>();
    let [os, arch, package, install_mode, path, url] = parts.as_slice() else {
        return Err(
            "--artifact must be os|arch|package|install_mode|path|download_url".to_string(),
        );
    };
    let install_mode = match *install_mode {
        "self_replace" => InstallMode::SelfReplace,
        "system_installer" => InstallMode::SystemInstaller,
        "store" => InstallMode::Store,
        "external" => InstallMode::External,
        _ => return Err("artifact install_mode is invalid".to_string()),
    };
    Ok(ArtifactArgument {
        os: (*os).to_string(),
        arch: (*arch).to_string(),
        package: (*package).to_string(),
        install_mode,
        path: PathBuf::from(path),
        url: Url::parse(url).map_err(|_| "artifact download_url is invalid".to_string())?,
    })
}

fn build_artifact(argument: ArtifactArgument, tag: &str) -> Result<UpdateArtifact, String> {
    let mut file = fs::File::open(&argument.path)
        .map_err(|_| format!("artifact could not be read: {}", argument.path.display()))?;
    let size = file
        .metadata()
        .map_err(|_| {
            format!(
                "artifact metadata could not be read: {}",
                argument.path.display()
            )
        })?
        .len();
    if size == 0 {
        return Err(format!("artifact is empty: {}", argument.path.display()));
    }
    if !argument
        .url
        .path()
        .starts_with(&format!("/vibex-ai/vibex/releases/download/{tag}/"))
    {
        return Err("artifact URL tag does not match the manifest version".to_string());
    }
    let mut sha256 = Sha256::new();
    let mut sha512 = Sha512::new();
    let mut buffer = vec![0u8; 1024 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| format!("artifact could not be hashed: {}", argument.path.display()))?;
        if count == 0 {
            break;
        }
        sha256.update(&buffer[..count]);
        sha512.update(&buffer[..count]);
    }
    Ok(UpdateArtifact {
        os: argument.os,
        arch: argument.arch,
        package: argument.package,
        install_mode: argument.install_mode,
        url: argument.url,
        size,
        sha256: hex_lower(&sha256.finalize()),
        sha512: Some(hex_lower(&sha512.finalize())),
    })
}

fn signing_key_from_environment() -> Result<SigningKey, String> {
    let encoded = env::var("VIBEX_UPDATE_SIGNING_KEY")
        .map_err(|_| "VIBEX_UPDATE_SIGNING_KEY is required".to_string())?;
    let bytes = BASE64_STANDARD
        .decode(encoded.trim())
        .map_err(|_| "VIBEX_UPDATE_SIGNING_KEY is not base64".to_string())?;
    let bytes: [u8; 32] = bytes
        .try_into()
        .map_err(|_| "VIBEX_UPDATE_SIGNING_KEY must contain exactly 32 bytes".to_string())?;
    Ok(SigningKey::from_bytes(&bytes))
}

fn write_atomic(path: &PathBuf, bytes: &[u8]) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("output has no parent: {}", path.display()))?;
    fs::create_dir_all(parent).map_err(|_| {
        format!(
            "output directory could not be created: {}",
            parent.display()
        )
    })?;
    let temporary = path.with_extension(format!("tmp-{}", std::process::id()));
    fs::write(&temporary, bytes).map_err(|_| {
        format!(
            "temporary output could not be written: {}",
            temporary.display()
        )
    })?;
    fs::rename(&temporary, path)
        .map_err(|_| format!("output could not be committed: {}", path.display()))
}

fn hex_lower(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}
