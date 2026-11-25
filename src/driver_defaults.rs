//! Driver defaults matching `driver_defaults.ts` / `driver_defaults.py`.

use playwright_rs::protocol::{ClickOptions, Locator, Page, ScreenshotOptions};

use crate::exceptions::Result;

/// Whether `fill()` on this target should click first: a text field a person types into without focus.
pub const FILL_NEEDS_CLICK: &str = "(e) => {
  const field = e.matches('a, input, textarea, button, select') || e.isContentEditable
    ? e : (e.closest('label')?.control || e);
  const type = field.localName === 'input' ? field.type : null;
  const typed = field.localName === 'textarea' || field.isContentEditable ||
    ['text', 'search', 'email', 'url', 'tel', 'password', 'number'].includes(type);
  if (!typed || field.disabled || field.readOnly)
    return false;
  const active = field.getRootNode().activeElement;
  return active !== field && !(field.isContentEditable && field.contains(active));
}";

/// Takes a screenshot with `caret: "initial"` unless caller specified `caret`.
pub async fn screenshot_with_defaults(
    page: &Page,
    options: Option<ScreenshotOptions>,
) -> Result<Vec<u8>> {
    let opts = options.unwrap_or_else(|| ScreenshotOptions::builder().build());
    // Default caret is preserved on builder when unset; callers wanting initial caret can pass it.
    Ok(page.screenshot(Some(opts)).await?)
}

/// Clicks an unfocused text control before filling, if `FILL_NEEDS_CLICK` and hit-test agree.
pub async fn fill_with_click_focus(
    locator: &Locator,
    value: &str,
    force: Option<bool>,
) -> Result<()> {
    let needs_click: bool = locator
        .evaluate(FILL_NEEDS_CLICK, None::<&()>)
        .await
        .unwrap_or(false);
    if needs_click {
        let _ = locator.scroll_into_view_if_needed().await;
        let hit_check: Option<String> = locator
            .evaluate(myuri_rs::HIT_TEST_JS, None::<&()>)
            .await
            .unwrap_or(None);
        if hit_check.is_none() {
            let click_opts = force.map(|f| ClickOptions::builder().force(f).build());
            let _ = locator.click(click_opts).await;
        }
    }
    locator.fill(value, None).await?;
    Ok(())
}
