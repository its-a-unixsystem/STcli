use anyhow::{Context, Result};

pub fn copy(content: &str) -> Result<()> {
    let mut clipboard = arboard::Clipboard::new().context("system clipboard is unavailable")?;
    clipboard
        .set_text(content.to_owned())
        .context("failed to write to the system clipboard")
}
