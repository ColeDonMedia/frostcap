/**
 * Tests assert English text; pin the reported language so a developer machine set
 * to another language (Node reports the OS locale as navigator.language) cannot change results.
 */
if (typeof navigator !== "undefined") {
  Object.defineProperty(navigator, "language", { value: "en-US", configurable: true });
  Object.defineProperty(navigator, "languages", { value: ["en-US", "en"], configurable: true });
}
