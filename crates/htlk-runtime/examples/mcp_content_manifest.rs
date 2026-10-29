//! Prints the exact shipped content-library manifest for explicit host/source linking.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let limits = htlk_executable::cbor::Limits::default();
    let manifest = htlk_runtime::mcp_content_library(&limits)?;
    let document =
        htlk_executable::JsonDocument::from_value(&manifest.to_value(&limits)?, &limits)?;
    println!("{}", std::str::from_utf8(document.as_bytes())?);
    Ok(())
}
