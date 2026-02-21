import { Picker, Database } from "/assets/libs/emoji-picker-element/index.js";
import textFieldEdit from "/assets/libs/text-field-edit/index.js";

// Make Database available globally for the Alpine component
window.EmojiDatabase = Database;

// Handle emoji picker clicks - find the correct textarea from the picker's container
document.addEventListener("emoji-click", (e) => {
  const picker = e.target;
  const container = picker.closest("[data-textarea-selector]");
  let textarea;
  if (container) {
    // Inline reply picker - use the data attribute to find the textarea
    textarea = document.querySelector(container.dataset.textareaSelector);
  } else {
    // Main form picker - use default selector
    textarea = document.getElementById("new-post-content");
  }
  if (textarea) {
    textFieldEdit.insert(textarea, e.detail.unicode);
  }
});
