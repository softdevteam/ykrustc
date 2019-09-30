//! Check for external package sources. Allow only vendorable packages.

use std::fs;
use std::path::Path;

/// List of whitelisted sources for packages.
const WHITELISTED_SOURCES: &[&str] = &[
    "\"registry+https://github.com/rust-lang/crates.io-index\"",
    // The following are needed for Yorick whilst we use an unreleased revision not on crates.io.
    "\"git+https://github.com/3Hren/msgpack-rust?\
        rev=40b3d480b20961e6eeceb416b32bcd0a3383846a#40b3d480b20961e6eeceb416b32bcd0a3383846a\"",
];

/// Checks for external package sources.
pub fn check(path: &Path, bad: &mut bool) {
    // `Cargo.lock` of rust (tidy runs inside `src/`).
    let path = path.join("../Cargo.lock");

    // Open and read the whole file.
    let cargo_lock = t!(fs::read_to_string(&path));

    // Process each line.
    for line in cargo_lock.lines() {
        // Consider only source entries.
        if ! line.starts_with("source = ") {
            continue;
        }

        // Extract source value.
        let source = line.splitn(2, '=').nth(1).unwrap().trim();

        // Allow all soft-dev repos.
        // We also allow our personal forks for scenarios where we are breaking a CI cycle and need
        // to temporarily use one of our personal feature branches.
        if source.starts_with("\"git+https://github.com/softdevteam/") ||
            source.starts_with("\"git+https://github.com/vext01/") ||
            source.starts_with("\"git+https://github.com/ltratt/") ||
            source.starts_with("\"git+https://github.com/ptersilie/")
        {
            continue;
        }

        // Ensure source is whitelisted.
        if !WHITELISTED_SOURCES.contains(&&*source) {
            println!("invalid source: {}", source);
            *bad = true;
        }
    }
}
