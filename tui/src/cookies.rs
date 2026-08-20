//! Reads a YouTube session out of the user's real browser, because Google refuses sign-in from an embedded webview and nothing crosses between two cookie jars on its own; Chromium keeps its values encrypted under a login-keychain key, so this costs one keychain prompt.

use std::path::PathBuf;
use std::process::Command;

use aes::cipher::block_padding::Pkcs7;
use aes::cipher::{BlockDecryptMut, KeyIvInit};
use sha2::{Digest, Sha256};

type Aes128CbcDec = cbc::Decryptor<aes::Aes128>;

/// Chromium's own key-derivation parameters; changing any of them just produces garbage.
const SALT: &[u8] = b"saltysalt";
const ITERATIONS: u32 = 1003;
const KEY_LENGTH: usize = 16;
/// Chromium encrypts with a fixed IV of sixteen spaces.
const IV: [u8; 16] = [0x20; 16];
/// Marks a value encrypted with the keychain key, as opposed to stored plainly.
const ENCRYPTED_PREFIX: &[u8] = b"v10";

/// The cookies that carry a YouTube session, as a fixed list rather than everything on the domain: the ad and experiment cookies beside them have no bearing on being signed in.
const WANTED: &[&str] = &[
    "SID",
    "HSID",
    "SSID",
    "APISID",
    "SAPISID",
    "LOGIN_INFO",
    "PREF",
    "SIDCC",
    "VISITOR_INFO1_LIVE",
    "__Secure-1PSID",
    "__Secure-3PSID",
    "__Secure-1PAPISID",
    "__Secure-3PAPISID",
    "__Secure-1PSIDTS",
    "__Secure-3PSIDTS",
    "__Secure-1PSIDCC",
    "__Secure-3PSIDCC",
];

/// The names that prove an account rather than the anonymous cookies YouTube hands every visitor; without one, the user has not signed in yet.
const PROOF_OF_LOGIN: &[&str] = &["SAPISID", "__Secure-3PAPISID", "__Secure-1PAPISID"];

/// Only youtube.com cookies are usable, since a page on music.youtube.com cannot set one for google.com however much it would like to.
const DOMAIN: &str = ".youtube.com";

/// A Chromium-family browser, described by its cookie database and the keychain entry that unlocks it.
struct Browser {
    name: &'static str,
    support_dir: &'static str,
    keychain_service: &'static str,
    keychain_account: &'static str,
}

const BROWSERS: &[Browser] = &[
    Browser {
        name: "Brave",
        support_dir: "BraveSoftware/Brave-Browser",
        keychain_service: "Brave Safe Storage",
        keychain_account: "Brave",
    },
    Browser {
        name: "Chrome",
        support_dir: "Google/Chrome",
        keychain_service: "Chrome Safe Storage",
        keychain_account: "Chrome",
    },
    Browser {
        name: "Edge",
        support_dir: "Microsoft Edge",
        keychain_service: "Microsoft Edge Safe Storage",
        keychain_account: "Microsoft Edge",
    },
    Browser {
        name: "Arc",
        support_dir: "Arc/User Data",
        keychain_service: "Arc Safe Storage",
        keychain_account: "Arc",
    },
    Browser {
        name: "Vivaldi",
        support_dir: "Vivaldi",
        keychain_service: "Vivaldi Safe Storage",
        keychain_account: "Vivaldi",
    },
    Browser {
        name: "Chromium",
        support_dir: "Chromium",
        keychain_service: "Chromium Safe Storage",
        keychain_account: "Chromium",
    },
];

/// Profile directories to search, in the order a single-profile user would expect; a session in "Profile 2" is still a session.
const PROFILES: &[&str] = &["Default", "Profile 1", "Profile 2", "Profile 3"];

pub struct Session {
    /// Which browser it came from, so the client can say so.
    pub browser: &'static str,
    pub cookies: Vec<(String, String)>,
}

/// Finds a signed-in YouTube session in whichever supported browser has one, distinguishing "no browser to read" from "found, but not signed in" because only the second is worth acting on.
pub fn find_session() -> Result<Session, String> {
    let mut looked_in = Vec::new();

    for browser in BROWSERS {
        for profile in PROFILES {
            let Some(database) = database_path(browser, profile) else {
                continue;
            };
            if !database.is_file() {
                continue;
            }
            looked_in.push(format!("{} ({profile})", browser.name));

            let cookies = match read_cookies(browser, &database) {
                Ok(cookies) => cookies,
                // An unreadable profile is no reason to stop; the next one may be the signed-in one.
                Err(_) => continue,
            };
            if PROOF_OF_LOGIN
                .iter()
                .any(|name| cookies.iter().any(|(cookie, _)| cookie == name))
            {
                return Ok(Session { browser: browser.name, cookies });
            }
        }
    }

    Err(match looked_in.is_empty() {
        true => "no supported browser found (Brave, Chrome, Edge, Arc, Vivaldi or Chromium)".into(),
        false => format!(
            "no signed-in YouTube session in {}",
            looked_in.join(", ")
        ),
    })
}

