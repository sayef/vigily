// The Copy page button: copies the page's Markdown and says so for a moment.
document.addEventListener("click", async (event) => {
  const button = event.target.closest("[data-vg-copy-page]");
  if (!button) return;
  const text = button.parentElement.querySelector(".vg-source")?.value ?? "";
  try {
    await navigator.clipboard.writeText(text);
  } catch {
    return;
  }
  const label = button.querySelector("span:last-child");
  const before = label.textContent;
  label.textContent = "Copied";
  setTimeout(() => (label.textContent = before), 1400);
});
