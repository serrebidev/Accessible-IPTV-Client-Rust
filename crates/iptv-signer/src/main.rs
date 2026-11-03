//! Sign release assets: `iptv-signer <file>...` writes
//! `<file>.minisig` beside each file, using the minisign secret
//! key from the MINISIGN_KEY environment variable. The key is
//! the same one BlindRadio uses; it lives in the repo's Actions
//! secrets and the owner's vault, never in git. Mirrors
//! BlindRadio's xtask sign_file.

use std::path::Path;

fn sign_file(path: &Path) -> Result<(), String> {
    let key = std::env::var("MINISIGN_KEY").map_err(|_| "MINISIGN_KEY is not set".to_string())?;
    let password = std::env::var("MINISIGN_PASSWORD")
        .ok()
        .filter(|value| !value.is_empty());
    let boxed =
        minisign::SecretKeyBox::from_string(&key).map_err(|e| format!("MINISIGN_KEY: {e}"))?;
    let key = match password {
        Some(password) => boxed.into_secret_key(Some(password)),
        None => boxed.into_unencrypted_secret_key(),
    }
    .map_err(|e| format!("MINISIGN_KEY: {e}"))?;
    let name = path
        .file_name()
        .ok_or_else(|| format!("{} has no file name", path.display()))?
        .to_string_lossy();
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let trusted = format!("timestamp:{timestamp}\tfile:{name}\thashed");
    let reader = std::io::BufReader::new(
        std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?,
    );
    let signature = minisign::sign(None, &key, reader, Some(&trusted), None)
        .map_err(|e| format!("signing {}: {e}", path.display()))?;
    let mut out = path.as_os_str().to_owned();
    out.push(".minisig");
    std::fs::write(&out, signature.into_string())
        .map_err(|e| format!("{}: {e}", Path::new(&out).display()))?;
    println!("signed {}", path.display());
    Ok(())
}

fn main() {
    let files: Vec<String> = std::env::args().skip(1).collect();
    if files.is_empty() {
        eprintln!("usage: iptv-signer <file>...");
        std::process::exit(2);
    }
    for file in &files {
        if let Err(error) = sign_file(Path::new(file)) {
            eprintln!("{error}");
            std::process::exit(1);
        }
    }
}