fn database_path(browser: &Browser, profile: &str) -> Option<PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(
        PathBuf::from(home)
            .join("Library/Application Support")
            .join(browser.support_dir)
            .join(profile)
            .join("Cookies"),
    )
}

fn read_cookies(browser: &Browser, database: &PathBuf) -> Result<Vec<(String, String)>, String> {
    let key = decryption_key(browser)?;
    let rows = query(database)?;

    let mut cookies = Vec::new();
    for (name, encrypted) in rows {
        if !WANTED.contains(&name.as_str()) {
            continue;
        }
        if let Ok(value) = decrypt(&encrypted, &key) {
            cookies.push((name, value));
        }
    }
    Ok(cookies)
}

/// Derives the AES key from the browser's keychain secret, which is the step that makes macOS ask permission and the reason sign-in is a deliberate keypress.
fn decryption_key(browser: &Browser) -> Result<[u8; KEY_LENGTH], String> {
    let output = Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-w",
            "-s",
            browser.keychain_service,
            "-a",
            browser.keychain_account,
        ])
        .output()
        .map_err(|error| format!("cannot run security(1): {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "keychain refused the {} entry (denied, or the browser has never run)",
            browser.keychain_service
        ));
    }
    let secret = String::from_utf8(output.stdout)
        .map_err(|_| "keychain secret is not text".to_string())?;

    let mut key = [0_u8; KEY_LENGTH];
    pbkdf2::pbkdf2_hmac::<sha1::Sha1>(secret.trim_end().as_bytes(), SALT, ITERATIONS, &mut key);
    Ok(key)
}

/// Reads the rows through sqlite3(1) rather than linking a SQL engine, from a copy: a running browser holds the file open, so reading in place gets a lock error or an answer predating the sign-in.
fn query(database: &PathBuf) -> Result<Vec<(String, Vec<u8>)>, String> {
    let scratch = std::env::temp_dir().join(format!("xmusic-cookies-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)
        .map_err(|error| format!("cannot create {}: {error}", scratch.display()))?;
    let copy = Copy(scratch);
    let target = copy.0.join("Cookies");
    std::fs::copy(database, &target)
        .map_err(|error| format!("cannot copy the cookie database: {error}"))?;

    // Hex keeps binary ciphertext intact through a text pipe.
    let output = Command::new("/usr/bin/sqlite3")
        .arg("-readonly")
        .arg("-newline")
        .arg("\n")
        .arg(&target)
        .arg(format!(
            "select name || ' ' || hex(encrypted_value) from cookies where host_key = '{DOMAIN}';"
        ))
        .output()
        .map_err(|error| format!("cannot run sqlite3(1): {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "sqlite3 could not read the cookie database: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }

    let text = String::from_utf8_lossy(&output.stdout);
    Ok(text
        .lines()
        .filter_map(|line| line.split_once(' '))
        .filter_map(|(name, hex)| Some((name.to_string(), from_hex(hex)?)))
        .collect())
}

/// Removes the scratch copy of the cookie database however this function exits.
struct Copy(PathBuf);

impl Drop for Copy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn from_hex(hex: &str) -> Option<Vec<u8>> {
    let bytes = hex.as_bytes();
    if bytes.len() % 2 != 0 {
        return None;
    }
    bytes
        .chunks(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).ok()?, 16).ok())
        .collect()
}

fn decrypt(encrypted: &[u8], key: &[u8; KEY_LENGTH]) -> Result<String, String> {
    let Some(ciphertext) = encrypted.strip_prefix(ENCRYPTED_PREFIX) else {
        // Written before the browser had a keychain key; already plain text.
        return String::from_utf8(encrypted.to_vec()).map_err(|_| "value is not text".into());
    };

    let plain = Aes128CbcDec::new(key.into(), &IV.into())
        .decrypt_padded_vec_mut::<Pkcs7>(ciphertext)
        .map_err(|_| "wrong key, or the value is not AES-CBC".to_string())?;

    // Chromium 130 and later prepend a SHA-256 of the cookie's own domain so a stolen value cannot be replayed elsewhere; older versions do not, so recognise it rather than assume.
    let digest = Sha256::digest(DOMAIN.as_bytes());
    let body = match plain.len() > digest.len() && plain[..digest.len()] == digest[..] {
        true => &plain[digest.len()..],
        false => &plain[..],
    };

    String::from_utf8(body.to_vec()).map_err(|_| "decrypted value is not text".into())
}

/// Hands a URL to whatever the user has set as their browser.
pub fn open_in_browser(url: &str) -> Result<(), String> {
    Command::new("/usr/bin/open")
        .arg(url)
        .status()
        .map_err(|error| format!("cannot run open(1): {error}"))?
        .success()
        .then_some(())
        .ok_or_else(|| format!("open(1) refused {url}"))
}
