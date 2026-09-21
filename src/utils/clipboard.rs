use anyhow::{Context, Result};

/// Write plain text to the system clipboard for UI and plugin actions.
pub(crate) fn set_text(text: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new().context("open system clipboard")?;
    clipboard
        .set_text(text.to_owned())
        .context("write system clipboard")
}
