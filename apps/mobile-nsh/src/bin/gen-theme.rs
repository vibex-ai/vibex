//! Regenerates `theme.json` from the shared design token source.
//!
//! ```sh
//! cargo run -p vibex-mobile-nsh --bin gen-theme
//! ```

use std::{fs, path::Path, process::ExitCode};

use vibex_mobile_nsh::theme_gen::{self, THEME_FILE, TOKEN_SOURCE};

fn main() -> ExitCode {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = manifest.join(TOKEN_SOURCE);
    let target = manifest.join(THEME_FILE);

    let tokens = match fs::read_to_string(&source) {
        Ok(tokens) => tokens,
        Err(error) => {
            eprintln!("read {}: {error}", source.display());
            return ExitCode::FAILURE;
        }
    };
    let theme = match theme_gen::generate(&tokens) {
        Ok(theme) => theme,
        Err(error) => {
            eprintln!("generate {}: {error}", source.display());
            return ExitCode::FAILURE;
        }
    };
    if let Err(error) = fs::write(&target, theme) {
        eprintln!("write {}: {error}", target.display());
        return ExitCode::FAILURE;
    }
    println!("wrote {}", target.display());
    ExitCode::SUCCESS
}
