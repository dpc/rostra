// Run in post-image.html via agent-browser eval --stdin at wide/narrow viewports
// and light/dark media settings. This fixture has no credentials or remote media.
(async () => {
  const assert = (condition, message) => {
    if (!condition) throw new Error(message);
  };
  const dialog = document.getElementById("post-image-dialog");
  if (dialog.open) closeRostraDialog(dialog);
  const image = dialog.querySelector("img");
  const close = dialog.querySelector("button");
  const results = [];
  for (const link of document.querySelectorAll("body > a.m-rostraMedia__image")) {
    link.focus();
    link.click();
    await image.decode();
    assert(dialog.open, "image opens the modal");
    assert(document.activeElement === close, "Close receives native dialog focus");
    assert(close.classList.contains("u-button"), "Close uses shared button styling");
    const icon = close.querySelector(".u-buttonIcon");
    assert(getComputedStyle(icon).backgroundImage.includes("xmark.svg"), "Close has its semantic icon");
    const bounds = dialog.getBoundingClientRect();
    const media = image.getBoundingClientRect();
    const control = close.getBoundingClientRect();
    assert(Math.abs(bounds.width - (innerWidth - 16)) < 1, "dialog uses viewport width");
    assert(Math.abs(bounds.height - (innerHeight - 16)) < 1, "dialog uses viewport height");
    assert(bounds.x >= 0 && bounds.y >= 0 && bounds.right <= innerWidth && bounds.bottom <= innerHeight,
      "dialog stays inside viewport");
    assert(control.top >= bounds.top && control.right <= bounds.right, "Close remains visible");
    assert(media.top >= control.bottom && media.bottom <= bounds.bottom, "image leaves room for Close");
    assert(Math.abs(media.width - (bounds.width - 18)) < 1, "image uses available inner width");
    assert(Math.abs(media.bottom - (bounds.bottom - 9)) < 1, "image uses remaining row height");
    assert(getComputedStyle(image).objectFit === "contain", "whole image retains aspect ratio");
    const scale = Math.min(media.width / image.naturalWidth, media.height / image.naturalHeight);
    if (image.naturalWidth <= 160) assert(scale > 1, "small image scales up");
    const painted = [image.naturalWidth * scale, image.naturalHeight * scale];
    assert(Math.abs(painted[0] - media.width) < 1 || Math.abs(painted[1] - media.height) < 1,
      "painted image fills at least one available dimension");
    assert(image.alt === link.querySelector("img").alt, "alt text is preserved");
    results.push({
      alt: image.alt, viewport: [innerWidth, innerHeight],
      natural: [image.naturalWidth, image.naturalHeight], scale,
      painted, dialog: [bounds.width, bounds.height], imageBox: [media.width, media.height],
    });
    close.click();
    assert(!dialog.open && document.activeElement === link, "Close returns focus");
    assert(!image.hasAttribute("src"), "closed viewer clears its source");
  }
  return results;
})()
